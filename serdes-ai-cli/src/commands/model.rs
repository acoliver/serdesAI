//! Model pinning commands: /pin_model, /unpin

use crate::bus;
use crate::commands::registry::{CommandCategory, CommandResult};
use crate::config;
use crate::register_command;
use crate::runner::{validate_agent, validate_model};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::Mutex;

// In-memory store for pinned models (until config persistence is set up)
static PINNED_MODELS: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub fn init() {
    hydrate_cache_from_config();

    // /pin_model <agent> <model>
    register_command!(
        name = "pin_model",
        description = "Pin a specific model to an agent",
        usage = "/pin_model <agent> <model>",
        aliases = [],
        category = CommandCategory::Config,
        handler = handle_pin_model
    )
    .ok();

    // /unpin <agent>
    register_command!(
        name = "unpin",
        description = "Unpin model from agent",
        usage = "/unpin <agent>",
        aliases = [],
        category = CommandCategory::Config,
        handler = handle_unpin
    )
    .ok();
}

fn handle_pin_model(cmd: &str) -> CommandResult {
    // Parse: /pin_model <agent> <model>
    let Some((agent, model)) = parse_pin_args(cmd) else {
        bus::emit_error("Usage: /pin_model <agent> <model>".to_string());
        return CommandResult::Handled;
    };

    // Validate agent exists
    if let Err(err) = validate_agent(&agent) {
        bus::emit_error(format!("Invalid agent '{agent}': {err}"));
        return CommandResult::Handled;
    }

    // Validate model exists
    if let Err(err) = validate_model(&model) {
        bus::emit_error(format!("Invalid model '{model}': {err}"));
        return CommandResult::Handled;
    }

    let normalized_agent = agent.to_ascii_lowercase();

    // Store in PINNED_MODELS
    {
        let mut cache = PINNED_MODELS
            .lock()
            .expect("pinned model cache lock poisoned while pinning model");
        cache.insert(normalized_agent.clone(), model.clone());
    }

    // Update config persistence + compatibility map
    config::set_pinned_model(&normalized_agent, &model);
    config::set_model_name(&model);

    bus::emit_success(format!(
        "Pinned model '{model}' to agent '{normalized_agent}'"
    ));
    CommandResult::Handled
}

fn handle_unpin(cmd: &str) -> CommandResult {
    // Parse: /unpin <agent>
    let Some(agent) = parse_unpin_arg(cmd) else {
        bus::emit_error("Usage: /unpin <agent>".to_string());
        return CommandResult::Handled;
    };

    let normalized_agent = agent.to_ascii_lowercase();

    let removed = {
        let mut cache = PINNED_MODELS
            .lock()
            .expect("pinned model cache lock poisoned while unpinning model");
        cache.remove(&normalized_agent)
    };

    // Clear from config persistence
    config::unpin_model(&normalized_agent);

    // Emit success or warning if not pinned
    if let Some(model) = removed {
        bus::emit_success(format!(
            "Unpinned model '{model}' from agent '{normalized_agent}'"
        ));
    } else {
        bus::emit_warning(format!("Agent '{normalized_agent}' has no pinned model."));
    }

    CommandResult::Handled
}

fn hydrate_cache_from_config() {
    let persisted = config::get_all_pinned_models();
    let mut cache = PINNED_MODELS
        .lock()
        .expect("pinned model cache lock poisoned during hydration");
    cache.clear();

    for (agent, model) in persisted {
        cache.insert(agent, model);
    }
}

fn parse_pin_args(cmd: &str) -> Option<(String, String)> {
    let mut parts = cmd.split_whitespace();
    let _command = parts.next()?;
    let agent = parts.next()?.trim().to_string();
    let model = parts.collect::<Vec<_>>().join(" ").trim().to_string();

    if agent.is_empty() || model.is_empty() {
        return None;
    }

    Some((agent, model))
}

fn parse_unpin_arg(cmd: &str) -> Option<String> {
    let mut parts = cmd.split_whitespace();
    let _command = parts.next()?;
    let agent = parts.next()?.trim().to_string();
    if agent.is_empty() {
        return None;
    }
    Some(agent)
}

// Helper functions
pub fn get_pinned_model(agent: &str) -> Option<String> {
    PINNED_MODELS
        .lock()
        .expect("pinned model cache lock poisoned during get")
        .get(&agent.to_ascii_lowercase())
        .cloned()
}

pub fn is_model_pinned(agent: &str) -> bool {
    PINNED_MODELS
        .lock()
        .expect("pinned model cache lock poisoned during contains check")
        .contains_key(&agent.to_ascii_lowercase())
}

pub fn get_all_pinned() -> Vec<(String, String)> {
    PINNED_MODELS
        .lock()
        .expect("pinned model cache lock poisoned during collect")
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}
