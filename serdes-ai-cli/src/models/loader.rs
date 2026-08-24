use std::collections::BTreeSet;
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

pub fn load_models_from_code_puppy() -> Vec<String> {
    let mut models = Vec::new();

    models.extend(load_model_file(&config::get_models_file()));
    models.extend(load_model_file(&config::get_extra_models_file()));

    dedupe_sorted(models)
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
    all.extend(load_models_from_code_puppy());
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
