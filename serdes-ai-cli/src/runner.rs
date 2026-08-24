use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use figlet_rs::FIGfont;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tracing::info;

use serdes_ai_agent::{Agent as SerdesAgent, AgentRun, AgentRunResult, RunOptions};
use serdes_ai_core::messages::{
    FileContent, ImageContent, ImageMediaType, UserContent, UserContentPart,
};

use crate::args::{Cli, RunMode};
use crate::bus::{AnyMessage, MessageBus};
use crate::commands;
use crate::commands::registry::{execute_command, CommandResult};
use crate::config;
use crate::input;
use crate::messages::{
    AgentResponseMessage, BaseMessage, MessageCategory, MessageLevel, TextMessage,
};
use crate::model_factory;
use crate::session;
use crate::terminal;
use crate::tools;
use crate::tui::{
    mark_tutorial_complete, run_tutorial_wizard, should_run_tutorial, TutorialResult,
};
use crate::wiggum;

pub type Agent = SerdesAgent<(), String>;
pub type AgentResult = AgentRunResult<String>;

#[derive(Debug, Clone)]
pub struct Attachment {
    pub source: String,
    pub part: UserContentPart,
}

#[derive(Debug, Clone)]
struct ParsedPrompt {
    prompt: String,
    attachments: Vec<Attachment>,
    link_attachments: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone)]
struct AutosaveState {
    session_id: String,
    created_at: chrono::DateTime<Utc>,
    transcript: Vec<AnyMessage>,
}

impl AutosaveState {
    fn new(session_id: String) -> Self {
        Self {
            session_id,
            created_at: Utc::now(),
            transcript: Vec::new(),
        }
    }

    fn try_restore_current() -> Option<Self> {
        let current_id = session::get_current_session_id()?;
        let autosave_dir = config::get_autosave_dir();
        let loaded = session::load_session(&current_id, &autosave_dir).ok()?;

        Some(Self {
            session_id: loaded.id,
            created_at: loaded.created_at,
            transcript: loaded.messages,
        })
    }

    fn push_user_message(&mut self, text: &str) {
        self.transcript.push(AnyMessage::Text(TextMessage {
            base: BaseMessage::new(MessageCategory::UserInteraction, None),
            level: MessageLevel::Info,
            text: format!("USER: {text}"),
        }));
    }

    fn push_agent_message(&mut self, text: &str) {
        self.transcript
            .push(AnyMessage::AgentResponse(AgentResponseMessage {
                base: BaseMessage::new(MessageCategory::Agent, None),
                content: text.to_string(),
                is_markdown: true,
                is_streaming: false,
            }));
    }

    fn save(&self, bus: &MessageBus) {
        let autosave_dir = config::get_autosave_dir();
        let snapshot = session::Session {
            id: self.session_id.clone(),
            created_at: self.created_at,
            updated_at: Utc::now(),
            messages: self.transcript.clone(),
        };

        match session::autosave_current_session(&snapshot) {
            Ok(path) => bus.emit_debug(format!("Autosaved session to {}", path.display())),
            Err(err) => bus.emit_warning(format!(
                "Failed to autosave session in {}: {err}",
                autosave_dir.display()
            )),
        }
    }
}

pub async fn run(cli: Cli) -> anyhow::Result<()> {
    config::ensure_config_exists().context("failed to ensure config exists")?;
    config::load_api_keys_to_environment().context("failed to load API keys")?;
    terminal::enable_ansi_support();
    commands::init_all();

    let bus = Arc::new(MessageBus::new());

    let run_result = async {
        if let Some(model) = cli.get_model() {
            validate_model(model)?;
            config::set_model_name(model);
            bus.emit_success(format!("🎯 Using model: {model}"));
        }

        if let Some(agent_name) = cli.get_agent() {
            validate_agent(agent_name)?;
            config::set_agent_name(agent_name);
            bus.emit_success(format!("🤖 Using agent: {agent_name}"));
        }

        run_startup_callbacks(&bus).await?;

        if cli.is_prompt_only() {
            let prompt = cli
                .get_initial_prompt()
                .ok_or_else(|| anyhow!("prompt-only mode requested without a prompt"))?;
            execute_prompt(Arc::clone(&bus), prompt, cli.mode).await
        } else {
            set_run_mode(cli.mode);
            interactive_mode(Arc::clone(&bus), cli.get_initial_prompt()).await
        }
    }
    .await;

    let shutdown_result = run_shutdown_callbacks(&bus).await;

    terminal::reset_windows_terminal_full();
    terminal::reset_unix_terminal();

    run_result.and(shutdown_result)
}

pub async fn interactive_mode(
    bus: Arc<MessageBus>,
    initial_command: Option<String>,
) -> anyhow::Result<()> {
    print_intro_banner();
    show_help_messages(&bus);
    terminal::print_truecolor_warning();

    let mut autosave = if let Some(restored) = AutosaveState::try_restore_current() {
        bus.emit_info(format!(
            "Found autosave session '{}' with {} messages.",
            restored.session_id,
            restored.transcript.len()
        ));
        restored
    } else {
        let sid =
            session::get_current_session_id().unwrap_or_else(session::finalize_autosave_session);
        AutosaveState::new(sid)
    };

    maybe_run_onboarding(&bus).await?;

    let mut history = Vec::new();

    if let Some(cmd) = initial_command {
        run_interactive_turn(&bus, &mut history, &mut autosave, cmd).await?;
    }

    loop {
        let prompt = get_prompt_with_active_model();
        let input = match get_input(&prompt).await {
            Ok(Some(v)) => v,
            Ok(None) => {
                bus.emit_success("Goodbye! (Ctrl+D)");
                break;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {
                bus.emit_warning("Input cancelled (Ctrl+C)");
                continue;
            }
            Err(err) => return Err(err).context("failed reading user input"),
        };

        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }

        let lower = trimmed.to_ascii_lowercase();
        if matches!(lower.as_str(), "exit" | "quit" | "/exit" | "/quit") {
            bus.emit_success("Goodbye!");
            break;
        }

        if matches!(lower.as_str(), "clear" | "/clear") {
            print!("\x1B[2J\x1B[H");
            io::stdout().flush().ok();

            history.clear();
            let next_session = session::finalize_autosave_session();
            autosave = AutosaveState::new(next_session.clone());
            bus.emit_warning("Conversation history cleared.");
            bus.emit_info(format!("Autosave session rotated to: {next_session}"));
            continue;
        }

        if let Err(err) = run_interactive_turn(&bus, &mut history, &mut autosave, input).await {
            if err.to_string() == "exit requested" {
                bus.emit_success("Goodbye!");
                break;
            }

            bus.emit_error(format!("Error: {err}"));
        }
    }

    Ok(())
}

/// Show or change the run mode.
fn handle_mode_command(bus: &MessageBus, argument: &str) {
    if argument.is_empty() {
        bus.emit_info(format!("Mode: {}", describe_mode(get_run_mode())));
        bus.emit_info("Use /mode single|fast|workflow to change it.".to_string());
        return;
    }

    let next = match argument.to_ascii_lowercase().as_str() {
        "single" => RunMode::Single,
        "fast" => RunMode::Fast,
        "workflow" => RunMode::Workflow,
        other => {
            bus.emit_warning(format!(
                "Unknown mode '{other}'. Expected one of: single, fast, workflow."
            ));
            return;
        }
    };

    set_run_mode(next);
    bus.emit_success(format!("Mode: {}", describe_mode(next)));
}

/// A one-line description of what a mode does.
fn describe_mode(mode: RunMode) -> &'static str {
    match mode {
        RunMode::Single => "single — one agent answers directly",
        RunMode::Fast => "fast — an orchestrator delegates to subagents, no gate",
        RunMode::Workflow => "workflow — plan, your approval, execution, then a verification gate",
    }
}

/// The mode interactive turns run in.
///
/// Held globally because `/mode` changes it mid-session and the input loop has
/// no other channel to the dispatcher.
static RUN_MODE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Set the mode for subsequent turns.
pub fn set_run_mode(mode: RunMode) {
    let value = match mode {
        RunMode::Single => 0,
        RunMode::Fast => 1,
        RunMode::Workflow => 2,
    };
    RUN_MODE.store(value, std::sync::atomic::Ordering::SeqCst);
}

/// The mode subsequent turns will run in.
pub fn get_run_mode() -> RunMode {
    match RUN_MODE.load(std::sync::atomic::Ordering::SeqCst) {
        1 => RunMode::Fast,
        2 => RunMode::Workflow,
        _ => RunMode::Single,
    }
}

/// Execute one prompt under `mode`.
pub async fn execute_prompt(
    bus: Arc<MessageBus>,
    prompt: String,
    mode: RunMode,
) -> anyhow::Result<()> {
    if matches!(mode, RunMode::Single) {
        return execute_single_prompt(bus, prompt).await;
    }

    let summary = crate::orchestration::run(Arc::clone(&bus), mode, &prompt).await?;

    bus.emit(AnyMessage::AgentResponse(AgentResponseMessage {
        base: BaseMessage::new(MessageCategory::Agent, None),
        content: summary,
        is_markdown: true,
        is_streaming: false,
    }));

    Ok(())
}

pub async fn execute_single_prompt(bus: Arc<MessageBus>, prompt: String) -> anyhow::Result<()> {
    let parsed = parse_prompt_attachments(&prompt);
    for warning in &parsed.warnings {
        bus.emit_warning(warning.clone());
    }

    let agent = get_current_agent().await?;
    let result = run_prompt_with_attachments(
        &agent,
        parsed.prompt,
        parsed.attachments,
        parsed.link_attachments,
    )
    .await?;

    bus.emit(AnyMessage::AgentResponse(AgentResponseMessage {
        base: BaseMessage::new(MessageCategory::Agent, None),
        content: result.output.clone(),
        is_markdown: true,
        is_streaming: false,
    }));

    Ok(())
}

pub async fn run_prompt_with_attachments(
    agent: &Agent,
    prompt: String,
    attachments: Vec<Attachment>,
    link_attachments: Vec<String>,
) -> anyhow::Result<AgentResult> {
    let mut parts = Vec::new();

    if !prompt.trim().is_empty() {
        parts.push(UserContentPart::text(prompt));
    }

    for attachment in attachments {
        parts.push(attachment.part);
    }

    for link in link_attachments {
        parts.push(UserContentPart::text(format!("Reference URL: {link}")));
    }

    let content = if parts.len() <= 1 {
        match parts.into_iter().next() {
            Some(UserContentPart::Text { text }) => UserContent::text(text),
            Some(single) => UserContent::parts(vec![single]),
            None => UserContent::text(""),
        }
    } else {
        UserContent::parts(parts)
    };

    let spinner_done = Arc::new(AtomicBool::new(false));
    let spinner_task = start_spinner(Arc::clone(&spinner_done), "Thinking...");

    let cancel_token = serdes_ai_agent::CancellationToken::new();
    let run = AgentRun::new_with_cancel(
        agent,
        content,
        (),
        RunOptions::default(),
        cancel_token.clone(),
    )
    .await
    .context("failed to initialize agent run")?;

    let mut run_future = Box::pin(run.run_to_completion());

    let result = tokio::select! {
        run_result = &mut run_future => {
            run_result.map_err(|err| anyhow!(err.to_string()))
        }
        _ = tokio::signal::ctrl_c() => {
            cancel_token.cancel();
            Err(anyhow!("agent execution cancelled by user"))
        }
    };

    spinner_done.store(true, Ordering::SeqCst);
    let _ = spinner_task.await;
    eprint!("\r\x1B[2K");
    io::stderr().flush().ok();

    result
}

pub fn print_intro_banner() {
    if let Ok(font) = FIGfont::standard() {
        if let Some(figure) = font.convert("CODE PUPPY") {
            println!("\n{figure}\n");
            return;
        }
    }

    println!("\n🐶 CODE PUPPY\n");
}

pub fn show_help_messages(bus: &MessageBus) {
    bus.emit_info("Type '/exit', '/quit', 'exit', or 'quit' to leave.".to_string());
    bus.emit_info("Type '/clear' or 'clear' to reset history and rotate autosave.".to_string());
    bus.emit_info("Type '/help' to show this startup guidance again.".to_string());
    bus.emit_info("Press Ctrl+C to cancel current processing, Ctrl+D to exit cleanly.".to_string());
}

pub fn validate_model(model: &str) -> Result<()> {
    let candidate = model.trim();
    if candidate.is_empty() {
        return Err(anyhow!("model cannot be empty"));
    }

    let provider = if candidate.contains(':') {
        candidate.split(':').next().unwrap_or_default()
    } else {
        "openai"
    };

    let allowed = [
        "openai",
        "gpt",
        "anthropic",
        "claude",
        "groq",
        "mistral",
        "ollama",
        "bedrock",
        "aws",
        "openrouter",
        "or",
        "huggingface",
        "hf",
        "cohere",
        "co",
        "google",
        "gemini",
    ];

    if !allowed.contains(&provider) {
        return Err(anyhow!(
            "unknown provider '{provider}' in model '{candidate}'. Use provider:model format."
        ));
    }

    Ok(())
}

pub fn validate_agent(agent: &str) -> Result<()> {
    let normalized = agent.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(anyhow!("agent cannot be empty"));
    }

    let supported = ["code-puppy", "default"];
    if !supported.contains(&normalized.as_str()) {
        return Err(anyhow!(
            "agent '{}' not found. Available agents: {}",
            normalized,
            supported.join(", ")
        ));
    }

    Ok(())
}

/// Create agent with proper model and tools.
pub async fn get_current_agent() -> Result<Agent> {
    let agent_name = config::get_agent_name();
    let model_spec = config::get_pinned_model(&agent_name).unwrap_or_else(config::get_model_name);

    info!("Creating agent: name={}, model={}", agent_name, model_spec);

    let model = model_factory::create_model_from_spec(&model_spec)
        .await
        .map_err(|e| anyhow!("Failed to create model '{}': {}", model_spec, e))?;

    let mut builder = serdes_ai_agent::AgentBuilder::from_arc(model)
        .name(agent_name.clone())
        .system_prompt(get_system_prompt(&agent_name));

    let bus = Arc::new(MessageBus::new());
    let mut tool_registry = tools::CliToolRegistry::new(bus);
    tool_registry.register_builtin_tools()?;
    builder = tool_registry.apply_to_builder(builder);

    let temperature = config::get_temperature();
    builder = builder.temperature(temperature);

    let max_tokens = config::get_max_tokens();
    if max_tokens > 0 {
        builder = builder.max_tokens(max_tokens as u64);
    }

    let agent = builder.build();

    info!("Agent created successfully");
    Ok(agent)
}

/// Get system prompt based on agent name.
fn get_system_prompt(agent_name: &str) -> String {
    match agent_name {
        "code-puppy" => r#"You are Code Puppy, a helpful coding assistant.

Your capabilities:
- Write and explain code in any programming language
- Read and analyze files
- Search through codebases
- Execute shell commands
- Fetch web content
- Generate images

Guidelines:
- Be concise but thorough
- Always use the available tools when appropriate
- For file operations, use the read_file, list_files, and grep tools
- For web searches, use web_search and web_fetch
- For code execution, use code_execution
- Show code examples when helpful
- Ask clarifying questions when needed
"#
        .to_string(),
        "default" => "You are a helpful assistant.".to_string(),
        _ => format!("You are {}. Be helpful and concise.", agent_name),
    }
}

async fn run_interactive_turn(
    bus: &Arc<MessageBus>,
    history: &mut Vec<serdes_ai_core::messages::ModelRequest>,
    autosave: &mut AutosaveState,
    raw_input: String,
) -> Result<()> {
    let mut effective_input = raw_input;

    loop {
        let parsed = parse_prompt_attachments(&effective_input);
        for warning in &parsed.warnings {
            bus.emit_warning(warning.clone());
        }

        let cleaned = parsed.prompt.trim().to_string();
        if cleaned.eq_ignore_ascii_case("/help") {
            show_help_messages(bus);
            return Ok(());
        }

        if let Some(rest) = cleaned.strip_prefix("/mode") {
            handle_mode_command(bus, rest.trim());
            return Ok(());
        }

        if cleaned.starts_with('/') {
            match execute_command(&cleaned) {
                CommandResult::Handled => return Ok(()),
                CommandResult::Exit => return Err(anyhow!("exit requested")),
                CommandResult::Prompt(next_prompt) => {
                    effective_input = next_prompt;
                    continue;
                }
                CommandResult::NotHandled => {
                    bus.emit_warning(format!("Unknown command: {cleaned}"));
                    return Ok(());
                }
            }
        }

        save_command_to_history(&effective_input);

        // A multi-agent turn runs the orchestrator. Attachments are not carried
        // through: subagents receive a self-contained task, not the session's
        // content parts, so silently dropping them would be misleading.
        let mode = get_run_mode();
        if !matches!(mode, RunMode::Single) {
            if !parsed.attachments.is_empty() {
                bus.emit_warning(
                    "Attachments are ignored in multi-agent modes; mention the paths in the request instead."
                        .to_string(),
                );
            }

            let summary = crate::orchestration::run(Arc::clone(bus), mode, &cleaned).await?;

            bus.emit(AnyMessage::AgentResponse(AgentResponseMessage {
                base: BaseMessage::new(MessageCategory::Agent, None),
                content: summary.clone(),
                is_markdown: true,
                is_streaming: false,
            }));

            autosave.push_user_message(&cleaned);
            autosave.push_agent_message(&summary);
            autosave.save(bus);

            return Ok(());
        }

        let agent = get_current_agent().await?;
        let run_opts = if history.is_empty() {
            RunOptions::default()
        } else {
            RunOptions::default().message_history(history.clone())
        };

        let mut parts = Vec::new();
        if !cleaned.is_empty() {
            parts.push(UserContentPart::text(cleaned.clone()));
        }
        for attachment in &parsed.attachments {
            parts.push(attachment.part.clone());
        }
        for url in &parsed.link_attachments {
            parts.push(UserContentPart::text(format!("Reference URL: {url}")));
        }

        let content = if parts.len() <= 1 {
            match parts.into_iter().next() {
                Some(UserContentPart::Text { text }) => UserContent::text(text),
                Some(single) => UserContent::parts(vec![single]),
                None => UserContent::text(""),
            }
        } else {
            UserContent::parts(parts)
        };

        let result = execute_with_wiggum(bus, &agent, run_opts, content, &cleaned).await?;

        *history = result.messages.clone();

        bus.emit(AnyMessage::AgentResponse(AgentResponseMessage {
            base: BaseMessage::new(MessageCategory::Agent, None),
            content: result.output.clone(),
            is_markdown: true,
            is_streaming: false,
        }));

        autosave.push_user_message(&cleaned);
        autosave.push_agent_message(&result.output);
        autosave.save(bus);

        return Ok(());
    }
}

/// Execute agent with potential wiggum loop.
async fn execute_with_wiggum(
    bus: &MessageBus,
    agent: &Agent,
    run_opts: RunOptions,
    initial_content: UserContent,
    _initial_prompt: &str,
) -> anyhow::Result<AgentResult> {
    let mut current_content = initial_content;
    let mut current_run_opts = run_opts;
    let mut iteration = 0usize;
    let max_iterations = 10usize;

    loop {
        iteration += 1;
        if iteration > max_iterations {
            bus.emit_warning("Wiggum loop reached maximum iterations (10). Stopping.");
            wiggum::stop_wiggum();
            break;
        }

        let result = execute_agent_prompt(agent, current_content, current_run_opts.clone()).await?;

        if wiggum::is_wiggum_active() {
            if wiggum::wiggum_should_continue(&result.output) {
                current_content = UserContent::text(format!(
                    "{}\n\nAgent asked: {}\n\nPlease provide the requested information.",
                    wiggum::next_wiggum_prompt(),
                    result.output
                ));
                current_run_opts = RunOptions::default().message_history(result.messages.clone());
                bus.emit_info(format!("🍩 Wiggum iteration {}...", iteration));
                continue;
            }

            bus.emit_success("🍩 Wiggum loop complete!");
            wiggum::stop_wiggum();
        }

        return Ok(result);
    }

    Err(anyhow!("wiggum loop exited without an agent result"))
}

/// Execute single agent prompt.
async fn execute_agent_prompt(
    agent: &Agent,
    content: UserContent,
    run_opts: RunOptions,
) -> anyhow::Result<AgentResult> {
    let spinner_done = Arc::new(AtomicBool::new(false));
    let spinner_task = start_spinner(Arc::clone(&spinner_done), "Thinking...");

    let result = agent
        .run_with_options(content, (), run_opts)
        .await
        .map_err(|err| anyhow!(err.to_string()));

    spinner_done.store(true, Ordering::SeqCst);
    let _ = spinner_task.await;
    eprint!("\r\x1B[2K");
    io::stderr().flush().ok();

    result
}

fn parse_prompt_attachments(raw: &str) -> ParsedPrompt {
    let mut attachments = Vec::new();
    let mut links = Vec::new();
    let mut warnings = Vec::new();
    let mut kept_tokens = Vec::new();

    for token in raw.split_whitespace() {
        let normalized = token.trim_matches(|c| c == '"' || c == '\'' || c == ',');

        if normalized.starts_with("http://") || normalized.starts_with("https://") {
            links.push(normalized.to_string());
            continue;
        }

        if let Some(path_candidate) = normalized.strip_prefix('@') {
            match read_attachment_from_path(Path::new(path_candidate)) {
                Ok(att) => attachments.push(att),
                Err(err) => {
                    warnings.push(format!("Could not attach '{}': {}", path_candidate, err))
                }
            }
            continue;
        }

        kept_tokens.push(token.to_string());
    }

    ParsedPrompt {
        prompt: kept_tokens.join(" ").trim().to_string(),
        attachments,
        link_attachments: links,
        warnings,
    }
}

fn read_attachment_from_path(path: &Path) -> Result<Attachment> {
    if !path.exists() {
        return Err(anyhow!("path does not exist"));
    }

    if !path.is_file() {
        return Err(anyhow!("path is not a file"));
    }

    let data = fs::read(path)
        .with_context(|| format!("failed to read attachment file '{}'", path.display()))?;
    let source = path.display().to_string();
    let file_name = path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("attachment")
        .to_string();

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let part = if let Some(img_ty) = ImageMediaType::from_extension(&ext) {
        UserContentPart::Image {
            image: ImageContent::binary(data, img_ty),
        }
    } else {
        let mime = guess_mime_type(path);
        let mut file_content = FileContent::binary(data, mime);
        if let FileContent::Binary(binary) = &mut file_content {
            binary.filename = Some(file_name);
        }
        UserContentPart::File { file: file_content }
    };

    Ok(Attachment { source, part })
}

fn guess_mime_type(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match ext.as_str() {
        "rs" => "text/rust",
        "py" => "text/x-python",
        "js" => "text/javascript",
        "ts" => "text/typescript",
        "tsx" => "text/tsx",
        "jsx" => "text/jsx",
        "json" => "application/json",
        "toml" => "application/toml",
        "yaml" | "yml" => "application/yaml",
        "md" => "text/markdown",
        "txt" => "text/plain",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn get_prompt_with_active_model() -> String {
    let model = config::get_model_name();
    format!("({model}) >>> ")
}

async fn get_input(prompt: &str) -> io::Result<Option<String>> {
    input::get_input_with_completion(prompt, None).await
}

fn save_command_to_history(command: &str) {
    let history_path = config::get_command_history_file();
    if let Some(parent) = history_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let mut file = match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&history_path)
    {
        Ok(f) => f,
        Err(_) => return,
    };

    let _ = writeln!(file, "{}", command.trim());
}

fn start_spinner(done: Arc<AtomicBool>, label: &'static str) -> JoinHandle<()> {
    tokio::spawn(async move {
        let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let mut idx = 0usize;

        while !done.load(Ordering::SeqCst) {
            eprint!("\r{} {}", frames[idx % frames.len()], label);
            io::stderr().flush().ok();
            idx += 1;
            sleep(Duration::from_millis(80)).await;
        }
    })
}

async fn maybe_run_onboarding(bus: &MessageBus) -> Result<()> {
    if !should_run_tutorial() {
        return Ok(());
    }

    match run_tutorial_wizard() {
        Ok(TutorialResult::Completed) => {
            mark_tutorial_complete();
            bus.emit_success("🎉 Tutorial complete! Welcome to Serdes AI!".to_string());
        }
        Ok(TutorialResult::Skipped) => {
            bus.emit_info("Tutorial skipped. Run /tutorial anytime!".to_string());
        }
        Err(err) => {
            bus.emit_warning(format!("Tutorial failed to launch: {err}"));
            bus.emit_info("👋 Welcome! Type /help to see available commands.".to_string());
            bus.emit_info("Tip: prefix file paths with @ to attach them to a prompt.".to_string());
        }
    }

    Ok(())
}

async fn run_startup_callbacks(_bus: &MessageBus) -> Result<()> {
    Ok(())
}

async fn run_shutdown_callbacks(_bus: &MessageBus) -> Result<()> {
    Ok(())
}
