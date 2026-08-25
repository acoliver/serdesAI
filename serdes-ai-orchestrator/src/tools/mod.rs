//! Tools that let an agent read, change, and exercise a codebase.
//!
//! The raw operations live in [`fs`] and [`shell`] and know nothing about
//! agents, so they can be tested directly. [`register_for_role`] wraps them as
//! agent tools.
//!
//! Because `AgentBuilder` has no toolset abstraction, a role's permissions are
//! expressed by *which tools get registered on it* — a read-only role simply
//! never receives `write_file`, so there is no way for it to call one.

pub mod fs;
pub mod shell;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serdes_ai_agent::{AgentBuilder, RunContext};
use serdes_ai_tools::{ToolError, ToolReturn};

use crate::role::Role;

/// Why a tool call could not be carried out.
///
/// Distinct from a command merely exiting non-zero, which is a normal result the
/// agent is expected to read and react to.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolFailure {
    /// The path was malformed or escaped the workspace root.
    #[error("invalid path {path}: {reason}")]
    InvalidPath {
        /// The offending path.
        path: String,
        /// Why it was rejected.
        reason: String,
    },

    /// The edit could not be applied unambiguously.
    #[error("cannot edit {path}: {reason}")]
    InvalidEdit {
        /// The file involved.
        path: String,
        /// Why the edit was refused.
        reason: String,
    },

    /// The filesystem refused the operation.
    #[error("io error on {path}: {message}")]
    Io {
        /// The file involved.
        path: String,
        /// The underlying message.
        message: String,
    },

    /// The command was unusable.
    #[error("invalid command: {reason}")]
    InvalidCommand {
        /// Why it was rejected.
        reason: String,
    },

    /// The command ran past its deadline.
    #[error("command timed out after {seconds}s: {command}")]
    Timeout {
        /// The command that hung.
        command: String,
        /// The limit it exceeded.
        seconds: u64,
    },
}

impl From<ToolFailure> for ToolError {
    fn from(value: ToolFailure) -> Self {
        ToolError::execution_failed(value.to_string())
    }
}

/// Shared, cheaply-cloneable tool configuration.
#[derive(Debug, Clone)]
pub struct ToolContext {
    /// Directory every path is resolved against and every command runs in.
    pub root: Arc<PathBuf>,
}

impl ToolContext {
    /// Create a context rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: Arc::new(root.into()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReadArgs {
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
struct BashArgs {
    command: String,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

/// Register the tools a given role is permitted to use.
///
/// Read-only tools go to every role. `write_file`/`edit_file` are gated on
/// [`Role::can_write`] and `bash` on [`Role::can_run_shell`], so a reviewer can
/// run the test suite but cannot quietly repair what it is judging.
pub fn register_for_role(
    builder: AgentBuilder<(), String>,
    role: Role,
    ctx: ToolContext,
) -> AgentBuilder<(), String> {
    let mut builder = register_read_only(builder, ctx.clone());

    if role.can_write() {
        builder = register_write(builder, ctx.clone());
    }

    if role.can_run_shell() {
        builder = register_shell(builder, ctx);
    }

    builder
}

/// A JSON Schema object for a tool's parameters.
///
/// Written out rather than derived: this is the only thing that tells the model
/// which arguments a tool takes. A tool registered without one advertises no
/// parameters, so the model guesses the names and the call fails to parse.
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

fn register_read_only(
    builder: AgentBuilder<(), String>,
    ctx: ToolContext,
) -> AgentBuilder<(), String> {
    let read_ctx = ctx.clone();
    let builder = builder.tool_fn_async_with_schema(
        "read_file",
        "Read the full contents of a file, relative to the workspace root.",
        params(
            serde_json::json!({"path": string_prop("Path to the file, relative to the workspace root.")}),
            &["path"],
        ),
        move |_c: &RunContext<()>, args: ReadArgs| {
            let ctx = read_ctx.clone();
            async move {
                let content = fs::read_file(&ctx.root, &args.path)?;
                Ok(ToolReturn::text(content))
            }
        },
    );

    let list_ctx = ctx;
    builder.tool_fn_async_with_schema(
        "list_files",
        "List the entries directly under a directory. Directories end with '/'.",
        params(
            serde_json::json!({"path": string_prop("Directory to list, relative to the workspace root.")}),
            &["path"],
        ),
        move |_c: &RunContext<()>, args: ListArgs| {
            let ctx = list_ctx.clone();
            async move {
                let entries = fs::list_files(&ctx.root, &args.path)?;
                Ok(ToolReturn::text(entries.join("\n")))
            }
        },
    )
}

fn register_write(builder: AgentBuilder<(), String>, ctx: ToolContext) -> AgentBuilder<(), String> {
    let write_ctx = ctx.clone();
    let builder = builder.tool_fn_async_with_schema(
        "write_file",
        "Create or overwrite a file with the given content. Parent directories are created automatically.",
        params(
            serde_json::json!({
                "path": string_prop("Path to the file, relative to the workspace root."),
                "content": string_prop("The complete contents to write."),
            }),
            &["path", "content"],
        ),
        move |_c: &RunContext<()>, args: WriteArgs| {
            let ctx = write_ctx.clone();
            async move {
                let out = fs::write_file(&ctx.root, &args.path, &args.content)?;
                let verb = if out.existed { "Overwrote" } else { "Created" };
                Ok(ToolReturn::text(format!(
                    "{verb} {} ({} bytes)",
                    out.path, out.bytes
                )))
            }
        },
    );

    let edit_ctx = ctx;
    builder.tool_fn_async_with_schema(
        "edit_file",
        "Replace an exact string in a file. old_string must occur exactly once — \
         include surrounding context to make it unique.",
        params(
            serde_json::json!({
                "path": string_prop("Path to the file, relative to the workspace root."),
                "old_string": string_prop("The exact text to replace. Must occur exactly once."),
                "new_string": string_prop("The text to put in its place."),
            }),
            &["path", "old_string", "new_string"],
        ),
        move |_c: &RunContext<()>, args: EditArgs| {
            let ctx = edit_ctx.clone();
            async move {
                let out = fs::edit_file(&ctx.root, &args.path, &args.old_string, &args.new_string)?;
                Ok(ToolReturn::text(format!(
                    "Edited {} at line {}",
                    out.path, out.line
                )))
            }
        },
    )
}

fn register_shell(builder: AgentBuilder<(), String>, ctx: ToolContext) -> AgentBuilder<(), String> {
    builder.tool_fn_async_with_schema(
        "bash",
        "Run a shell command in the workspace root and return its output. \
         A non-zero exit status is returned as output, not as an error.",
        params(
            serde_json::json!({
                "command": string_prop("The shell command to run."),
                "timeout_seconds": {"type": "integer", "description": "How long to allow before giving up."},
            }),
            &["command"],
        ),
        move |_c: &RunContext<()>, args: BashArgs| {
            let ctx = ctx.clone();
            async move {
                let limit = args.timeout_seconds.map(Duration::from_secs);
                let out = shell::run(&ctx.root, &args.command, limit).await?;

                let mut rendered = String::new();
                if !out.stdout.trim().is_empty() {
                    rendered.push_str(&out.stdout);
                }
                if !out.stderr.trim().is_empty() {
                    if !rendered.is_empty() {
                        rendered.push('\n');
                    }
                    rendered.push_str("[stderr]\n");
                    rendered.push_str(&out.stderr);
                }
                match out.exit_code {
                    Some(0) => {}
                    Some(code) => rendered.push_str(&format!("\n[exit status {code}]")),
                    None => rendered.push_str("\n[terminated by signal]"),
                }
                if rendered.trim().is_empty() {
                    rendered.push_str("[no output]");
                }

                Ok(ToolReturn::text(rendered))
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serdes_ai_models::FunctionModel;

    /// Build an agent for `role` and report which tools it ended up with.
    fn tools_for(role: Role) -> Vec<String> {
        let builder = AgentBuilder::<(), String>::new(FunctionModel::constant_text("x"));
        let agent = register_for_role(builder, role, ToolContext::new(".")).build();

        let mut names: Vec<String> = agent.tools().iter().map(|d| d.name.clone()).collect();
        names.sort();
        names
    }

    #[test]
    fn code_role_can_read_write_and_run() {
        let tools = tools_for(Role::Code);

        for expected in ["read_file", "list_files", "write_file", "edit_file", "bash"] {
            assert!(
                tools.contains(&expected.to_string()),
                "missing {expected} in {tools:?}"
            );
        }
    }

    #[test]
    fn explore_role_is_strictly_read_only() {
        let tools = tools_for(Role::Explore);

        assert!(tools.contains(&"read_file".to_string()));
        assert!(!tools.contains(&"write_file".to_string()));
        assert!(!tools.contains(&"edit_file".to_string()));
        assert!(
            !tools.contains(&"bash".to_string()),
            "explore must not reach the shell, which would route around the read-only rule"
        );
    }

    #[test]
    fn judging_roles_may_run_tests_but_never_write() {
        for role in [Role::Reviewer, Role::Verifier] {
            let tools = tools_for(role);

            assert!(
                tools.contains(&"bash".to_string()),
                "{role} must be able to run tests"
            );
            assert!(
                !tools.contains(&"write_file".to_string()),
                "{role} must not be able to repair what it is judging"
            );
            assert!(!tools.contains(&"edit_file".to_string()), "{role}");
        }
    }
}
