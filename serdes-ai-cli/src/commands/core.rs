//! Core commands: /help, /exit, /cd, /clear, /model, /agent, /session

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::bus;
use crate::commands::registry::{self, CommandCategory, CommandResult};
use crate::config;
use crate::models::loader;
use crate::picker::{pick_agent_inline, pick_model_inline, AgentInfo};
use crate::register_command;
use crate::runner::{validate_agent, validate_model};
use crate::session;
use crate::tui::ModelInfo;

// Register core commands
pub fn init() {
    register_command!(
        name = "help",
        description = "Show help message",
        usage = "/help",
        aliases = ["h"],
        category = CommandCategory::Core,
        handler = handle_help
    )
    .ok();

    register_command!(
        name = "exit",
        description = "Exit the application",
        usage = "/exit",
        aliases = ["quit"],
        category = CommandCategory::Core,
        handler = handle_exit
    )
    .ok();

    register_command!(
        name = "cd",
        description = "Change directory or list current directory contents",
        usage = "/cd [directory]",
        aliases = [],
        category = CommandCategory::Core,
        handler = handle_cd
    )
    .ok();

    register_command!(
        name = "clear",
        description = "Clear conversation (handled by runner)",
        usage = "/clear",
        aliases = [],
        category = CommandCategory::Core,
        handler = handle_clear
    )
    .ok();

    register_command!(
        name = "model",
        description = "Show or set active model",
        usage = "/model [name]",
        aliases = [],
        category = CommandCategory::Core,
        handler = handle_model
    )
    .ok();

    register_command!(
        name = "agent",
        description = "Show or set active agent",
        usage = "/agent [name]",
        aliases = [],
        category = CommandCategory::Core,
        handler = handle_agent
    )
    .ok();

    register_command!(
        name = "session",
        description = "Show current session details",
        usage = "/session",
        aliases = [],
        category = CommandCategory::Core,
        handler = handle_session
    )
    .ok();
}

fn handle_help(_cmd: &str) -> CommandResult {
    let help_text = registry::get_commands_help();
    bus::emit_info(help_text);
    CommandResult::Handled
}

fn handle_exit(_cmd: &str) -> CommandResult {
    CommandResult::Exit
}

fn handle_cd(cmd: &str) -> CommandResult {
    let raw_arg = command_arg(cmd);

    if raw_arg.is_empty() {
        match list_current_directory() {
            Ok(listing) => bus::emit_info(listing),
            Err(err) => bus::emit_error(format!("Failed to list current directory: {err}")),
        }
        return CommandResult::Handled;
    }

    let arg = trim_wrapping_quotes(raw_arg);
    let expanded = expand_tilde(arg);

    if let Err(err) = env::set_current_dir(&expanded) {
        bus::emit_error(format!(
            "Failed to change directory to '{}': {err}",
            expanded.display()
        ));
        return CommandResult::Handled;
    }

    match env::current_dir() {
        Ok(cwd) => bus::emit_success(format!("Changed directory to {}", cwd.display())),
        Err(err) => bus::emit_warning(format!(
            "Directory changed, but failed to resolve current directory: {err}"
        )),
    }

    CommandResult::Handled
}

fn handle_clear(_cmd: &str) -> CommandResult {
    // `/clear` is processed specially in runner for screen + session reset behavior.
    // We still register and handle it here so it appears in /help output.
    CommandResult::Handled
}

fn handle_model(cmd: &str) -> CommandResult {
    let arg = command_arg(cmd);

    if arg.is_empty() {
        let models = get_available_models_with_info();
        let model_names: Vec<String> = models.into_iter().map(|m| m.name).collect();
        let current_model = config::get_model_name();

        match pick_model_inline(&model_names, Some(&current_model)) {
            Ok(Some(selected)) => {
                config::set_model_name(&selected);
                bus::emit_success(format!("Model set to: {selected}"));
            }
            Ok(None) => {
                bus::emit_info("Model selection cancelled");
            }
            Err(err) => {
                bus::emit_error(format!("Picker failed: {err}"));
            }
        }

        return CommandResult::Handled;
    }

    let next_model = trim_wrapping_quotes(arg);
    match validate_model(next_model) {
        Ok(()) => {
            config::set_model_name(next_model);
            bus::emit_success(format!("Model set to: {next_model}"));
        }
        Err(err) => {
            bus::emit_error(format!("Invalid model '{}': {err}", next_model));
        }
    }

    CommandResult::Handled
}

fn handle_agent(cmd: &str) -> CommandResult {
    let arg = command_arg(cmd);

    if arg.is_empty() {
        let agents = get_available_agents_with_info();
        let current_agent = config::get_agent_name();

        match pick_agent_inline(&agents, Some(&current_agent)) {
            Ok(Some(selected)) => {
                config::set_agent_name(&selected);
                let new_session_id = session::finalize_autosave_session();
                bus::emit_success(format!("Switched to agent: {selected}"));
                bus::emit_info(format!("Auto-save session rotated to: {new_session_id}"));
            }
            Ok(None) => {
                bus::emit_info("Agent selection cancelled");
            }
            Err(err) => {
                bus::emit_error(format!("Picker failed: {err}"));
            }
        }

        return CommandResult::Handled;
    }

    let next_agent = trim_wrapping_quotes(arg);
    match validate_agent(next_agent) {
        Ok(()) => {
            config::set_agent_name(next_agent);
            bus::emit_success(format!("Agent set to: {next_agent}"));
        }
        Err(err) => {
            bus::emit_error(format!("Invalid agent '{}': {err}", next_agent));
        }
    }

    CommandResult::Handled
}

fn handle_session(_cmd: &str) -> CommandResult {
    let Some(session_id) = session::get_current_session_id() else {
        bus::emit_info("No active session.".to_string());
        return CommandResult::Handled;
    };

    bus::emit_info(format!("Current session: {session_id}"));

    let autosave_dir = config::get_autosave_dir();
    match session::load_session(&session_id, &autosave_dir) {
        Ok(loaded) => {
            bus::emit_info(format!("Message count: {}", loaded.messages.len()));
        }
        Err(err) => {
            bus::emit_warning(format!(
                "Session metadata unavailable for '{}': {err}",
                session_id
            ));
        }
    }

    CommandResult::Handled
}

fn command_arg(cmd: &str) -> &str {
    let trimmed = cmd.trim();
    let Some((_, rest)) = trimmed.split_once(char::is_whitespace) else {
        return "";
    };

    rest.trim()
}

fn trim_wrapping_quotes(input: &str) -> &str {
    let bytes = input.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &input[1..input.len() - 1];
        }
    }

    input
}

fn expand_tilde(raw_path: &str) -> PathBuf {
    if !raw_path.starts_with('~') {
        return PathBuf::from(raw_path);
    }

    let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE"));
    let Some(home) = home else {
        return PathBuf::from(raw_path);
    };

    let home = PathBuf::from(home);
    if raw_path == "~" {
        return home;
    }

    if let Some(stripped) = raw_path.strip_prefix("~/") {
        return home.join(stripped);
    }

    if let Some(stripped) = raw_path.strip_prefix("~\\") {
        return home.join(stripped);
    }

    PathBuf::from(raw_path)
}

fn list_current_directory() -> Result<String, String> {
    let cwd = env::current_dir().map_err(|err| err.to_string())?;
    let mut entries = fs::read_dir(&cwd)
        .map_err(|err| err.to_string())?
        .filter_map(Result::ok)
        .collect::<Vec<_>>();

    entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_string());

    let mut lines = Vec::with_capacity(entries.len() + 1);
    lines.push(format!("Current directory: {}", cwd.display()));

    if entries.is_empty() {
        lines.push("(empty directory)".to_string());
    } else {
        for entry in entries {
            let path = entry.path();
            let name = file_display_name(&path, &entry.file_name());
            lines.push(format!("- {name}"));
        }
    }

    Ok(lines.join("\n"))
}

fn file_display_name(path: &Path, fallback_name: &std::ffi::OsStr) -> String {
    let mut name = fallback_name.to_string_lossy().to_string();

    if path.is_dir() {
        name.push('/');
    }

    name
}

fn get_available_agents_with_info() -> Vec<AgentInfo> {
    let candidates = [
        (
            "code-puppy",
            "Sassy coding assistant with tools and terminal workflow",
        ),
        ("default", "General-purpose helpful assistant"),
    ];

    candidates
        .into_iter()
        .map(|(name, description)| AgentInfo {
            name: name.to_string(),
            description: description.to_string(),
        })
        .collect()
}

fn get_available_models_with_info() -> Vec<ModelInfo> {
    let current_model = config::get_model_name();
    let current_agent = config::get_agent_name();
    let pinned_model = config::get_pinned_model(&current_agent);

    loader::available_models()
        .into_iter()
        .map(|name| {
            let provider = provider_from_model_spec(&name);
            ModelInfo {
                name: name.clone(),
                provider: provider.to_string(),
                description: model_description(&name),
                is_current: current_model.eq_ignore_ascii_case(&name),
                is_pinned: pinned_model
                    .as_deref()
                    .map(|p| p.eq_ignore_ascii_case(&name))
                    .unwrap_or(false),
            }
        })
        .collect()
}

fn provider_from_model_spec(spec: &str) -> &'static str {
    match spec
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "openai" | "gpt" => "OpenAI",
        "anthropic" | "claude" => "Anthropic",
        "google" | "gemini" => "Google",
        "mistral" => "Mistral",
        "groq" => "Groq",
        "bedrock" | "aws" => "Bedrock",
        "openrouter" | "or" => "OpenRouter",
        "cohere" | "co" => "Cohere",
        "huggingface" | "hf" => "Hugging Face",
        "ollama" => "Ollama",
        _ => "Other",
    }
}

fn model_description(spec: &str) -> String {
    if let Some((_, model_name)) = spec.split_once(':') {
        format!("{} model", model_name)
    } else {
        "Model".to_string()
    }
}
