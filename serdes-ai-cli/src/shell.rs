//! Shell command execution with live output streaming.

use std::process::Stdio;

use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::bus::{AnyMessage, MessageBus};
use crate::messages::{
    BaseMessage, MessageCategory, ShellLineMessage, ShellOutputMessage, ShellStartMessage,
};

/// Execute a shell command with live output streaming
pub async fn execute_shell_command(
    bus: &MessageBus,
    command: &str,
    cwd: Option<&str>,
) -> Result<ShellResult> {
    info!("Executing shell command: {}", command);

    let cwd = cwd.map(str::to_string).unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    });

    // Emit start message
    bus.emit(AnyMessage::ShellStart(ShellStartMessage {
        base: BaseMessage::new(MessageCategory::ToolOutput, None),
        command: command.to_string(),
        cwd: cwd.clone(),
    }));

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(&cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to spawn shell command: {}", command))?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    let command_id = uuid::Uuid::new_v4().to_string();

    // Channels for collecting output
    let (stdout_tx, mut stdout_rx) = mpsc::unbounded_channel::<String>();
    let (stderr_tx, mut stderr_rx) = mpsc::unbounded_channel::<String>();

    // Spawn stdout reader
    let stdout_handle = tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let _ = stdout_tx.send(line.clone());
        }
    });

    // Spawn stderr reader
    let stderr_handle = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();

        while let Ok(Some(line)) = lines.next_line().await {
            let _ = stderr_tx.send(line.clone());
        }
    });

    // Collect output and emit to bus
    let mut stdout_lines = Vec::new();
    let mut stderr_lines = Vec::new();

    loop {
        tokio::select! {
            Some(line) = stdout_rx.recv() => {
                debug!("stdout: {}", line);
                stdout_lines.push(line.clone());

                bus.emit(AnyMessage::ShellLine(ShellLineMessage {
                    base: BaseMessage::new(MessageCategory::ToolOutput, None),
                    command_id: command_id.clone(),
                    line,
                }));
            }
            Some(line) = stderr_rx.recv() => {
                debug!("stderr: {}", line);
                stderr_lines.push(line.clone());

                bus.emit(AnyMessage::ShellLine(ShellLineMessage {
                    base: BaseMessage::new(MessageCategory::ToolOutput, None),
                    command_id: command_id.clone(),
                    line: format!("[stderr] {}", line),
                }));
            }
            result = child.wait() => {
                let exit_code = result.ok().and_then(|s| s.code());
                let success = exit_code == Some(0);

                let output = format_output(&stdout_lines, &stderr_lines);

                bus.emit(AnyMessage::ShellOutput(ShellOutputMessage {
                    base: BaseMessage::new(MessageCategory::ToolOutput, None),
                    command_id: command_id.clone(),
                    output: output.clone(),
                    success,
                    exit_code,
                }));

                stdout_handle.abort();
                stderr_handle.abort();

                return Ok(ShellResult {
                    command: command.to_string(),
                    output,
                    stdout: stdout_lines.join("\n"),
                    stderr: stderr_lines.join("\n"),
                    exit_code,
                    success,
                });
            }
        }
    }
}

/// Result of shell command execution
#[derive(Debug, Clone)]
pub struct ShellResult {
    pub command: String,
    pub output: String,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub success: bool,
}

fn format_output(stdout: &[String], stderr: &[String]) -> String {
    let mut output = String::new();

    if !stdout.is_empty() {
        output.push_str(&stdout.join("\n"));
    }

    if !stderr.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str("[stderr]\n");
        output.push_str(&stderr.join("\n"));
    }

    output
}

/// Run a simple shell command (blocking, no streaming)
pub async fn run_simple_command(command: &str) -> Result<String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .await
        .with_context(|| format!("Failed to run command: {}", command))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        return Err(anyhow!(
            "Command failed with exit code {:?}: {}",
            output.status.code(),
            stderr
        ));
    }

    Ok(stdout.to_string())
}

/// Check if a command exists in PATH
pub async fn command_exists(command: &str) -> bool {
    Command::new("which")
        .arg(command)
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
}
