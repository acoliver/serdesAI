//! Live smoke test for the responses model's WebSocket transport.
//!
//! Two variants share one code path:
//!
//! - **default**: `wss://api.openai.com/v1/responses`, token read from the
//!   `OPENAI_API_KEY` environment variable.
//! - **codex** (`--codex` flag or `CODEX=1`): the ChatGPT codex backend at
//!   `wss://chatgpt.com/backend-api/codex/responses`, authenticated by the
//!   OAuth PKCE flow from `serdes-ai-providers` (the codex CLI's client id,
//!   browser opens automatically, callback on localhost:1455).
//!
//! ```bash
//! # one haiku, streamed as deltas
//! cargo run -p serdes-ai-providers --example codex_haiku
//! cargo run -p serdes-ai-providers --example codex_haiku -- gpt-5.6-sol
//! # codex backend instead of the plain API endpoint
//! cargo run -p serdes-ai-providers --example codex_haiku -- --codex
//! # tool-call round trip: model calls get_weather, example answers,
//! # chained second turn returns the final answer
//! cargo run -p serdes-ai-providers --example codex_haiku -- tools
//! ```
//!
//! Codex tokens are cached in `~/.keys/.serdes_codex_token.json` (never
//! printed) and reused while fresh, so a second run within ~25 minutes
//! skips the browser.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use futures::StreamExt;
use serdes_ai_core::ModelSettings;
use serdes_ai_core::messages::{
    ModelRequest, ModelRequestPart, ModelResponsePartDelta, ModelResponseStreamEvent,
    UserPromptPart,
};
use serdes_ai_models::model::{Model, ModelRequestParameters};
use serdes_ai_models::openai::OpenAIResponsesModel;
use serdes_ai_models::openai::responses::Transport;
use serdes_ai_providers::{TokenResponse, chatgpt_oauth_config, run_pkce_flow};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Plain OpenAI Responses websocket endpoint (default variant).
const DEFAULT_ENDPOINT: &str = "wss://api.openai.com/v1/responses";
/// ChatGPT codex backend (`--codex` / `CODEX=1`).
const CODEX_ENDPOINT: &str = "wss://chatgpt.com/backend-api/codex/responses";
/// Default model on the plain OpenAI endpoint.
const DEFAULT_MODEL: &str = "gpt-5.1";
/// Default model on the codex backend.
const CODEX_DEFAULT_MODEL: &str = "gpt-5.6-luna";
/// Conservative reuse window; grants are typically valid for an hour.
const REUSE_SECS: u64 = 25 * 60;

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedToken {
    token: TokenResponse,
    fetched_at: u64,
}

fn cache_path() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME not set");
    PathBuf::from(home)
        .join(".keys")
        .join(".serdes_codex_token.json")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_secs()
}

async fn obtain_token() -> Result<TokenResponse, Box<dyn std::error::Error>> {
    if let Ok(raw) = std::fs::read_to_string(cache_path()) {
        if let Ok(cached) = serde_json::from_str::<CachedToken>(&raw) {
            let age = now_secs().saturating_sub(cached.fetched_at);
            if age < REUSE_SECS {
                println!("(reusing cached token, {age}s old)");
                return Ok(cached.token);
            }
        }
    }

    let config = chatgpt_oauth_config();
    let (url, handle) = run_pkce_flow(&config).await?;
    println!("Login required. Opening your browser; if nothing happens, visit:");
    println!();
    println!("  {url}");
    println!();
    println!("Waiting for the callback on localhost:1455 ...");
    std::io::stdout().flush()?;

    let _ = std::process::Command::new("open").arg(&url).spawn();
    let token = handle.wait_for_tokens().await?;

    let cached = CachedToken {
        token: token.clone(),
        fetched_at: now_secs(),
    };
    if let Some(dir) = cache_path().parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(cache_path(), serde_json::to_string(&cached)?)?;
    println!("Token cached to ~/.keys/.serdes_codex_token.json (not printed).");
    Ok(token)
}

/// Extract `chatgpt_account_id` from the id_token JWT, the header the codex
/// backend requires for ChatGPT-plan accounts. Best effort: returns None on
/// any decode failure and the request simply goes out without the header.
fn account_id(id_token: Option<&str>) -> Option<String> {
    let payload_b64 = id_token?.split('.').nth(1)?;
    let payload = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    claims["https://api.openai.com/auth"]["chatgpt_account_id"]
        .as_str()
        .map(str::to_owned)
}

fn user_turn(text: &str) -> ModelRequest {
    ModelRequest::with_parts(vec![ModelRequestPart::UserPrompt(UserPromptPart::new(
        text,
    ))])
}

/// Full tool-call round trip against the live backend: register a function
/// tool, let the model call it, answer locally, and finish on a chained
/// turn. Fails loudly if any leg of the loop is broken on the wire.
async fn tool_round_trip(model: &OpenAIResponsesModel) -> Result<(), Box<dyn std::error::Error>> {
    use serdes_ai_core::messages::{ModelResponsePart, ToolCallArgs, ToolReturnPart};
    use serdes_ai_tools::ToolDefinition;

    let params = ModelRequestParameters::new().with_tools(vec![ToolDefinition {
        name: "get_weather".into(),
        description: "Get the current weather for a city.".into(),
        parameters_json_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "city": {"type": "string", "description": "City name"}
            },
            "required": ["city"],
            "additionalProperties": false
        }),
        strict: Some(true),
        outer_typed_dict_key: None,
    }]);

    println!("-- turn 1: one function tool offered, asking about Tokyo weather");
    let history = vec![user_turn(
        "What is the weather in Tokyo? Call the get_weather tool.",
    )];
    let first = model
        .request(&history, &ModelSettings::default(), &params)
        .await?;

    let call = first.parts.iter().find_map(|part| match part {
        ModelResponsePart::ToolCall(call) => Some(call.clone()),
        _ => None,
    });
    let Some(call) = call else {
        let text = first
            .parts
            .iter()
            .filter_map(|part| match part {
                ModelResponsePart::Text(t) => Some(t.content.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        return Err(format!("model returned no tool call; text was: {text}").into());
    };
    let args = match &call.args {
        ToolCallArgs::String(s) => s.clone(),
        other => format!("{other:?}"),
    };
    println!(
        "-- tool call received: name={} call_id={:?}",
        call.tool_name, call.tool_call_id
    );
    println!("   arguments={args}");
    if args.trim().is_empty() {
        return Err(
            "tool call arrived with EMPTY arguments: argument deltas did not assemble".into(),
        );
    }

    // "Execute" the tool locally and return the result on a chained turn.
    let mut tool_return = ToolReturnPart::new(
        "get_weather",
        r#"{"temperature_c": 18, "conditions": "clear"}"#,
    );
    tool_return.tool_call_id = call.tool_call_id.clone();
    let history = vec![
        history.into_iter().next().expect("one turn"),
        ModelRequest::with_parts(vec![ModelRequestPart::ModelResponse(Box::new(first))]),
        ModelRequest::with_parts(vec![ModelRequestPart::ToolReturn(tool_return)]),
    ];
    println!("-- turn 2: returning the tool output on a chained turn");
    let second = model
        .request(&history, &ModelSettings::default(), &params)
        .await?;

    let text = second
        .parts
        .iter()
        .filter_map(|part| match part {
            ModelResponsePart::Text(t) => Some(t.content.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    println!();
    println!("{text}");
    println!();
    if text.trim().is_empty() {
        return Err("second turn returned no text".into());
    }
    println!(
        "-- tool round trip complete: finish={:?} usage={:?}",
        second.finish_reason, second.usage
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    let mut model_name: Option<String> = None;
    let mut tools_mode = false;
    let mut codex = std::env::var("CODEX").is_ok_and(|v| v == "1");
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "tools" => tools_mode = true,
            "--codex" => codex = true,
            _ => model_name = Some(arg),
        }
    }
    let name = model_name.unwrap_or_else(|| {
        if codex {
            CODEX_DEFAULT_MODEL.to_owned()
        } else {
            DEFAULT_MODEL.to_owned()
        }
    });

    let (model, endpoint) = if codex {
        let token = obtain_token().await?;
        let mut model = OpenAIResponsesModel::new(&name, token.access_token.clone())
            .with_base_url(CODEX_ENDPOINT)
            .with_transport(Transport::WebSocket)
            .with_session_chaining(true)
            .with_header("Authorization", format!("Bearer {}", token.access_token))
            .with_header("OpenAI-Beta", "responses_websockets=2026-02-06")
            .with_header("originator", "serdesai_miniclient")
            .with_header("User-Agent", "serdesai_miniclient/0.1 (codex ws smoke)");
        if let Some(id) = account_id(token.id_token.as_deref()) {
            model = model.with_header("chatgpt-account-id", id);
        }
        (model, CODEX_ENDPOINT)
    } else {
        let api_key = std::env::var("OPENAI_API_KEY").map_err(
            |_| "OPENAI_API_KEY not set; export it or pass --codex for the ChatGPT OAuth flow",
        )?;
        let model = OpenAIResponsesModel::new(&name, api_key.clone())
            .with_base_url(DEFAULT_ENDPOINT)
            .with_transport(Transport::WebSocket)
            .with_session_chaining(true)
            .with_header("Authorization", format!("Bearer {api_key}"));
        (model, DEFAULT_ENDPOINT)
    };

    println!();
    println!("Connecting: model={name} endpoint={endpoint} transport=websocket");
    println!();

    if tools_mode {
        return tool_round_trip(&model).await;
    }

    let history = vec![user_turn(
        "Write us one haiku about finally getting websockets to work.",
    )];
    let mut stream = model
        .request_stream(
            &history,
            &ModelSettings::default(),
            &ModelRequestParameters::new(),
        )
        .await?;

    while let Some(event) = stream.next().await {
        match event? {
            ModelResponseStreamEvent::PartDelta(delta) => {
                if let ModelResponsePartDelta::Text(text) = delta.delta {
                    print!("{}", text.content_delta);
                    std::io::stdout().flush()?;
                }
            }
            ModelResponseStreamEvent::StreamComplete(complete) => {
                println!();
                println!();
                println!(
                    "-- stream complete: finish={:?} input_tokens={:?} output_tokens={:?}",
                    complete.finish_reason, complete.input_tokens, complete.output_tokens
                );
            }
            ModelResponseStreamEvent::PartStart(_) | ModelResponseStreamEvent::PartEnd(_) => {}
        }
    }
    Ok(())
}
