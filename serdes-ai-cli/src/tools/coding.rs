//! Tools that let the CLI's agent read, change and exercise a codebase.
//!
//! Two things happen for every call: the operation itself, and a structured
//! message describing it. The renderer already knows how to draw file contents,
//! directory listings, grep hits, diffs and shell output — before this, nothing
//! produced those messages, so results arrived as undifferentiated tool text and
//! the display code sat unused.
//!
//! The operations themselves come from `serdes-ai-orchestrator`, so the
//! single-agent path and the multi-agent modes behave identically rather than
//! having two implementations that can drift.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use serdes_ai_agent::{AgentBuilder, RunContext};
use serdes_ai_orchestrator::tools::{ToolContext, fs as ops, shell};
use serdes_ai_tools::ToolReturn;

use crate::bus::{AnyMessage, MessageBus};
use crate::messages::{
    BaseMessage, DiffLine, DiffLineType, DiffMessage, FileContentMessage, FileEntry, FileEntryType,
    FileListingMessage, GrepMatch, GrepResultMessage, MessageCategory, ShellOutputMessage,
    ShellStartMessage,
};

/// How many matches a single grep reports before stopping.
const MAX_GREP_MATCHES: usize = 200;

#[derive(Debug, Deserialize)]
struct PathArgs {
    path: String,
}

#[derive(Debug, Deserialize)]
struct ListArgs {
    #[serde(default = "dot")]
    path: String,
}

fn dot() -> String {
    ".".to_string()
}

#[derive(Debug, Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct EditArgs {
    path: String,
    old_string: String,
    new_string: String,
}

#[derive(Debug, Deserialize)]
struct GrepArgs {
    pattern: String,
    #[serde(default = "dot")]
    path: String,
}

#[derive(Debug, Deserialize)]
struct BashArgs {
    command: String,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

/// A JSON Schema object for a tool's parameters.
///
/// Written out rather than derived: the model is told the argument names from
/// this alone, so a tool registered without one is effectively uncallable —
/// the model guesses names and the call fails to deserialize.
fn params(properties: serde_json::Value, required: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

fn string_prop(description: &str) -> serde_json::Value {
    serde_json::json!({"type": "string", "description": description})
}

/// Register the coding tools on `builder`.
pub fn register(
    builder: AgentBuilder<(), String>,
    bus: Arc<MessageBus>,
    root: PathBuf,
) -> AgentBuilder<(), String> {
    let ctx = ToolContext::new(root);

    let builder = register_read(builder, Arc::clone(&bus), ctx.clone());
    let builder = register_write(builder, Arc::clone(&bus), ctx.clone());
    register_shell(builder, bus, ctx)
}

fn register_read(
    builder: AgentBuilder<(), String>,
    bus: Arc<MessageBus>,
    ctx: ToolContext,
) -> AgentBuilder<(), String> {
    let (read_bus, read_ctx) = (Arc::clone(&bus), ctx.clone());
    let builder = builder.tool_fn_async_with_schema(
        "read_file",
        "Read a file's contents, relative to the working directory.",
        params(
            serde_json::json!({"path": string_prop("Path to the file, relative to the working directory.")}),
            &["path"],
        ),
        move |_c: &RunContext<()>, args: PathArgs| {
            let (bus, ctx) = (Arc::clone(&read_bus), read_ctx.clone());
            async move {
                let content = ops::read_file(&ctx.root, &args.path)?;
                let total_lines = content.lines().count() as u32;

                bus.emit(AnyMessage::FileContent(FileContentMessage {
                    base: BaseMessage::new(MessageCategory::ToolOutput, None),
                    path: args.path.clone(),
                    content: content.clone(),
                    start_line: None,
                    num_lines: Some(total_lines),
                    total_lines,
                    // A rough count is enough for display; an exact tokenisation
                    // would mean pulling in a tokeniser for a status line.
                    num_tokens: (content.len() / 4) as u32,
                }));

                Ok(ToolReturn::text(content))
            }
        },
    );

    let (list_bus, list_ctx) = (Arc::clone(&bus), ctx.clone());
    let builder = builder.tool_fn_async_with_schema(
        "list_files",
        "List the entries in a directory. Directories are marked.",
        params(
            serde_json::json!({"path": string_prop("Directory to list. Defaults to the working directory.")}),
            &[],
        ),
        move |_c: &RunContext<()>, args: ListArgs| {
            let (bus, ctx) = (Arc::clone(&list_bus), list_ctx.clone());
            async move {
                let names = ops::list_files(&ctx.root, &args.path)?;

                let mut entries = Vec::new();
                let mut dir_count = 0;
                let mut file_count = 0;
                for name in &names {
                    let is_dir = name.ends_with('/');
                    if is_dir {
                        dir_count += 1;
                    } else {
                        file_count += 1;
                    }
                    entries.push(FileEntry {
                        path: name.trim_end_matches('/').to_string(),
                        entry_type: if is_dir {
                            FileEntryType::Dir
                        } else {
                            FileEntryType::File
                        },
                        size: 0,
                        depth: 0,
                    });
                }

                bus.emit(AnyMessage::FileListing(FileListingMessage {
                    base: BaseMessage::new(MessageCategory::ToolOutput, None),
                    directory: args.path.clone(),
                    files: entries,
                    recursive: false,
                    total_size: 0,
                    dir_count,
                    file_count,
                }));

                Ok(ToolReturn::text(names.join("\n")))
            }
        },
    );

    let (grep_bus, grep_ctx) = (bus, ctx);
    builder.tool_fn_async_with_schema(
        "grep",
        "Search files under a directory for a literal string.",
        params(
            serde_json::json!({
                "pattern": string_prop("The literal string to search for."),
                "path": string_prop("Directory to search. Defaults to the working directory."),
            }),
            &["pattern"],
        ),
        move |_c: &RunContext<()>, args: GrepArgs| {
            let (bus, ctx) = (Arc::clone(&grep_bus), grep_ctx.clone());
            async move {
                let matches = search(&ctx.root, &args.path, &args.pattern)?;

                bus.emit(AnyMessage::GrepResult(GrepResultMessage {
                    base: BaseMessage::new(MessageCategory::ToolOutput, None),
                    search_term: args.pattern.clone(),
                    directory: args.path.clone(),
                    matches: matches.clone(),
                    verbose: false,
                }));

                let rendered = matches
                    .iter()
                    .map(|m| format!("{}:{}: {}", m.file_path, m.line_number, m.line_content))
                    .collect::<Vec<_>>()
                    .join("\n");

                Ok(ToolReturn::text(if rendered.is_empty() {
                    "no matches".to_string()
                } else {
                    rendered
                }))
            }
        },
    )
}

fn register_write(
    builder: AgentBuilder<(), String>,
    bus: Arc<MessageBus>,
    ctx: ToolContext,
) -> AgentBuilder<(), String> {
    let (write_bus, write_ctx) = (Arc::clone(&bus), ctx.clone());
    let builder = builder.tool_fn_async_with_schema(
        "write_file",
        "Create or overwrite a file. Parent directories are created as needed.",
        params(
            serde_json::json!({
                "path": string_prop("Path to the file, relative to the working directory."),
                "content": string_prop("The complete contents to write."),
            }),
            &["path", "content"],
        ),
        move |_c: &RunContext<()>, args: WriteArgs| {
            let (bus, ctx) = (Arc::clone(&write_bus), write_ctx.clone());
            async move {
                // Read first so the diff shows what actually changed rather than
                // presenting a rewrite as if the whole file were new.
                let previous = ops::read_file(&ctx.root, &args.path).unwrap_or_default();
                let outcome = ops::write_file(&ctx.root, &args.path, &args.content)?;

                emit_diff(&bus, &outcome.path, &previous, &args.content);

                Ok(ToolReturn::text(format!(
                    "{} {} ({} bytes)",
                    if outcome.existed {
                        "Overwrote"
                    } else {
                        "Created"
                    },
                    outcome.path,
                    outcome.bytes
                )))
            }
        },
    );

    let (edit_bus, edit_ctx) = (bus, ctx);
    builder.tool_fn_async_with_schema(
        "edit_file",
        "Replace an exact string in a file. old_string must occur exactly once — \
         include surrounding context to make it unique.",
        params(
            serde_json::json!({
                "path": string_prop("Path to the file, relative to the working directory."),
                "old_string": string_prop("The exact text to replace. Must occur exactly once."),
                "new_string": string_prop("The text to put in its place."),
            }),
            &["path", "old_string", "new_string"],
        ),
        move |_c: &RunContext<()>, args: EditArgs| {
            let (bus, ctx) = (Arc::clone(&edit_bus), edit_ctx.clone());
            async move {
                let previous = ops::read_file(&ctx.root, &args.path)?;
                let outcome =
                    ops::edit_file(&ctx.root, &args.path, &args.old_string, &args.new_string)?;
                let updated = ops::read_file(&ctx.root, &args.path)?;

                emit_diff(&bus, &outcome.path, &previous, &updated);

                Ok(ToolReturn::text(format!(
                    "Edited {} at line {}",
                    outcome.path, outcome.line
                )))
            }
        },
    )
}

fn register_shell(
    builder: AgentBuilder<(), String>,
    bus: Arc<MessageBus>,
    ctx: ToolContext,
) -> AgentBuilder<(), String> {
    builder.tool_fn_async_with_schema(
        "bash",
        "Run a shell command in the working directory. A non-zero exit status is \
         returned as output, not as an error.",
        params(
            serde_json::json!({
                "command": string_prop("The shell command to run."),
                "timeout_seconds": {"type": "integer", "description": "How long to allow before giving up."},
            }),
            &["command"],
        ),
        move |_c: &RunContext<()>, args: BashArgs| {
            let (bus, ctx) = (Arc::clone(&bus), ctx.clone());
            async move {
                let command_id = uuid::Uuid::new_v4().to_string();

                bus.emit(AnyMessage::ShellStart(ShellStartMessage {
                    base: BaseMessage::new(MessageCategory::ToolOutput, None),
                    command: args.command.clone(),
                    cwd: ctx.root.display().to_string(),
                }));

                let limit = args.timeout_seconds.map(std::time::Duration::from_secs);
                let outcome = shell::run(&ctx.root, &args.command, limit).await?;

                let mut rendered = String::new();
                if !outcome.stdout.trim().is_empty() {
                    rendered.push_str(&outcome.stdout);
                }
                if !outcome.stderr.trim().is_empty() {
                    if !rendered.is_empty() {
                        rendered.push('\n');
                    }
                    rendered.push_str("[stderr]\n");
                    rendered.push_str(&outcome.stderr);
                }
                if rendered.trim().is_empty() {
                    rendered.push_str("[no output]");
                }

                bus.emit(AnyMessage::ShellOutput(ShellOutputMessage {
                    base: BaseMessage::new(MessageCategory::ToolOutput, None),
                    command_id,
                    output: rendered.clone(),
                    success: outcome.success(),
                    exit_code: outcome.exit_code,
                }));

                // The exit status goes to the model too: a command that failed is
                // something it has to be able to see and react to.
                if let Some(code) = outcome.exit_code {
                    if code != 0 {
                        rendered.push_str(&format!("\n[exit status {code}]"));
                    }
                }

                Ok(ToolReturn::text(rendered))
            }
        },
    )
}

/// Publish a line diff between `before` and `after`.
fn emit_diff(bus: &MessageBus, path: &str, before: &str, after: &str) {
    let lines = diff_lines(before, after);
    let additions = lines
        .iter()
        .filter(|l| matches!(l.line_type, DiffLineType::Add))
        .count() as u32;
    let deletions = lines
        .iter()
        .filter(|l| matches!(l.line_type, DiffLineType::Remove))
        .count() as u32;

    bus.emit(AnyMessage::Diff(DiffMessage {
        base: BaseMessage::new(MessageCategory::ToolOutput, None),
        file_path: path.to_string(),
        lines,
        additions,
        deletions,
    }));
}

/// A line-level diff.
///
/// Deliberately simple: a common prefix and suffix are held as context and
/// everything between them is shown as removed then added. That is enough to
/// see what a tool call did, without taking on a diff library for a status
/// display.
fn diff_lines(before: &str, after: &str) -> Vec<DiffLine> {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();

    let prefix = old
        .iter()
        .zip(new.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let max_suffix = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();

    let mut lines = Vec::new();

    for (i, line) in old.iter().enumerate().take(prefix) {
        lines.push(DiffLine {
            line_type: DiffLineType::Context,
            content: (*line).to_string(),
            old_line_num: Some(i as u32 + 1),
            new_line_num: Some(i as u32 + 1),
        });
    }

    for (offset, line) in old[prefix..old.len() - suffix].iter().enumerate() {
        lines.push(DiffLine {
            line_type: DiffLineType::Remove,
            content: (*line).to_string(),
            old_line_num: Some((prefix + offset) as u32 + 1),
            new_line_num: None,
        });
    }

    for (offset, line) in new[prefix..new.len() - suffix].iter().enumerate() {
        lines.push(DiffLine {
            line_type: DiffLineType::Add,
            content: (*line).to_string(),
            old_line_num: None,
            new_line_num: Some((prefix + offset) as u32 + 1),
        });
    }

    for (offset, line) in new[new.len() - suffix..].iter().enumerate() {
        let n = (new.len() - suffix + offset) as u32 + 1;
        lines.push(DiffLine {
            line_type: DiffLineType::Context,
            content: (*line).to_string(),
            old_line_num: Some((old.len() - suffix + offset) as u32 + 1),
            new_line_num: Some(n),
        });
    }

    lines
}

/// Search text files under `dir` for `needle`.
fn search(
    root: &std::path::Path,
    dir: &str,
    needle: &str,
) -> Result<Vec<GrepMatch>, serdes_ai_orchestrator::ToolFailure> {
    let start = ops::resolve_in_root(root, dir)?;
    let mut matches = Vec::new();
    let mut stack = vec![start];

    while let Some(current) = stack.pop() {
        if matches.len() >= MAX_GREP_MATCHES {
            break;
        }

        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();

            // Skipping these keeps a search over a project from spending its
            // whole budget inside build output or version-control metadata.
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }

            if path.is_dir() {
                stack.push(path);
                continue;
            }

            let Ok(content) = std::fs::read_to_string(&path) else {
                continue; // not text
            };

            for (i, line) in content.lines().enumerate() {
                if line.contains(needle) {
                    matches.push(GrepMatch {
                        file_path: path
                            .strip_prefix(root)
                            .unwrap_or(&path)
                            .display()
                            .to_string(),
                        line_number: i as u32 + 1,
                        line_content: line.trim().to_string(),
                    });
                    if matches.len() >= MAX_GREP_MATCHES {
                        break;
                    }
                }
            }
        }
    }

    Ok(matches)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pure_addition_is_all_adds_and_context() {
        let lines = diff_lines("a\nb\n", "a\nb\nc\n");

        let kinds: Vec<&DiffLineType> = lines.iter().map(|l| &l.line_type).collect();
        assert!(matches!(kinds[0], DiffLineType::Context));
        assert!(matches!(kinds[1], DiffLineType::Context));
        assert!(matches!(kinds[2], DiffLineType::Add));
        assert_eq!(lines[2].content, "c");
    }

    #[test]
    fn a_changed_line_is_a_removal_then_an_addition() {
        let lines = diff_lines("a\nOLD\nc\n", "a\nNEW\nc\n");

        let removed: Vec<&str> = lines
            .iter()
            .filter(|l| matches!(l.line_type, DiffLineType::Remove))
            .map(|l| l.content.as_str())
            .collect();
        let added: Vec<&str> = lines
            .iter()
            .filter(|l| matches!(l.line_type, DiffLineType::Add))
            .map(|l| l.content.as_str())
            .collect();

        assert_eq!(removed, vec!["OLD"]);
        assert_eq!(added, vec!["NEW"]);
    }

    #[test]
    fn a_new_file_is_all_additions() {
        let lines = diff_lines("", "one\ntwo\n");

        assert_eq!(lines.len(), 2);
        assert!(
            lines
                .iter()
                .all(|l| matches!(l.line_type, DiffLineType::Add))
        );
    }

    #[test]
    fn an_unchanged_file_produces_no_edits() {
        let lines = diff_lines("same\n", "same\n");

        assert!(
            lines
                .iter()
                .all(|l| matches!(l.line_type, DiffLineType::Context))
        );
    }

    #[test]
    fn a_deletion_is_reported_as_removed() {
        let lines = diff_lines("a\ngone\nb\n", "a\nb\n");

        let removed: Vec<&str> = lines
            .iter()
            .filter(|l| matches!(l.line_type, DiffLineType::Remove))
            .map(|l| l.content.as_str())
            .collect();

        assert_eq!(removed, vec!["gone"]);
    }
}
