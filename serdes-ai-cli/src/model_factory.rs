//! Model factory for creating models from spec strings.
//!
//! Supports formats like:
//! - "openai:gpt-4o"
//! - "anthropic:claude-3-5-sonnet-20241022"
//! - "groq:llama-3.1-70b"
//! - "ollama:llama3.1"
//! - "openrouter:anthropic/claude-3.5-sonnet"

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use serdes_ai_agent::ModelConfig;
use serdes_ai_models::Model;
use serdes_ai_models::scripted::{Script, ScriptedModel};
use tracing::{debug, info, warn};

use crate::config;

/// Parse a model spec and create a model instance.
///
/// Format: "provider:model_name" or just "model_name" (defaults to openai)
pub async fn create_model_from_spec(spec: &str) -> Result<Arc<dyn Model>> {
    create_model_sync(spec)
}

/// Build a model without awaiting.
///
/// Model construction does no I/O, and the orchestrator's `ModelFactory` is a
/// synchronous trait, so both callers share this.
pub fn create_model_sync(spec: &str) -> Result<Arc<dyn Model>> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(anyhow!("model spec cannot be empty"));
    }

    // A scripted model takes precedence over every provider, so UI tests and
    // reproductions run the real application with no network involved.
    if let Some(model) = scripted_model_from_env()? {
        return Ok(model);
    }

    // A model the user defined with its own endpoint carries everything needed
    // to reach it, so it is resolved before provider parsing: its selector is a
    // name of the user's choosing and generally not a `provider:model` pair.
    if let Some(custom) = crate::models::loader::custom_model(spec) {
        return build_custom_model(&custom);
    }

    let (provider, model_name) = if spec.contains(':') {
        let parts: Vec<&str> = spec.splitn(2, ':').collect();
        (parts[0].to_lowercase(), parts[1])
    } else {
        ("openai".to_string(), spec)
    };

    info!(
        "Creating model from spec: provider={}, model={}",
        provider, model_name
    );

    let mut model_config = ModelConfig::new(format!("{}:{}", provider, model_name));

    if let Some(api_key) = load_api_key(&provider)? {
        model_config = model_config.with_api_key(api_key);
    }

    if let Some(base_url) = load_base_url(&provider) {
        model_config = model_config.with_base_url(normalize_base_url(&base_url));
    }

    let timeout_secs = config::get_request_timeout();
    model_config = model_config.with_timeout(Duration::from_secs(timeout_secs));

    let model = model_config
        .build_model()
        .map_err(|e| anyhow!("failed to build model '{}': {}", spec, e))?;

    debug!("Model created successfully from spec '{}'.", spec);
    Ok(model)
}

/// Build a model from a user-defined endpoint.
///
/// These speak the OpenAI protocol — that is what `custom_openai` means — so
/// the openai implementation is used, pointed at the given address and asked
/// for the name that server actually serves.
fn build_custom_model(custom: &crate::models::loader::CustomModel) -> Result<Arc<dyn Model>> {
    info!(
        "Creating custom model '{}': model={} endpoint={}",
        custom.id, custom.wire_name, custom.url
    );

    let mut model_config = ModelConfig::new(format!("openai:{}", custom.wire_name))
        .with_base_url(normalize_base_url(&custom.url));

    if !custom.api_key.trim().is_empty() {
        model_config = model_config.with_api_key(custom.api_key.clone());
    }

    // An explicit endpoint on the command line is the more specific
    // instruction, so it still overrides the saved one.
    if let Some(override_url) = std::env::var("SERDES_AI_BASE_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        model_config = model_config.with_base_url(normalize_base_url(&override_url));
    }

    model_config = model_config.with_timeout(Duration::from_secs(config::get_request_timeout()));

    model_config
        .build_model()
        .map_err(|e| anyhow!("failed to build custom model '{}': {}", custom.id, e))
}

/// Environment variable pointing at a scripted-model fixture.
pub const MOCK_ENV: &str = "SERDES_AI_MOCK";

/// Load the scripted fixture, if one is configured.
pub fn mock_script() -> Result<Option<Script>> {
    let Some(path) = std::env::var_os(MOCK_ENV) else {
        return Ok(None);
    };

    let path = std::path::PathBuf::from(path);
    if path.as_os_str().is_empty() {
        return Ok(None);
    }

    let script = Script::from_file(&path)
        .map_err(|e| anyhow!("{MOCK_ENV} is set but the script could not be loaded: {e}"))?;

    Ok(Some(script))
}

/// Build a scripted model when [`MOCK_ENV`] is set.
///
/// Returns `Ok(None)` when unset, so normal runs are untouched. A set-but-broken
/// value is an error rather than a silent fallback to a real provider: a test
/// that quietly started calling the network would be worse than one that fails.
fn scripted_model_from_env() -> Result<Option<Arc<dyn Model>>> {
    let Some(path) = std::env::var_os(MOCK_ENV) else {
        return Ok(None);
    };

    let path = std::path::PathBuf::from(path);
    if path.as_os_str().is_empty() {
        return Ok(None);
    }

    let script = Script::from_file(&path)
        .map_err(|e| anyhow!("{MOCK_ENV} is set but the script could not be loaded: {e}"))?;

    warn!(
        "{MOCK_ENV} is set: replaying {} instead of calling a provider",
        path.display()
    );
    Ok(Some(Arc::new(ScriptedModel::new(script))))
}

/// Load API key from environment or config file.
fn load_api_key(provider: &str) -> Result<Option<String>> {
    let env_var = match provider {
        "openai" | "gpt" => "OPENAI_API_KEY",
        "anthropic" | "claude" => "ANTHROPIC_API_KEY",
        "groq" => "GROQ_API_KEY",
        "mistral" => "MISTRAL_API_KEY",
        "bedrock" | "aws" => "AWS_ACCESS_KEY_ID",
        "openrouter" | "or" => "OPENROUTER_API_KEY",
        "google" | "gemini" => "GOOGLE_API_KEY",
        "cohere" | "co" => "CO_API_KEY",
        "huggingface" | "hf" => "HF_TOKEN",
        "ollama" => return Ok(None),
        _ => return Ok(None),
    };

    if let Ok(key) = std::env::var(env_var) {
        if !key.trim().is_empty() {
            return Ok(Some(key));
        }
    }

    if let Some(key) = config::get_api_key(provider) {
        if !key.trim().is_empty() {
            return Ok(Some(key));
        }
    }

    warn!(
        "No API key found for provider '{}' (expected env var '{}').",
        provider, env_var
    );
    Ok(None)
}

/// The endpoint to talk to for `provider`.
///
/// Order of precedence, most specific first:
///
/// 1. `SERDES_AI_BASE_URL` — applies whatever the provider is, for a one-off
///    against a local or proxied server.
/// 2. `<PROVIDER>_BASE_URL`, e.g. `OPENAI_BASE_URL`, so different providers can
///    be pointed at different endpoints in the same shell.
/// 3. The `base_urls` entry saved in the configuration.
/// 4. The provider's own default, which for most is whatever the SDK uses and
///    for Ollama is the local daemon.
///
/// This is what makes an OpenAI-compatible server usable: point the `openai`
/// provider at it and give the model whatever name that server expects.
fn load_base_url(provider: &str) -> Option<String> {
    let from_env = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };

    if let Some(url) = from_env("SERDES_AI_BASE_URL") {
        return Some(url);
    }

    let provider_var = format!("{}_BASE_URL", provider.to_uppercase());
    if let Some(url) = from_env(&provider_var) {
        return Some(url);
    }

    if let Some(url) = config::get_base_url(provider) {
        let url = url.trim().to_string();
        if !url.is_empty() {
            return Some(url);
        }
    }

    match provider {
        // Ollama runs locally and its address is not something the SDK knows.
        "ollama" => from_env("OLLAMA_HOST").or_else(|| Some("http://localhost:11434".to_string())),
        _ => None,
    }
}

/// Strip trailing slashes from an endpoint.
///
/// The request path is appended to this, so `https://host/v1/` would produce
/// `https://host/v1//chat/completions` — which servers answer with a 404 that
/// says nothing about the cause. Writing the trailing slash is natural enough
/// that it should simply work.
fn normalize_base_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

/// List available providers.
pub fn list_providers() -> Vec<&'static str> {
    vec![
        "openai",
        "anthropic",
        "groq",
        "ollama",
        "mistral",
        "bedrock",
        "openrouter",
        "google",
        "cohere",
        "huggingface",
    ]
}

/// Validate a model spec.
pub fn validate_model_spec(spec: &str) -> Result<()> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(anyhow!("model spec cannot be empty"));
    }

    // A user-defined model names itself and brings its own endpoint, so there
    // is no provider to recognise.
    if crate::models::loader::custom_model(spec).is_some() {
        return Ok(());
    }

    let (provider, model_name) = if spec.contains(':') {
        let parts: Vec<&str> = spec.splitn(2, ':').collect();
        (parts[0].to_lowercase(), parts[1].trim())
    } else {
        ("openai".to_string(), spec)
    };

    if model_name.is_empty() {
        return Err(anyhow!("model name cannot be empty in spec '{}'.", spec));
    }

    // The provider chooses the wire protocol, so an unrecognised one has no
    // implementation to run. Custom and self-hosted servers are reached by
    // naming the protocol they speak — nearly always openai — and pointing it
    // at their address.
    let valid = list_providers();
    if !valid.contains(&provider.as_str()) {
        return Err(anyhow!(
            "unknown provider '{}'.\n\
             For an OpenAI-compatible server use the openai provider and point it \
             at your endpoint:\n    \
             serdes-ai --base-url <URL> -m openai:{}\n\
             Built-in providers: {}",
            provider,
            model_name,
            valid.join(", ")
        ));
    }

    Ok(())
}
