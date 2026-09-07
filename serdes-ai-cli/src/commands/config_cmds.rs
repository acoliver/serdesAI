//! Configuration commands - /show, /set, /reasoning, /verbosity

use crate::bus;
use crate::commands::registry::{CommandCategory, CommandResult};
use crate::config;
use crate::register_command;
use crate::tui::{interactive_colors_menu, interactive_model_settings};

pub fn init() {
    // /show command
    register_command!(
        name = "mode",
        description = "Show or set how a turn runs: single, fast or workflow",
        usage = "/mode [single|fast|workflow]",
        aliases = [],
        category = CommandCategory::Config,
        handler = handle_mode
    )
    .ok();

    register_command!(
        name = "show",
        description = "Show current configuration",
        usage = "/show",
        aliases = [],
        category = CommandCategory::Config,
        handler = handle_show
    )
    .ok();

    // /set command
    register_command!(
        name = "set",
        description = "Set configuration value (KEY=VALUE or KEY VALUE)",
        usage = "/set KEY=VALUE or /set KEY VALUE",
        aliases = [],
        category = CommandCategory::Config,
        handler = handle_set
    )
    .ok();

    // /reasoning command
    register_command!(
        name = "reasoning",
        description = "Set OpenAI reasoning effort (minimal|low|medium|high|xhigh)",
        usage = "/reasoning <level>",
        aliases = [],
        category = CommandCategory::Config,
        handler = handle_reasoning
    )
    .ok();

    // /verbosity command
    register_command!(
        name = "verbosity",
        description = "Set OpenAI verbosity (low|medium|high)",
        usage = "/verbosity <level>",
        aliases = [],
        category = CommandCategory::Config,
        handler = handle_verbosity
    )
    .ok();

    // /model_settings command
    register_command!(
        name = "model_settings",
        description = "Open interactive model settings editor",
        usage = "/model_settings [model]",
        aliases = [],
        category = CommandCategory::Config,
        handler = handle_model_settings
    )
    .ok();

    // /colors command
    register_command!(
        name = "colors",
        description = "Open interactive banner color editor",
        usage = "/colors",
        aliases = [],
        category = CommandCategory::Config,
        handler = handle_colors
    )
    .ok();

    // /diff command
    register_command!(
        name = "diff",
        description = "Configure diff display mode (compact/full)",
        usage = "/diff [compact|full]",
        aliases = [],
        category = CommandCategory::Config,
        handler = handle_diff
    )
    .ok();
}

/// Show or change how a turn runs.
///
/// Registered rather than intercepted before the registry, so it appears in the
/// completion list and in /help. While it was intercepted, typing it offered
/// /model as a completion instead, and choosing that replaced what had been
/// typed.
fn handle_mode(cmd: &str) -> CommandResult {
    let argument = command_arg(cmd);
    crate::runner::handle_mode_argument(argument);
    CommandResult::Handled
}

fn handle_show(_cmd: &str) -> CommandResult {
    let current_agent = config::get_agent_name();
    let default_agent = config::get_config_value("default_agent")
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(config::default_agent_name);
    let model = config::get_model_name();
    let yolo_mode = std::env::var("YOLO_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty() && !v.eq_ignore_ascii_case("false") && v != "0")
        .map(|_| "on")
        .unwrap_or("off");

    let strategy = match config::get_compaction_strategy() {
        config::CompactionStrategy::Summarization => "summarization",
        config::CompactionStrategy::Truncation => "truncation",
    };

    let reasoning = match config::get_openai_reasoning_effort() {
        config::ReasoningEffort::Minimal => "minimal",
        config::ReasoningEffort::Low => "low",
        config::ReasoningEffort::Medium => "medium",
        config::ReasoningEffort::High => "high",
        config::ReasoningEffort::Xhigh => "xhigh",
    };

    let verbosity = match config::get_openai_verbosity() {
        config::Verbosity::Low => "low",
        config::Verbosity::Medium => "medium",
        config::Verbosity::High => "high",
    };

    let cancel_key = match config::get_cancel_agent_key() {
        config::CancelKey::CtrlC => "ctrl_c",
        config::CancelKey::CtrlK => "ctrl_k",
        config::CancelKey::CtrlQ => "ctrl_q",
    };

    let auto_save = if config::get_autosave_enabled() {
        "enabled"
    } else {
        "disabled"
    };

    let temperature = match config::get_config_value("temperature") {
        Some(value) if !value.trim().is_empty() => value,
        _ => "(model default)".to_string(),
    };

    let protected_tokens = format_number(config::get_protected_token_count());
    let threshold_pct = format!("{:.0}%", config::get_compaction_threshold() * 100.0);

    bus::emit_info("Status".to_string());
    bus::emit_info("".to_string());
    bus::emit_info(format_status_line("current_agent", &current_agent));
    bus::emit_info(format_status_line("default_agent", &default_agent));
    bus::emit_info(format_status_line("model", &model));
    bus::emit_info(format_status_line("YOLO_MODE", yolo_mode));
    bus::emit_info(format_status_line("auto_save_session", auto_save));
    bus::emit_info(format_status_line("protected_tokens", &protected_tokens));
    bus::emit_info(format_status_line("compaction_threshold", &threshold_pct));
    bus::emit_info(format_status_line("compaction_strategy", strategy));
    bus::emit_info(format_status_line(
        "resume_message_count",
        &config::get_resume_message_count().to_string(),
    ));
    bus::emit_info(format_status_line("reasoning_effort", reasoning));
    bus::emit_info(format_status_line("verbosity", verbosity));
    bus::emit_info(format_status_line("temperature", &temperature));
    bus::emit_info(format_status_line("cancel_agent_key", cancel_key));

    CommandResult::Handled
}

fn handle_set(cmd: &str) -> CommandResult {
    let Some((key, value)) = parse_set_assignment(cmd) else {
        bus::emit_error("Usage: /set KEY=VALUE or /set KEY VALUE".to_string());
        return CommandResult::Handled;
    };

    let key_norm = key.trim().to_lowercase();
    if key_norm.is_empty() {
        bus::emit_error("Config key cannot be empty.".to_string());
        return CommandResult::Handled;
    }

    let value = value.trim();
    if value.is_empty() {
        bus::emit_error(format!("Value for '{}' cannot be empty.", key_norm));
        return CommandResult::Handled;
    }

    let result = match key_norm.as_str() {
        "cancel_agent_key" => parse_cancel_key(value)
            .map(config::set_cancel_agent_key)
            .map_err(|err| err.to_string()),
        "compaction_strategy" => parse_compaction_strategy(value)
            .map(config::set_compaction_strategy)
            .map_err(|err| err.to_string()),
        "protected_token_count" | "protected_tokens" => value
            .parse::<usize>()
            .map(|n| config::set_protected_token_count(n.max(1)))
            .map_err(|_| "protected_token_count must be a positive integer".to_string()),
        "compaction_threshold" => value
            .parse::<f32>()
            .map(|v| {
                if !(0.0..=1.0).contains(&v) {
                    Err("compaction_threshold must be between 0.0 and 1.0")
                } else {
                    config::set_compaction_threshold(v);
                    Ok(())
                }
            })
            .unwrap_or_else(|_| Err("compaction_threshold must be a float between 0.0 and 1.0"))
            .map_err(|err| err.to_string()),
        "resume_message_count" => value
            .parse::<usize>()
            .map(|n| config::set_resume_message_count(n.max(1)))
            .map_err(|_| "resume_message_count must be a positive integer".to_string()),
        "subagent_verbose" => parse_bool(value)
            .map(config::set_subagent_verbose)
            .map_err(|err| err.to_string()),
        _ => config::set_config_value(&key_norm, value).map_err(|err| err.to_string()),
    };

    match result {
        Ok(()) => {
            bus::emit_success(format!("Set {} = {}", key_norm, value));
        }
        Err(err) => {
            let available = config::get_config_keys().join(", ");
            bus::emit_error(format!(
                "Failed to set '{}': {}\nAvailable keys: {}",
                key_norm, err, available
            ));
        }
    }

    CommandResult::Handled
}

fn handle_reasoning(cmd: &str) -> CommandResult {
    let level = command_arg(cmd).to_lowercase();
    if level.is_empty() {
        bus::emit_error("Usage: /reasoning <minimal|low|medium|high|xhigh>".to_string());
        return CommandResult::Handled;
    }

    let effort = match level.as_str() {
        "minimal" => config::ReasoningEffort::Minimal,
        "low" => config::ReasoningEffort::Low,
        "medium" => config::ReasoningEffort::Medium,
        "high" => config::ReasoningEffort::High,
        "xhigh" => config::ReasoningEffort::Xhigh,
        _ => {
            bus::emit_error(format!(
                "Invalid reasoning level '{}'. Use: minimal|low|medium|high|xhigh",
                level
            ));
            return CommandResult::Handled;
        }
    };

    config::set_openai_reasoning_effort(effort);
    bus::emit_success(format!("Reasoning effort set to: {level}"));
    bus::emit_info("Agent settings updated (new value applies to subsequent turns).".to_string());

    CommandResult::Handled
}

fn handle_model_settings(cmd: &str) -> CommandResult {
    let arg = command_arg(cmd);
    let model = if arg.is_empty() {
        config::get_model_name()
    } else {
        arg.to_string()
    };

    let settings = config::get_model_settings(&model);

    match interactive_model_settings(&model, settings) {
        Ok(Some(new_settings)) => {
            config::update_model_settings(&model, new_settings);
            bus::emit_success(format!("Settings saved for {}", model));
        }
        Ok(None) => {
            bus::emit_info("Settings unchanged");
        }
        Err(e) => {
            bus::emit_error(format!("Settings editor failed: {}", e));
        }
    }

    CommandResult::Handled
}

fn handle_colors(_cmd: &str) -> CommandResult {
    match interactive_colors_menu() {
        Ok(true) => bus::emit_success("Colors saved"),
        Ok(false) => bus::emit_info("Colors unchanged"),
        Err(e) => bus::emit_error(format!("Colors menu failed: {}", e)),
    }

    CommandResult::Handled
}

fn handle_diff(cmd: &str) -> CommandResult {
    let arg = command_arg(cmd).to_lowercase();
    match arg.as_str() {
        "compact" => {
            config::set_compact_diffs(true);
            bus::emit_success("Diff display set to compact mode");
        }
        "full" => {
            config::set_compact_diffs(false);
            bus::emit_success("Diff display set to full mode");
        }
        _ => {
            let compact = config::get_compact_diffs();
            bus::emit_info(format!(
                "Diff mode: {}",
                if compact { "compact" } else { "full" }
            ));
        }
    }

    CommandResult::Handled
}

fn handle_verbosity(cmd: &str) -> CommandResult {
    let level = command_arg(cmd).to_lowercase();
    if level.is_empty() {
        bus::emit_error("Usage: /verbosity <low|medium|high>".to_string());
        return CommandResult::Handled;
    }

    let verbosity = match level.as_str() {
        "low" => config::Verbosity::Low,
        "medium" => config::Verbosity::Medium,
        "high" => config::Verbosity::High,
        _ => {
            bus::emit_error(format!(
                "Invalid verbosity level '{}'. Use: low|medium|high",
                level
            ));
            return CommandResult::Handled;
        }
    };

    config::set_openai_verbosity(verbosity);
    bus::emit_success(format!("Verbosity set to: {level}"));
    bus::emit_info("Agent settings updated (new value applies to subsequent turns).".to_string());

    CommandResult::Handled
}

fn command_arg(cmd: &str) -> &str {
    let trimmed = cmd.trim();
    let Some((_, rest)) = trimmed.split_once(char::is_whitespace) else {
        return "";
    };
    rest.trim()
}

fn parse_set_assignment(cmd: &str) -> Option<(String, String)> {
    let raw = command_arg(cmd);
    if raw.is_empty() {
        return None;
    }

    if let Some((k, v)) = raw.split_once('=') {
        return Some((k.trim().to_string(), v.trim().to_string()));
    }

    let mut parts = raw.split_whitespace();
    let key = parts.next()?.trim();
    let value = parts.collect::<Vec<_>>().join(" ");
    if value.trim().is_empty() {
        return None;
    }

    Some((key.to_string(), value.trim().to_string()))
}

fn parse_bool(value: &str) -> Result<bool, &'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err("must be true/false (also accepts 1/0, yes/no, on/off)"),
    }
}

fn parse_compaction_strategy(value: &str) -> Result<config::CompactionStrategy, &'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "summarization" => Ok(config::CompactionStrategy::Summarization),
        "truncation" => Ok(config::CompactionStrategy::Truncation),
        _ => Err("compaction_strategy must be summarization or truncation"),
    }
}

fn parse_cancel_key(value: &str) -> Result<config::CancelKey, &'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "ctrl_c" => Ok(config::CancelKey::CtrlC),
        "ctrl_k" => Ok(config::CancelKey::CtrlK),
        "ctrl_q" => Ok(config::CancelKey::CtrlQ),
        _ => Err("cancel_agent_key must be one of: ctrl_c, ctrl_k, ctrl_q"),
    }
}

fn format_status_line(label: &str, value: &str) -> String {
    format!("{label:<20} {value}")
}

fn format_number(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (idx, ch) in s.chars().rev().enumerate() {
        if idx != 0 && idx % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}
