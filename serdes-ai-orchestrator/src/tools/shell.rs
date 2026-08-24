//! Shell execution for coding agents.
//!
//! Kept separate from the agent layer so it can be tested directly. Commands run
//! with the workspace root as their working directory and are bounded by a
//! timeout — an agent that runs `cargo build` on a large tree must not wedge the
//! whole orchestration.

use std::path::Path;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

use super::ToolFailure;

/// Default per-command timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// Upper bound a caller may request.
pub const MAX_TIMEOUT: Duration = Duration::from_secs(600);

/// How much of each stream to keep. Model context is finite, and a runaway build
/// log is worth truncating rather than dropping the whole result.
const MAX_STREAM_BYTES: usize = 32 * 1024;

/// Result of running a shell command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellOutcome {
    /// Exit status, or `None` if the process was killed by a signal.
    pub exit_code: Option<i32>,
    /// Captured stdout, possibly truncated.
    pub stdout: String,
    /// Captured stderr, possibly truncated.
    pub stderr: String,
    /// Whether either stream was truncated.
    pub truncated: bool,
}

impl ShellOutcome {
    /// Whether the command exited zero.
    pub fn success(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// Keep the tail of a stream: for build and test output the failure is at the end.
fn clamp(raw: Vec<u8>) -> (String, bool) {
    let text = String::from_utf8_lossy(&raw);
    if text.len() <= MAX_STREAM_BYTES {
        return (text.into_owned(), false);
    }

    let tail = &text[text.len() - MAX_STREAM_BYTES..];
    let start = tail.char_indices().next().map(|(i, _)| i).unwrap_or(0);
    (format!("[... truncated ...]\n{}", &tail[start..]), true)
}

/// Run `command` through `sh -c` in `root`.
pub async fn run(
    root: &Path,
    command: &str,
    requested_timeout: Option<Duration>,
) -> Result<ShellOutcome, ToolFailure> {
    if command.trim().is_empty() {
        return Err(ToolFailure::InvalidCommand {
            reason: "command is empty".to_string(),
        });
    }

    let limit = requested_timeout
        .unwrap_or(DEFAULT_TIMEOUT)
        .min(MAX_TIMEOUT);

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(root)
        .kill_on_drop(true);

    let child = cmd.output();

    let output = match timeout(limit, child).await {
        Err(_) => {
            return Err(ToolFailure::Timeout {
                command: command.to_string(),
                seconds: limit.as_secs(),
            })
        }
        Ok(Err(e)) => {
            return Err(ToolFailure::InvalidCommand {
                reason: format!("failed to spawn: {e}"),
            })
        }
        Ok(Ok(output)) => output,
    };

    let (stdout, out_trunc) = clamp(output.stdout);
    let (stderr, err_trunc) = clamp(output.stderr);

    Ok(ShellOutcome {
        exit_code: output.status.code(),
        stdout,
        stderr,
        truncated: out_trunc || err_trunc,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_root() -> PathBuf {
        let base = std::env::temp_dir().join(format!("serdes-shell-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&base).unwrap();
        base.canonicalize().unwrap()
    }

    #[tokio::test]
    async fn captures_stdout_and_exit_code() {
        let root = temp_root();
        let out = run(&root, "echo hello", None).await.unwrap();

        assert!(out.success());
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.stdout.trim(), "hello");
        assert!(!out.truncated);
    }

    #[tokio::test]
    async fn reports_a_failing_command_without_erroring() {
        // A non-zero exit is data the agent must see, not a tool failure.
        let root = temp_root();
        let out = run(&root, "exit 3", None).await.unwrap();

        assert!(!out.success());
        assert_eq!(out.exit_code, Some(3));
    }

    #[tokio::test]
    async fn captures_stderr() {
        let root = temp_root();
        let out = run(&root, "echo oops 1>&2", None).await.unwrap();

        assert_eq!(out.stderr.trim(), "oops");
    }

    #[tokio::test]
    async fn runs_in_the_workspace_root() {
        let root = temp_root();
        fs::write(root.join("marker.txt"), "x").unwrap();

        let out = run(&root, "ls", None).await.unwrap();

        assert!(out.stdout.contains("marker.txt"));
    }

    #[tokio::test]
    async fn enforces_the_timeout() {
        let root = temp_root();
        let err = run(&root, "sleep 5", Some(Duration::from_millis(150)))
            .await
            .unwrap_err();

        assert!(matches!(err, ToolFailure::Timeout { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn caps_an_over_long_timeout_request() {
        // Asking for a week must not disable the bound.
        let root = temp_root();
        let out = run(&root, "echo ok", Some(Duration::from_secs(999_999)))
            .await
            .unwrap();

        assert!(out.success());
    }

    #[tokio::test]
    async fn rejects_an_empty_command() {
        let root = temp_root();
        let err = run(&root, "   ", None).await.unwrap_err();

        assert!(matches!(err, ToolFailure::InvalidCommand { .. }));
    }

    #[tokio::test]
    async fn truncates_a_flood_of_output() {
        let root = temp_root();
        let out = run(&root, "yes abcdefgh | head -c 200000", None)
            .await
            .unwrap();

        assert!(out.truncated);
        assert!(out.stdout.starts_with("[... truncated ...]"));
        assert!(out.stdout.len() < 40 * 1024);
    }
}
