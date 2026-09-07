//! A model that replays a script instead of calling a provider.
//!
//! Exists so the CLI can be driven end to end without a network: point
//! `SERDES_AI_MOCK` at a script file and every model request is answered from it.
//! That makes terminal UI tests deterministic, and doubles as a way to reproduce
//! a reported sequence locally.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serdes_ai_core::messages::StreamCompleteEvent;
use serdes_ai_core::{
    FinishReason, ModelRequest, ModelResponse, ModelResponsePart, ModelResponseStreamEvent,
    ModelSettings,
};

use crate::error::ModelError;
use crate::model::{Model, ModelRequestParameters, StreamedResponse};
use crate::profile::ModelProfile;

/// One scripted response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Turn {
    /// Reply with text and stop.
    Text {
        /// What to say.
        text: String,
    },
    /// Call a tool.
    ///
    /// Also how a structured-output agent is driven: name the output tool and
    /// pass the object it should return.
    Tool {
        /// Tool name.
        tool: String,
        /// Arguments to pass.
        args: serde_json::Value,
    },
    /// Fail the request, to exercise error handling.
    Error {
        /// The message to fail with.
        error: String,
    },
}

/// A recorded conversation for one or more agents.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Script {
    /// Turns used when no more specific sequence matches.
    #[serde(default)]
    pub turns: Vec<Turn>,
    /// Turns keyed by agent role, for multi-agent runs.
    #[serde(default)]
    pub by_role: HashMap<String, Vec<Turn>>,
}

impl Script {
    /// Parse a script from JSON.
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        serde_json::from_str(json)
            .map_err(|e| ModelError::configuration(format!("invalid mock script: {e}")))
    }

    /// Load a script from a file.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, ModelError> {
        let path = path.as_ref();
        let raw = std::fs::read_to_string(path).map_err(|e| {
            ModelError::configuration(format!(
                "could not read mock script {}: {e}",
                path.display()
            ))
        })?;
        Self::from_json(&raw)
    }

    /// The turns for `role`, falling back to the default sequence.
    pub fn turns_for(&self, role: Option<&str>) -> &[Turn] {
        role.and_then(|r| self.by_role.get(r))
            .map(Vec::as_slice)
            .unwrap_or(&self.turns)
    }
}

/// Replays a [`Script`], advancing one turn per request.
///
/// The final turn repeats once the script is exhausted, so a short script cannot
/// wedge an agent loop waiting for a response that never comes.
pub struct ScriptedModel {
    name: String,
    role: Option<String>,
    script: Arc<Script>,
    step: AtomicUsize,
    profile: ModelProfile,
}

impl ScriptedModel {
    /// Replay `script`, using the default turn sequence.
    pub fn new(script: Script) -> Self {
        Self {
            name: "scripted".to_string(),
            role: None,
            script: Arc::new(script),
            step: AtomicUsize::new(0),
            profile: ModelProfile::default(),
        }
    }

    /// Replay the sequence recorded for `role`.
    pub fn for_role(script: Arc<Script>, role: impl Into<String>) -> Self {
        Self {
            name: "scripted".to_string(),
            role: Some(role.into()),
            script,
            step: AtomicUsize::new(0),
            profile: ModelProfile::default(),
        }
    }

    /// Override the reported model name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// How many requests have been answered.
    pub fn served(&self) -> usize {
        self.step.load(Ordering::SeqCst)
    }

    /// The next turn, repeating the last once exhausted.
    fn next_turn(&self) -> Turn {
        let turns = self.script.turns_for(self.role.as_deref());
        if turns.is_empty() {
            return Turn::Text {
                text: String::new(),
            };
        }

        let i = self.step.fetch_add(1, Ordering::SeqCst);
        turns[i.min(turns.len() - 1)].clone()
    }
}

fn response_for(turn: &Turn) -> Result<ModelResponse, ModelError> {
    match turn {
        Turn::Text { text } => {
            Ok(ModelResponse::text(text.clone()).with_finish_reason(FinishReason::Stop))
        }
        Turn::Tool { tool, args } => {
            Ok(ModelResponse::with_parts(vec![ModelResponsePart::tool_call(
                tool.clone(),
                args.clone(),
            )])
            .with_finish_reason(FinishReason::ToolCall))
        }
        Turn::Error { error } => Err(ModelError::Other(anyhow::anyhow!("{error}"))),
    }
}

fn stream_for(turn: &Turn) -> Result<StreamedResponse, ModelError> {
    let events = match turn {
        Turn::Text { text } => vec![
            Ok(ModelResponseStreamEvent::part_start(
                0,
                ModelResponsePart::text(""),
            )),
            Ok(ModelResponseStreamEvent::text_delta(0, text.clone())),
            Ok(ModelResponseStreamEvent::StreamComplete(
                StreamCompleteEvent::new(FinishReason::Stop),
            )),
        ],
        Turn::Tool { tool, args } => vec![
            Ok(ModelResponseStreamEvent::part_start(
                0,
                ModelResponsePart::tool_call(tool.clone(), args.clone()),
            )),
            Ok(ModelResponseStreamEvent::StreamComplete(
                StreamCompleteEvent::new(FinishReason::ToolCall),
            )),
        ],
        Turn::Error { error } => return Err(ModelError::Other(anyhow::anyhow!("{error}"))),
    };

    Ok(Box::pin(futures::stream::iter(events)))
}

#[async_trait]
impl Model for ScriptedModel {
    fn name(&self) -> &str {
        &self.name
    }

    fn system(&self) -> &str {
        "scripted"
    }

    fn profile(&self) -> &ModelProfile {
        &self.profile
    }

    async fn request(
        &self,
        _messages: &[ModelRequest],
        _settings: &ModelSettings,
        _params: &ModelRequestParameters,
    ) -> Result<ModelResponse, ModelError> {
        response_for(&self.next_turn())
    }

    async fn request_stream(
        &self,
        _messages: &[ModelRequest],
        _settings: &ModelSettings,
        _params: &ModelRequestParameters,
    ) -> Result<StreamedResponse, ModelError> {
        stream_for(&self.next_turn())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script() -> Script {
        Script::from_json(
            r#"{
                "turns": [
                    {"tool": {"tool": "read_file", "args": {"path": "a.rs"}}},
                    {"text": {"text": "all done"}}
                ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn parses_turns_from_json() {
        let s = script();

        assert_eq!(s.turns.len(), 2);
        assert!(matches!(&s.turns[0], Turn::Tool { tool, .. } if tool == "read_file"));
        assert!(matches!(&s.turns[1], Turn::Text { text } if text == "all done"));
    }

    #[test]
    fn rejects_malformed_json_with_a_useful_message() {
        let err = Script::from_json("{ not json").unwrap_err();

        assert!(err.to_string().contains("invalid mock script"));
    }

    #[test]
    fn a_missing_file_is_reported_with_its_path() {
        let err = Script::from_file("/nonexistent/script.json").unwrap_err();

        assert!(err.to_string().contains("/nonexistent/script.json"));
    }

    #[tokio::test]
    async fn replays_turns_in_order() {
        let model = ScriptedModel::new(script());
        let settings = ModelSettings::default();
        let params = ModelRequestParameters::new();

        let first = model.request(&[], &settings, &params).await.unwrap();
        assert_eq!(first.finish_reason, Some(FinishReason::ToolCall));

        let second = model.request(&[], &settings, &params).await.unwrap();
        assert_eq!(second.finish_reason, Some(FinishReason::Stop));
        assert_eq!(model.served(), 2);
    }

    #[tokio::test]
    async fn the_last_turn_repeats_once_exhausted() {
        // A short script must not wedge an agent loop waiting for a reply.
        let model = ScriptedModel::new(script());
        let settings = ModelSettings::default();
        let params = ModelRequestParameters::new();

        for _ in 0..5 {
            model.request(&[], &settings, &params).await.unwrap();
        }

        let last = model.request(&[], &settings, &params).await.unwrap();
        assert_eq!(last.finish_reason, Some(FinishReason::Stop));
    }

    #[tokio::test]
    async fn an_error_turn_fails_the_request() {
        let model = ScriptedModel::new(
            Script::from_json(r#"{"turns":[{"error":{"error":"rate limited"}}]}"#).unwrap(),
        );

        let err = model
            .request(
                &[],
                &ModelSettings::default(),
                &ModelRequestParameters::new(),
            )
            .await
            .unwrap_err();

        assert!(err.to_string().contains("rate limited"));
    }

    #[tokio::test]
    async fn streaming_replays_the_same_script() {
        // Subagents are driven through run_stream, so both paths must answer.
        use futures::StreamExt;

        let model = ScriptedModel::new(script());
        let mut stream = model
            .request_stream(
                &[],
                &ModelSettings::default(),
                &ModelRequestParameters::new(),
            )
            .await
            .unwrap();

        let mut saw_terminal = false;
        while let Some(event) = stream.next().await {
            if matches!(event, Ok(ModelResponseStreamEvent::StreamComplete(_))) {
                saw_terminal = true;
            }
        }

        assert!(saw_terminal);
    }

    #[test]
    fn per_role_turns_override_the_default() {
        let s = Script::from_json(
            r#"{
                "turns": [{"text": {"text": "default"}}],
                "by_role": {"code": [{"text": {"text": "coding"}}]}
            }"#,
        )
        .unwrap();

        assert!(matches!(&s.turns_for(Some("code"))[0], Turn::Text { text } if text == "coding"));
        assert!(matches!(&s.turns_for(Some("other"))[0], Turn::Text { text } if text == "default"));
        assert!(matches!(&s.turns_for(None)[0], Turn::Text { text } if text == "default"));
    }

    #[tokio::test]
    async fn an_empty_script_answers_rather_than_hanging() {
        let model = ScriptedModel::new(Script::default());

        let response = model
            .request(
                &[],
                &ModelSettings::default(),
                &ModelRequestParameters::new(),
            )
            .await
            .unwrap();

        assert_eq!(response.finish_reason, Some(FinishReason::Stop));
    }
}
