use std::collections::{BTreeSet, HashMap};
use std::fs;

use serde::Deserialize;

use crate::config;

const SUPPORTED_MODEL_TYPES: &[&str] =
    &["openai", "anthropic", "gemini", "custom_openai", "cerebras"];

#[derive(Debug, Deserialize)]
struct CustomEndpoint {
    #[serde(default)]
    url: String,
    #[serde(default)]
    api_key: String,
}

/// A model served by an OpenAI-compatible endpoint of the user's own.
///
/// The entry's key is what the user selects; `wire_name` is what the server is
/// actually asked for, which is often different — an entry called
/// `kaban-kimi-ega` may be served as `lagon-5.0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomModel {
    pub id: String,
    pub wire_name: String,
    pub url: String,
    pub api_key: String,
}

#[derive(Debug, Deserialize)]
struct ModelDefinition {
    #[serde(rename = "type", default)]
    model_type: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    custom_endpoint: Option<CustomEndpoint>,
}

type ModelsFile = std::collections::HashMap<String, ModelDefinition>;

pub fn load_models_from_config() -> Vec<String> {
    let mut models = Vec::new();

    for path in model_files() {
        models.extend(load_model_file(&path));
    }

    dedupe_sorted(models)
}

/// Every place a model definition file may sit.
///
/// The files live in the configuration directory alongside the settings, but
/// were only ever looked for in the data directory — so a definition written by
/// hand, or carried over from a previous install, was silently never read. Both
/// are checked, the configuration directory last so it wins on a conflict.
fn model_files() -> Vec<std::path::PathBuf> {
    let mut paths = vec![config::get_models_file(), config::get_extra_models_file()];

    let config_dir = config::get_config_dir();
    for name in ["models.json", "extra_models.json"] {
        let candidate = config_dir.join(name);
        if !paths.contains(&candidate) {
            paths.push(candidate);
        }
    }

    paths
}

/// The models reached through an endpoint of the user's own, by selector.
pub fn custom_models() -> HashMap<String, CustomModel> {
    let mut found = HashMap::new();

    for path in model_files() {
        for model in load_custom_models(&path) {
            found.insert(model.id.clone(), model);
        }
    }

    found
}

/// The definition for `id`, if it names a model with its own endpoint.
pub fn custom_model(id: &str) -> Option<CustomModel> {
    custom_models().remove(id.trim())
}

fn load_custom_models(path: &std::path::Path) -> Vec<CustomModel> {
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<ModelsFile>(&raw) else {
        return Vec::new();
    };

    parsed
        .into_iter()
        .filter_map(|(id, definition)| {
            let endpoint = definition.custom_endpoint?;

            let url = endpoint.url.trim().to_string();
            if url.is_empty() {
                return None;
            }

            // The server is asked for `name` when the entry gives one, since
            // the selector is frequently a local nickname rather than
            // something the server would recognise.
            let wire_name = match definition.name.trim() {
                "" => id.trim().to_string(),
                name => name.to_string(),
            };

            Some(CustomModel {
                id: id.trim().to_string(),
                wire_name,
                url,
                api_key: resolve_env_reference(&endpoint.api_key),
            })
        })
        .collect()
}

pub fn default_models() -> Vec<String> {
    vec![
        "openai:gpt-4o".to_string(),
        "openai:gpt-4.1".to_string(),
        "anthropic:claude-3-5-sonnet-20241022".to_string(),
        "google:gemini-2.0-flash".to_string(),
        "mistral:mistral-large-latest".to_string(),
        "groq:llama-3.1-70b-versatile".to_string(),
        "openrouter:anthropic/claude-3.5-sonnet".to_string(),
        "ollama:llama3.1".to_string(),
    ]
}

pub fn available_models() -> Vec<String> {
    let mut all = default_models();
    all.extend(load_models_from_config());
    all.push(config::get_model_name());
    dedupe_sorted(all)
}

fn load_model_file(path: &std::path::Path) -> Vec<String> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(_) => return Vec::new(),
    };

    let parsed = match serde_json::from_str::<ModelsFile>(&raw) {
        Ok(parsed) => parsed,
        Err(_) => return Vec::new(),
    };

    parsed
        .into_iter()
        .filter_map(|(id, mut definition)| {
            if !SUPPORTED_MODEL_TYPES.contains(&definition.model_type.as_str()) {
                return None;
            }

            if let Some(endpoint) = definition.custom_endpoint.as_mut() {
                endpoint.api_key = resolve_env_reference(&endpoint.api_key);
                if endpoint.url.trim().is_empty() {
                    return None;
                }
            }

            let _resolved_name = definition.name.trim();
            normalize_model(Some(&id))
        })
        .collect()
}

fn resolve_env_reference(value: &str) -> String {
    let trimmed = value.trim();
    if let Some(var_name) = trimmed.strip_prefix('$') {
        return std::env::var(var_name).unwrap_or_default();
    }

    trimmed.to_string()
}

fn normalize_model(value: Option<&str>) -> Option<String> {
    let s = value?.trim();
    if s.is_empty() {
        return None;
    }

    Some(s.to_string())
}

fn dedupe_sorted(values: Vec<String>) -> Vec<String> {
    let mut set = BTreeSet::new();
    for value in values {
        if !value.trim().is_empty() {
            set.insert(value);
        }
    }
    set.into_iter().collect()
}
