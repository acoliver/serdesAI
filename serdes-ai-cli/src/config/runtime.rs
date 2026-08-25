use std::collections::HashMap;
use std::sync::RwLock;

use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;

use super::{
    default_agent, default_max_sessions, default_model, CancelKey, ColorsConfig,
    CompactionStrategy, Config, ModelSettings, ReasoningEffort, Verbosity,
};

static RUNTIME_CONFIG: Lazy<RwLock<Config>> = Lazy::new(|| {
    let cfg = Config::load().unwrap_or_else(|_| Config::default());
    RwLock::new(cfg)
});

fn with_read<T>(f: impl FnOnce(&Config) -> T) -> T {
    let guard = RUNTIME_CONFIG
        .read()
        .expect("runtime config lock poisoned for read");
    f(&guard)
}

fn with_write<T>(f: impl FnOnce(&mut Config) -> T) -> T {
    let mut guard = RUNTIME_CONFIG
        .write()
        .expect("runtime config lock poisoned for write");
    let out = f(&mut guard);
    guard.sync_legacy_fields();
    let _ = guard.save();
    out
}

pub fn get_model_name() -> String {
    with_read(|cfg| cfg.model.clone().unwrap_or_else(default_model))
}

pub fn set_model_name(model: &str) {
    with_write(|cfg| {
        let model = model.trim();
        if model.is_empty() {
            cfg.model = Some(default_model());
        } else {
            cfg.model = Some(model.to_string());
        }
    });
}

pub fn get_agent_name() -> String {
    // Mapped on read, so a settings file naming the agent by its previous name
    // resolves to the current one rather than failing validation.
    with_read(|cfg| {
        cfg.agent
            .as_deref()
            .map(super::canonical_agent_name)
            .unwrap_or_else(default_agent)
    })
}

pub fn set_agent_name(agent: &str) {
    with_write(|cfg| {
        let agent = agent.trim();
        if agent.is_empty() {
            cfg.agent = Some(default_agent());
        } else {
            cfg.agent = Some(agent.to_string());
        }
    });
}

pub fn get_api_key(name: &str) -> Option<String> {
    let key = name.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }

    with_read(|cfg| {
        cfg.api_keys
            .as_ref()
            .and_then(|keys| keys.get(&key).cloned())
    })
}

/// The configured endpoint override for `provider`, if any.
pub fn get_base_url(provider: &str) -> Option<String> {
    let key = provider.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }

    with_read(|cfg| {
        cfg.base_urls
            .as_ref()
            .and_then(|urls| urls.get(&key).cloned())
    })
}

/// Point `provider` at `url`. An empty url clears the override.
pub fn set_base_url(provider: &str, url: &str) {
    let key = provider.trim().to_lowercase();
    let url = url.trim();
    if key.is_empty() {
        return;
    }

    with_write(|cfg| {
        let urls = cfg.base_urls.get_or_insert_with(Default::default);
        if url.is_empty() {
            urls.remove(&key);
        } else {
            urls.insert(key.clone(), url.to_string());
        }
    });
}

pub fn set_api_key(name: &str, value: &str) {
    let key = name.trim().to_lowercase();
    let value = value.trim();
    if key.is_empty() {
        return;
    }

    with_write(|cfg| {
        let map = cfg.api_keys.get_or_insert_with(HashMap::new);
        if value.is_empty() {
            map.remove(&key);
            if map.is_empty() {
                cfg.api_keys = None;
            }
        } else {
            map.insert(key, value.to_string());
        }
    });
}

pub fn load_api_keys_to_environment() -> Result<()> {
    let entries = with_read(|cfg| cfg.api_keys.clone().unwrap_or_default());

    for (name, value) in entries {
        super::set_env_if_missing(&name.to_uppercase(), Some(&value));
    }

    Ok(())
}

pub fn get_request_timeout() -> u64 {
    with_read(|cfg| {
        cfg.request_timeout_secs
            .unwrap_or(120)
            .max(1)
            .try_into()
            .unwrap_or(120)
    })
}

pub fn get_temperature() -> f64 {
    with_read(|cfg| cfg.temperature.unwrap_or(0.7))
}

pub fn get_max_tokens() -> i64 {
    with_read(|cfg| cfg.max_tokens.unwrap_or(0).max(0))
}

pub fn get_autosave_enabled() -> bool {
    with_read(|cfg| cfg.autosave_enabled)
}

pub fn get_autosave_max_sessions() -> usize {
    with_read(|cfg| {
        cfg.max_autosave_sessions
            .unwrap_or(default_max_sessions() as i64)
            .max(1) as usize
    })
}

pub fn get_current_session_id() -> Option<String> {
    with_read(|cfg| cfg.autosave.current_session.clone())
}

pub fn set_current_session_id(id: String) {
    with_write(|cfg| {
        let trimmed = id.trim();
        cfg.autosave.current_session = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    });
}

pub fn get_compact_diffs() -> bool {
    with_read(|cfg| cfg.compact_diffs)
}

pub fn set_compact_diffs(compact: bool) {
    with_write(|cfg| {
        cfg.compact_diffs = compact;
    });
}

pub fn is_onboarding_complete() -> bool {
    with_read(|cfg| cfg.onboarding_complete)
}

pub fn set_onboarding_complete(complete: bool) {
    with_write(|cfg| {
        cfg.onboarding_complete = complete;
    });
}

pub fn get_universal_constructor_enabled() -> bool {
    with_read(|cfg| cfg.universal_constructor_enabled)
}

pub fn set_universal_constructor_enabled(enabled: bool) {
    with_write(|cfg| {
        cfg.universal_constructor_enabled = enabled;
    });
}

pub fn get_banner_color(name: &str) -> String {
    with_read(|cfg| banner_color_from(&cfg.colors, name))
}

pub fn get_colors_config() -> ColorsConfig {
    with_read(|cfg| cfg.colors.clone())
}

pub fn set_banner_color(name: &str, color: &str) {
    let key = name.trim().to_lowercase();
    let value = color.trim();
    if key.is_empty() || value.is_empty() {
        return;
    }

    with_write(|cfg| match key.as_str() {
        "thinking" | "reasoning" | "banner_thinking" => {
            cfg.colors.banner_thinking = value.to_string();
        }
        "shell_command" | "banner_shell_command" => {
            cfg.colors.banner_shell_command = value.to_string();
        }
        "edit_file" | "banner_edit_file" => {
            cfg.colors.banner_edit_file = value.to_string();
        }
        "directory_listing" | "banner_directory_listing" => {
            cfg.colors.banner_directory_listing = value.to_string();
        }
        "grep" | "banner_grep" => {
            cfg.colors.banner_grep = value.to_string();
        }
        _ => {}
    });
}

fn banner_color_from(colors: &ColorsConfig, name: &str) -> String {
    match name.trim().to_lowercase().as_str() {
        "thinking" | "reasoning" | "banner_thinking" => colors.banner_thinking.clone(),
        "shell_command" | "banner_shell_command" => colors.banner_shell_command.clone(),
        "edit_file" | "banner_edit_file" => colors.banner_edit_file.clone(),
        "directory_listing" | "banner_directory_listing" => colors.banner_directory_listing.clone(),
        "grep" | "banner_grep" => colors.banner_grep.clone(),
        "info" => colors.banner_thinking.clone(),
        "tool_output" => colors.banner_edit_file.clone(),
        _ => colors.banner_thinking.clone(),
    }
}

pub fn get_compaction_strategy() -> CompactionStrategy {
    with_read(|cfg| cfg.compaction_strategy.clone())
}

pub fn get_protected_token_count() -> usize {
    with_read(|cfg| cfg.protected_token_count.max(1))
}

pub fn get_compaction_threshold() -> f32 {
    with_read(|cfg| cfg.compaction_threshold.clamp(0.0, 1.0))
}

pub fn get_resume_message_count() -> usize {
    with_read(|cfg| cfg.resume_message_count.max(1))
}

pub fn get_openai_reasoning_effort() -> ReasoningEffort {
    with_read(|cfg| cfg.openai_reasoning_effort.clone())
}

pub fn get_openai_verbosity() -> Verbosity {
    with_read(|cfg| cfg.openai_verbosity.clone())
}

pub fn get_cancel_agent_key() -> CancelKey {
    with_read(|cfg| cfg.cancel_agent_key.clone())
}

pub fn get_use_dbos() -> bool {
    with_read(|cfg| cfg.enable_dbos)
}

pub fn get_subagent_verbose() -> bool {
    with_read(|cfg| cfg.subagent_verbose)
}

pub fn get_model_settings(model: &str) -> ModelSettings {
    let key = model.trim().to_lowercase();
    if key.is_empty() {
        return ModelSettings::default();
    }

    with_read(|cfg| {
        cfg.model_settings
            .get(&key)
            .cloned()
            .unwrap_or_else(ModelSettings::default)
    })
}

pub fn get_pinned_model(agent: &str) -> Option<String> {
    let key = agent.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }

    with_read(|cfg| cfg.pinned_models.get(&key).cloned())
}

pub fn set_pinned_model(agent: &str, model: &str) {
    let agent_key = agent.trim().to_lowercase();
    let model_value = model.trim();
    if agent_key.is_empty() || model_value.is_empty() {
        return;
    }

    with_write(|cfg| {
        cfg.pinned_models.insert(agent_key, model_value.to_string());
    });
}

pub fn unpin_model(agent: &str) {
    let key = agent.trim().to_lowercase();
    if key.is_empty() {
        return;
    }

    with_write(|cfg| {
        cfg.pinned_models.remove(&key);
    });
}

pub fn get_all_pinned_models() -> Vec<(String, String)> {
    with_read(|cfg| {
        cfg.pinned_models
            .iter()
            .map(|(agent, model)| (agent.clone(), model.clone()))
            .collect()
    })
}

pub fn set_compaction_strategy(strategy: CompactionStrategy) {
    with_write(|cfg| {
        cfg.compaction_strategy = strategy;
    });
}

pub fn set_protected_token_count(count: usize) {
    with_write(|cfg| {
        cfg.protected_token_count = count.max(1);
    });
}

pub fn set_compaction_threshold(threshold: f32) {
    with_write(|cfg| {
        cfg.compaction_threshold = threshold.clamp(0.0, 1.0);
    });
}

pub fn set_resume_message_count(count: usize) {
    with_write(|cfg| {
        cfg.resume_message_count = count.max(1);
    });
}

pub fn set_openai_reasoning_effort(effort: ReasoningEffort) {
    with_write(|cfg| {
        cfg.openai_reasoning_effort = effort;
    });
}

pub fn set_openai_verbosity(verbosity: Verbosity) {
    with_write(|cfg| {
        cfg.openai_verbosity = verbosity;
    });
}

pub fn set_cancel_agent_key(key: CancelKey) {
    with_write(|cfg| {
        cfg.cancel_agent_key = key;
    });
}

pub fn set_enable_dbos(enabled: bool) {
    with_write(|cfg| {
        cfg.enable_dbos = enabled;
    });
}

pub fn set_subagent_verbose(enabled: bool) {
    with_write(|cfg| {
        cfg.subagent_verbose = enabled;
    });
}

fn with_model_settings_mut(model: &str, f: impl FnOnce(&mut ModelSettings)) {
    let key = model.trim().to_lowercase();
    if key.is_empty() {
        return;
    }

    with_write(|cfg| {
        let settings = cfg.model_settings.entry(key).or_default();
        f(settings);
    });
}

pub fn set_model_temperature(model: &str, temp: f32) {
    with_model_settings_mut(model, |settings| settings.temperature = Some(temp));
}

pub fn set_model_seed(model: &str, seed: i32) {
    with_model_settings_mut(model, |settings| settings.seed = Some(seed));
}

pub fn set_model_top_p(model: &str, top_p: f32) {
    with_model_settings_mut(model, |settings| settings.top_p = Some(top_p));
}

pub fn set_model_max_tokens(model: &str, max: usize) {
    with_model_settings_mut(model, |settings| settings.max_tokens = Some(max));
}

pub fn update_model_settings(model: &str, settings: ModelSettings) {
    let key = model.trim().to_lowercase();
    if key.is_empty() {
        return;
    }

    with_write(|cfg| {
        if settings.temperature.is_none()
            && settings.seed.is_none()
            && settings.top_p.is_none()
            && settings.max_tokens.is_none()
        {
            cfg.model_settings.remove(&key);
        } else {
            cfg.model_settings.insert(key, settings);
        }
    });
}

pub fn get_config_keys() -> Vec<String> {
    vec![
        "model".to_string(),
        "agent".to_string(),
        "compaction_strategy".to_string(),
        "protected_token_count".to_string(),
        "compaction_threshold".to_string(),
        "resume_message_count".to_string(),
        "openai_reasoning_effort".to_string(),
        "openai_verbosity".to_string(),
        "cancel_agent_key".to_string(),
        "enable_dbos".to_string(),
        "subagent_verbose".to_string(),
        "request_timeout_secs".to_string(),
        "temperature".to_string(),
        "max_tokens".to_string(),
        "autosave_enabled".to_string(),
        "max_autosave_sessions".to_string(),
        "universal_constructor_enabled".to_string(),
        "compact_diffs".to_string(),
        "onboarding_complete".to_string(),
    ]
}

pub fn get_config_value(key: &str) -> Option<String> {
    let key = key.trim().to_lowercase();
    if key.is_empty() {
        return None;
    }

    with_read(|cfg| match key.as_str() {
        "model" => cfg.model.clone(),
        "agent" => cfg.agent.clone(),
        "compaction_strategy" => serde_json::to_string(&cfg.compaction_strategy).ok(),
        "protected_token_count" => Some(cfg.protected_token_count.to_string()),
        "compaction_threshold" => Some(cfg.compaction_threshold.to_string()),
        "resume_message_count" => Some(cfg.resume_message_count.to_string()),
        "openai_reasoning_effort" => serde_json::to_string(&cfg.openai_reasoning_effort).ok(),
        "openai_verbosity" => serde_json::to_string(&cfg.openai_verbosity).ok(),
        "cancel_agent_key" => serde_json::to_string(&cfg.cancel_agent_key).ok(),
        "enable_dbos" => Some(cfg.enable_dbos.to_string()),
        "subagent_verbose" => Some(cfg.subagent_verbose.to_string()),
        "request_timeout_secs" => cfg.request_timeout_secs.map(|v| v.to_string()),
        "temperature" => cfg.temperature.map(|v| v.to_string()),
        "max_tokens" => cfg.max_tokens.map(|v| v.to_string()),
        "autosave_enabled" => Some(cfg.autosave_enabled.to_string()),
        "max_autosave_sessions" => cfg.max_autosave_sessions.map(|v| v.to_string()),
        "universal_constructor_enabled" => Some(cfg.universal_constructor_enabled.to_string()),
        "compact_diffs" => Some(cfg.compact_diffs.to_string()),
        "onboarding_complete" => Some(cfg.onboarding_complete.to_string()),
        _ => None,
    })
    .map(|v| v.trim_matches('"').to_string())
}

pub fn set_config_value(key: &str, value: &str) -> Result<()> {
    let key = key.trim().to_lowercase();
    let value = value.trim();
    if key.is_empty() {
        return Err(anyhow!("config key cannot be empty"));
    }

    with_write(|cfg| -> Result<()> {
        match key.as_str() {
            "model" => cfg.model = Some(value.to_string()),
            "agent" => cfg.agent = Some(value.to_string()),
            "compaction_strategy" => {
                cfg.compaction_strategy = match value {
                    "summarization" => CompactionStrategy::Summarization,
                    "truncation" => CompactionStrategy::Truncation,
                    _ => {
                        return Err(anyhow!(
                            "invalid compaction_strategy '{}', expected summarization|truncation",
                            value
                        ));
                    }
                }
            }
            "protected_token_count" => {
                cfg.protected_token_count = value
                    .parse::<usize>()
                    .map_err(|_| anyhow!("protected_token_count must be a positive integer"))?
                    .max(1)
            }
            "compaction_threshold" => {
                cfg.compaction_threshold = value
                    .parse::<f32>()
                    .map_err(|_| anyhow!("compaction_threshold must be a float"))?
                    .clamp(0.0, 1.0)
            }
            "resume_message_count" => {
                cfg.resume_message_count = value
                    .parse::<usize>()
                    .map_err(|_| anyhow!("resume_message_count must be a positive integer"))?
                    .max(1)
            }
            "openai_reasoning_effort" => {
                cfg.openai_reasoning_effort = match value {
                    "minimal" => ReasoningEffort::Minimal,
                    "low" => ReasoningEffort::Low,
                    "medium" => ReasoningEffort::Medium,
                    "high" => ReasoningEffort::High,
                    "xhigh" => ReasoningEffort::Xhigh,
                    _ => {
                        return Err(anyhow!(
                            "invalid openai_reasoning_effort '{}', expected minimal|low|medium|high|xhigh",
                            value
                        ));
                    }
                }
            }
            "openai_verbosity" => {
                cfg.openai_verbosity = match value {
                    "low" => Verbosity::Low,
                    "medium" => Verbosity::Medium,
                    "high" => Verbosity::High,
                    _ => {
                        return Err(anyhow!(
                            "invalid openai_verbosity '{}', expected low|medium|high",
                            value
                        ));
                    }
                }
            }
            "cancel_agent_key" => {
                cfg.cancel_agent_key = match value {
                    "ctrl_c" => CancelKey::CtrlC,
                    "ctrl_k" => CancelKey::CtrlK,
                    "ctrl_q" => CancelKey::CtrlQ,
                    _ => {
                        return Err(anyhow!(
                            "invalid cancel_agent_key '{}', expected ctrl_c|ctrl_k|ctrl_q",
                            value
                        ));
                    }
                }
            }
            "enable_dbos" => {
                cfg.enable_dbos = parse_bool(value, "enable_dbos")?;
            }
            "subagent_verbose" => {
                cfg.subagent_verbose = parse_bool(value, "subagent_verbose")?;
            }
            "request_timeout_secs" => {
                cfg.request_timeout_secs = Some(
                    value
                        .parse::<i64>()
                        .map_err(|_| anyhow!("request_timeout_secs must be an integer"))?
                        .max(1),
                );
            }
            "temperature" => {
                cfg.temperature = Some(
                    value
                        .parse::<f64>()
                        .map_err(|_| anyhow!("temperature must be a float"))?,
                );
            }
            "max_tokens" => {
                cfg.max_tokens = Some(
                    value
                        .parse::<i64>()
                        .map_err(|_| anyhow!("max_tokens must be an integer"))?
                        .max(0),
                );
            }
            "autosave_enabled" => {
                cfg.autosave_enabled = parse_bool(value, "autosave_enabled")?;
            }
            "max_autosave_sessions" => {
                cfg.max_autosave_sessions = Some(
                    value
                        .parse::<i64>()
                        .map_err(|_| anyhow!("max_autosave_sessions must be an integer"))?
                        .max(1),
                );
            }
            "universal_constructor_enabled" => {
                cfg.universal_constructor_enabled =
                    parse_bool(value, "universal_constructor_enabled")?;
            }
            "compact_diffs" => {
                cfg.compact_diffs = parse_bool(value, "compact_diffs")?;
            }
            "onboarding_complete" => {
                cfg.onboarding_complete = parse_bool(value, "onboarding_complete")?;
            }
            _ => return Err(anyhow!("unknown config key: {}", key)),
        }

        Ok(())
    })
}

fn parse_bool(value: &str, key: &str) -> Result<bool> {
    value
        .parse::<bool>()
        .map_err(|_| anyhow!("{} must be true or false", key))
}

#[allow(dead_code)]
pub fn reload_runtime_config() {
    if let Ok(cfg) = Config::load() {
        let mut guard = RUNTIME_CONFIG
            .write()
            .expect("runtime config lock poisoned during reload");
        *guard = cfg;
    }
}

#[allow(dead_code)]
pub fn snapshot_config() -> Config {
    with_read(Clone::clone)
}

/// The models to run the gate's verifiers on, if configured.
///
/// Unset means the orchestrator's own default, which names three models from
/// three providers.
pub fn get_gate_verifier_models() -> Option<Vec<String>> {
    with_read(|cfg| {
        cfg.gate_verifier_models
            .as_ref()
            .filter(|models| !models.is_empty())
            .cloned()
    })
}

/// How many rounds the gate may send work back, if configured.
pub fn get_gate_max_rounds() -> Option<u32> {
    with_read(|cfg| cfg.gate_max_rounds)
}

/// The command the gate runs to gather evidence, if configured.
pub fn get_gate_test_command() -> Option<String> {
    with_read(|cfg| cfg.gate_test_command.clone())
}

/// Whether answers are shown as they arrive rather than all at once.
pub fn get_enable_streaming() -> bool {
    with_read(|cfg| cfg.general.enable_streaming)
}
