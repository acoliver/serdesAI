//! API server management commands: /api start|stop|status

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::bus;
use crate::commands::registry::{CommandCategory, CommandResult};
use crate::config;
use crate::register_command;

const API_PORT: u16 = 8765;
const PID_FILE: &str = "api_server.pid";

pub fn init() {
    register_command!(
        name = "api",
        description = "Manage the API server for GUI integration",
        usage = "/api [start|stop|status]",
        aliases = [],
        category = CommandCategory::Core,
        handler = handle_api
    )
    .ok();
}

fn handle_api(cmd: &str) -> CommandResult {
    let args: Vec<&str> = cmd.split_whitespace().skip(1).collect();

    let subcommand = if args.is_empty() { "status" } else { args[0] };

    match subcommand {
        "start" => handle_api_start(),
        "stop" => handle_api_stop(),
        "status" => handle_api_status(),
        _ => {
            bus::emit_error(format!("Unknown subcommand: {subcommand}"));
            bus::emit_info("Usage: /api [start|stop|status]");
            CommandResult::Handled
        }
    }
}

fn handle_api_start() -> CommandResult {
    let pid_file = get_pid_file();

    // Check if already running
    if pid_file.exists() {
        if let Ok(pid_str) = fs::read_to_string(&pid_file) {
            if let Ok(pid) = pid_str.trim().parse::<u32>() {
                if is_process_running(pid) {
                    bus::emit_info(format!("API server already running (PID {pid})"));
                    bus::emit_info(format!("URL: http://127.0.0.1:{API_PORT}"));
                    return CommandResult::Handled;
                }
            }
        }

        // Stale PID file, remove it
        let _ = fs::remove_file(&pid_file);
    }

    // Start the server
    bus::emit_info(format!(
        "Starting API server on http://127.0.0.1:{API_PORT} ..."
    ));

    match start_api_server() {
        Ok(child) => {
            let pid = child.id();

            // Write PID file
            if let Err(e) = fs::write(&pid_file, pid.to_string()) {
                bus::emit_warning(format!("Failed to write PID file: {e}"));
            }

            bus::emit_success(format!("API server started (PID {pid})"));
            bus::emit_info(format!("URL: http://127.0.0.1:{API_PORT}"));
            bus::emit_info(format!("Documentation: http://127.0.0.1:{API_PORT}/docs"));
        }
        Err(e) => {
            bus::emit_error(format!("Failed to start API server: {e}"));
        }
    }

    CommandResult::Handled
}

fn handle_api_stop() -> CommandResult {
    let pid_file = get_pid_file();

    if !pid_file.exists() {
        bus::emit_info("API server is not running");
        return CommandResult::Handled;
    }

    let pid_str = match fs::read_to_string(&pid_file) {
        Ok(s) => s,
        Err(e) => {
            bus::emit_error(format!("Failed to read PID file: {e}"));
            return CommandResult::Handled;
        }
    };

    let pid = match pid_str.trim().parse::<u32>() {
        Ok(p) => p,
        Err(e) => {
            bus::emit_error(format!("Invalid PID in file: {e}"));
            let _ = fs::remove_file(&pid_file);
            return CommandResult::Handled;
        }
    };

    // Try to kill the process
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
    }

    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
    }

    // Remove PID file
    let _ = fs::remove_file(&pid_file);

    bus::emit_success(format!("API server stopped (PID {pid})"));
    CommandResult::Handled
}

fn handle_api_status() -> CommandResult {
    let pid_file = get_pid_file();

    if !pid_file.exists() {
        bus::emit_info("API server is not running");
        return CommandResult::Handled;
    }

    let pid_str = match fs::read_to_string(&pid_file) {
        Ok(s) => s,
        Err(e) => {
            bus::emit_error(format!("Failed to read PID file: {e}"));
            return CommandResult::Handled;
        }
    };

    let pid = match pid_str.trim().parse::<u32>() {
        Ok(p) => p,
        Err(_) => {
            bus::emit_info("API server is not running (stale PID file removed)");
            let _ = fs::remove_file(&pid_file);
            return CommandResult::Handled;
        }
    };

    if is_process_running(pid) {
        bus::emit_success(format!("API server is running (PID {pid})"));
        bus::emit_info(format!("URL: http://127.0.0.1:{API_PORT}"));
        bus::emit_info(format!("Documentation: http://127.0.0.1:{API_PORT}/docs"));
    } else {
        bus::emit_info("API server is not running (stale PID file removed)");
        let _ = fs::remove_file(&pid_file);
    }

    CommandResult::Handled
}

fn get_pid_file() -> PathBuf {
    let state_dir = config::get_state_dir();
    std::fs::create_dir_all(&state_dir).ok();
    state_dir.join(PID_FILE)
}

fn is_process_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // `kill -0` checks existence without sending a signal.
        match Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(status) => status.success(),
            Err(_) => false,
        }
    }

    #[cfg(windows)]
    {
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output();

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                stdout.contains(&pid.to_string())
            }
            Err(_) => false,
        }
    }
}

fn start_api_server() -> anyhow::Result<Child> {
    // In a real implementation, this would start the actual API server binary.
    // For now, it's a placeholder that would need the actual server implementation.
    Err(anyhow::anyhow!(
        "API server not yet implemented. This is a placeholder for the GUI integration server."
    ))
}
