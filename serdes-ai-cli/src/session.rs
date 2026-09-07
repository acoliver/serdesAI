use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::bus::AnyMessage;
use crate::config::{self, CompactionStrategy};
use crate::messages::{AgentResponseMessage, BaseMessage, MessageCategory, TextMessage};

pub type Message = AnyMessage;

/// Compaction result with statistics
#[derive(Debug, Clone)]
pub struct CompactionResult {
    pub before_count: usize,
    pub after_count: usize,
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub strategy: CompactionStrategy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ContextDump {
    name: String,
    timestamp: DateTime<Utc>,
    message_count: usize,
    messages: Vec<AnyMessage>,
}

/// Auto-rotate session when message count reaches this threshold.
///
/// Keep it intentionally conservative to avoid giant session blobs and keep restore UX snappy.
pub const DEFAULT_ROTATE_AFTER_MESSAGES: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub message_count: usize,
    pub total_tokens: usize,
    pub file_size: u64,
}

/// Generate a new UUID-based session id.
#[must_use]
pub fn generate_session_id() -> String {
    Uuid::new_v4().to_string()
}

/// Determine if a session should rotate due to message count threshold.
#[must_use]
pub fn should_rotate_session(session: &Session, rotate_after_messages: usize) -> bool {
    if rotate_after_messages == 0 {
        return false;
    }
    session.messages.len() >= rotate_after_messages
}

/// Convenience helper for `/clear` command flow or explicit new-session requests.
#[must_use]
pub fn rotate_session_now() -> String {
    finalize_autosave_session()
}

pub fn save_session(session: &Session, base_dir: &Path) -> Result<PathBuf> {
    ensure_directory(base_dir)?;

    let target_path = session_file_path(base_dir, &session.id);
    let temp_path = target_path.with_extension("json.tmp");

    let payload = serde_json::to_vec_pretty(session)
        .with_context(|| format!("failed to serialize session '{}'", session.id))?;

    fs::write(&temp_path, payload).with_context(|| {
        format!(
            "failed to write temporary session file '{}'",
            temp_path.display()
        )
    })?;

    fs::rename(&temp_path, &target_path).with_context(|| {
        format!(
            "failed to atomically move session file '{}' to '{}'",
            temp_path.display(),
            target_path.display()
        )
    })?;

    Ok(target_path)
}

pub fn load_session(session_id: &str, base_dir: &Path) -> Result<Session> {
    let path = session_file_path(base_dir, session_id);
    if !path.exists() {
        anyhow::bail!("session '{}' not found at {}", session_id, path.display());
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read session file '{}'", path.display()))?;

    let session: Session = serde_json::from_str(&content).with_context(|| {
        format!(
            "failed to parse session JSON for '{}' from '{}'",
            session_id,
            path.display()
        )
    })?;

    Ok(session)
}

pub fn list_sessions(base_dir: &Path) -> Result<Vec<SessionInfo>> {
    if !base_dir.exists() {
        return Ok(Vec::new());
    }

    let mut infos = Vec::new();
    for entry in fs::read_dir(base_dir)
        .with_context(|| format!("failed to list autosave directory '{}'", base_dir.display()))?
    {
        let entry = entry.with_context(|| "failed to read directory entry")?;
        let path = entry.path();

        if !is_json_session_file(&path) {
            continue;
        }

        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to read metadata for '{}'", path.display()))?;
        let file_size = metadata.len();

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let session: Session = match serde_json::from_str(&content) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let id = session.id.clone();
        let message_count = session.messages.len();
        let total_tokens = estimate_total_tokens(&session.messages);

        infos.push(SessionInfo {
            id,
            created_at: session.created_at,
            updated_at: session.updated_at,
            message_count,
            total_tokens,
            file_size,
        });
    }

    infos.sort_by_key(|info| std::cmp::Reverse(info.updated_at));
    Ok(infos)
}

pub fn delete_session(session_id: &str, base_dir: &Path) -> Result<()> {
    let path = session_file_path(base_dir, session_id);
    if !path.exists() {
        anyhow::bail!("session '{}' not found at {}", session_id, path.display());
    }

    fs::remove_file(&path)
        .with_context(|| format!("failed to delete session '{}'", path.display()))?;
    Ok(())
}

pub fn autosave_current_session(session: &Session) -> Result<PathBuf> {
    let base_dir = config::get_autosave_dir();

    // Save first.
    let path = save_session(session, &base_dir)?;

    // Ensure current session id is tracked in config.
    set_current_session_id(session.id.clone());

    // Keep autosave directory bounded.
    let max_sessions = config::get_autosave_max_sessions();
    if max_sessions > 0 {
        let _ = cleanup_old_sessions(max_sessions, &base_dir)?;
    }

    Ok(path)
}

pub fn finalize_autosave_session() -> String {
    let new_id = generate_session_id();
    set_current_session_id(new_id.clone());
    new_id
}

pub fn cleanup_old_sessions(max_count: usize, base_dir: &Path) -> Result<usize> {
    if max_count == 0 || !base_dir.exists() {
        return Ok(0);
    }

    let mut infos = list_sessions(base_dir)?;
    if infos.len() <= max_count {
        return Ok(0);
    }

    // Keep newest N (list_sessions returns descending updated_at), delete the tail.
    infos.sort_by_key(|info| std::cmp::Reverse(info.updated_at));
    let stale = infos.into_iter().skip(max_count);

    let mut removed = 0usize;
    for item in stale {
        if delete_session(&item.id, base_dir).is_ok() {
            removed += 1;
        }
    }

    Ok(removed)
}

#[must_use]
pub fn get_current_session_id() -> Option<String> {
    config::get_current_session_id()
}

pub fn set_current_session_id(id: String) {
    config::set_current_session_id(id);
}

/// Compact message history using configured strategy
pub async fn compact_history(
    messages: &mut Vec<AnyMessage>,
    strategy: CompactionStrategy,
    protected_tokens: usize,
) -> Result<CompactionResult> {
    let before_count = messages.len();
    let before_tokens = estimate_total_tokens(messages);

    if messages.len() <= 2 {
        return Ok(CompactionResult {
            before_count,
            after_count: before_count,
            before_tokens,
            after_tokens: before_tokens,
            strategy,
        });
    }

    let mut compacted = messages.clone();
    match strategy {
        CompactionStrategy::Truncation => {
            compact_to_token_budget(&mut compacted, protected_tokens.max(1));
        }
        CompactionStrategy::Summarization => {
            compact_with_summary(&mut compacted, protected_tokens.max(1));
        }
    }

    let after_tokens = estimate_total_tokens(&compacted);
    let after_count = compacted.len();
    *messages = compacted;

    Ok(CompactionResult {
        before_count,
        after_count,
        before_tokens,
        after_tokens,
        strategy,
    })
}

/// Truncate history to N messages while preserving the first message.
///
/// Behavior:
/// - N must be >= 1
/// - If history is empty: no-op
/// - Keep message[0] (typically system prompt)
/// - Keep up to N-1 most recent messages from the remainder
pub fn truncate_history(messages: &mut Vec<AnyMessage>, n: usize) -> Result<usize> {
    if n < 1 {
        anyhow::bail!("N must be >= 1");
    }

    if messages.is_empty() {
        return Ok(0);
    }

    let before = messages.len();

    let mut keep_indices = vec![0usize];
    let tail_budget = n.saturating_sub(1);

    if tail_budget > 0 && messages.len() > 1 {
        let tail_start = 1usize.max(messages.len().saturating_sub(tail_budget));
        keep_indices.extend(tail_start..messages.len());
    }

    keep_indices.sort_unstable();
    keep_indices.dedup();

    let mut out = Vec::with_capacity(keep_indices.len());
    for idx in keep_indices {
        out.push(messages[idx].clone());
    }

    *messages = out;
    Ok(before.saturating_sub(messages.len()))
}

/// Dump context to file
pub fn dump_context(name: &str, messages: &[AnyMessage]) -> Result<PathBuf> {
    let context_dir = resolve_context_dir();
    ensure_directory(&context_dir)?;

    let safe_name = sanitize_context_name(name)?;
    let path = context_dir.join(format!("{}.json", safe_name));
    let payload = ContextDump {
        name: safe_name,
        timestamp: Utc::now(),
        message_count: messages.len(),
        messages: messages.to_vec(),
    };

    let raw = serde_json::to_vec_pretty(&payload).context("failed to serialize context dump")?;
    fs::write(&path, raw)
        .with_context(|| format!("failed to write context file '{}'", path.display()))?;

    Ok(path)
}

/// Load context from file
pub fn load_context(name: &str) -> Result<Vec<AnyMessage>> {
    let context_dir = resolve_context_dir();
    let safe_name = sanitize_context_name(name)?;
    let path = context_dir.join(format!("{}.json", safe_name));

    if !path.exists() {
        anyhow::bail!("context '{}' not found at {}", safe_name, path.display());
    }

    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read context file '{}'", path.display()))?;
    let dump: ContextDump = serde_json::from_str(&raw)
        .with_context(|| format!("invalid context JSON at {}", path.display()))?;

    Ok(dump.messages)
}

/// List available contexts
pub fn list_contexts(base_dir: &Path) -> Result<Vec<String>> {
    let context_dir = if base_dir.as_os_str().is_empty() {
        resolve_context_dir()
    } else {
        base_dir.to_path_buf()
    };
    if !context_dir.exists() {
        return Ok(Vec::new());
    }

    let mut names = Vec::new();
    for entry in fs::read_dir(&context_dir).with_context(|| {
        format!(
            "failed to read context directory '{}'",
            context_dir.display()
        )
    })? {
        let entry = entry.with_context(|| "failed to read context entry")?;
        let path = entry.path();
        if !is_json_session_file(&path) {
            continue;
        }

        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            names.push(stem.to_string());
        }
    }

    names.sort();
    Ok(names)
}

/// Interactive autosave restore
pub fn restore_autosave_interactively(base_dir: &Path) -> Result<Session> {
    let sessions = list_sessions(base_dir)?;
    if sessions.is_empty() {
        anyhow::bail!("no autosave sessions available in {}", base_dir.display());
    }

    println!("\nAutosave sessions:");
    for (idx, info) in sessions.iter().enumerate() {
        println!(
            "  [{}] {} | {} msgs | {} tokens | updated {} | {} bytes",
            idx + 1,
            info.id,
            info.message_count,
            info.total_tokens,
            info.updated_at,
            info.file_size
        );
    }

    print!(
        "\nSelect session number to restore (1-{}): ",
        sessions.len()
    );
    io::stdout().flush().ok();

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read selection input")?;

    let selected = input
        .trim()
        .parse::<usize>()
        .context("invalid selection: expected a number")?;

    if selected == 0 || selected > sessions.len() {
        anyhow::bail!(
            "selection out of range: got {}, expected 1..={}",
            selected,
            sessions.len()
        );
    }

    let chosen = &sessions[selected - 1];
    load_session(&chosen.id, base_dir)
}

fn ensure_directory(base_dir: &Path) -> Result<()> {
    fs::create_dir_all(base_dir).with_context(|| {
        format!(
            "failed to create autosave directory '{}'",
            base_dir.display()
        )
    })?;
    Ok(())
}

fn session_file_path(base_dir: &Path, session_id: &str) -> PathBuf {
    base_dir.join(format!("{}.json", session_id))
}

fn is_json_session_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("json"))
            .unwrap_or(false)
}

/// Display resumed history preview
pub fn display_resumed_history(messages: &[AnyMessage], count: usize) {
    if messages.is_empty() {
        println!("No resumed history available.");
        return;
    }

    let count = count.max(1);
    let start = messages.len().saturating_sub(count);

    println!(
        "\nResumed history preview (last {} messages):",
        messages.len() - start
    );
    for message in messages.iter().skip(start) {
        let role = role_label(message);
        let content = preview_for_message(message, 120);
        println!("  [{}] {}", role, content);
    }
}

fn estimate_total_tokens(messages: &[Message]) -> usize {
    messages.iter().map(estimate_message_tokens).sum()
}

fn estimate_message_tokens(message: &AnyMessage) -> usize {
    serde_json::to_string(message)
        .map(|s| s.split_whitespace().count())
        .unwrap_or(0)
}

fn compact_to_token_budget(messages: &mut Vec<AnyMessage>, token_budget: usize) {
    if messages.is_empty() {
        return;
    }

    let system_idx = messages
        .iter()
        .position(|m| matches!(m.base().category, MessageCategory::System));

    while estimate_total_tokens(messages) > token_budget {
        if let Some(remove_idx) = oldest_removable_index(messages, system_idx) {
            messages.remove(remove_idx);
        } else {
            break;
        }
    }
}

fn compact_with_summary(messages: &mut Vec<AnyMessage>, token_budget: usize) {
    if messages.len() <= 2 {
        compact_to_token_budget(messages, token_budget);
        return;
    }

    let system_idx = messages
        .iter()
        .position(|m| matches!(m.base().category, MessageCategory::System));

    let removable_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter_map(|(idx, _)| (Some(idx) != system_idx).then_some(idx))
        .collect();

    if removable_indices.len() < 3 {
        compact_to_token_budget(messages, token_budget);
        return;
    }

    let summarize_upto = removable_indices.len() / 2;
    let to_summarize = &removable_indices[..summarize_upto];

    let mut summary_bits = Vec::new();
    for idx in to_summarize {
        let preview = preview_for_message(&messages[*idx], 160);
        if !preview.is_empty() {
            summary_bits.push(preview);
        }
    }

    let summary_text = if summary_bits.is_empty() {
        "Summary unavailable for older messages.".to_string()
    } else {
        format!(
            "Summarized {} older messages:\n- {}",
            to_summarize.len(),
            summary_bits.join("\n- ")
        )
    };

    for idx in to_summarize.iter().rev() {
        messages.remove(*idx);
    }

    let summary_msg = AnyMessage::AgentResponse(AgentResponseMessage {
        base: BaseMessage::new(MessageCategory::Agent, None),
        content: summary_text,
        is_markdown: false,
        is_streaming: false,
    });

    let insert_idx = system_idx.map_or(0, |idx| (idx + 1).min(messages.len()));
    messages.insert(insert_idx, summary_msg);

    compact_to_token_budget(messages, token_budget);
}

fn oldest_removable_index(messages: &[AnyMessage], system_idx: Option<usize>) -> Option<usize> {
    messages
        .iter()
        .enumerate()
        .find_map(|(idx, _)| (Some(idx) != system_idx).then_some(idx))
}

fn role_label(message: &AnyMessage) -> &'static str {
    match message.base().category {
        MessageCategory::UserInteraction => "USER",
        MessageCategory::Agent => "ASSISTANT",
        MessageCategory::System => "SYSTEM",
        MessageCategory::ToolOutput => "TOOL",
        MessageCategory::Divider => "DIVIDER",
    }
}

fn preview_for_message(message: &AnyMessage, max_chars: usize) -> String {
    let raw = match message {
        AnyMessage::Text(TextMessage { text, .. }) => text.clone(),
        AnyMessage::AgentResponse(AgentResponseMessage { content, .. }) => content.clone(),
        _ => serde_json::to_string(message).unwrap_or_else(|_| "<unavailable>".to_string()),
    };

    truncate_for_preview(&raw, max_chars)
}

fn truncate_for_preview(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }

    let mut s = input.chars().take(max_chars).collect::<String>();
    s.push('…');
    s
}

fn sanitize_context_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        anyhow::bail!("context name cannot be empty");
    }

    let mut out = String::with_capacity(trimmed.len());
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else if ch.is_whitespace() {
            out.push('_');
        }
    }

    if out.is_empty() {
        anyhow::bail!("context name must contain alphanumeric characters");
    }

    Ok(out)
}

fn resolve_context_dir() -> PathBuf {
    // Via the shared helper rather than rebuilding the path: this had its own
    // copy of the directory name, so it kept pointing at the old location when
    // the application was renamed.
    crate::config::get_config_dir().join("contexts")
}
