//! Groq model implementation.
//!
//! [Groq](https://groq.com) provides ultra-fast inference for popular open models
//! like Llama, Mixtral, and Gemma using their custom LPU hardware.
//!
//! Groq uses an OpenAI-compatible API, so this implementation wraps OpenAIChatModel.
//!
//! ## Example
//!
//! ```ignore
//! use serdes_ai_models::groq::GroqModel;
//!
//! let model = GroqModel::new("llama-3.1-70b-versatile", api_key);
//! // or
//! let model = GroqModel::from_env("mixtral-8x7b-32768")?;
//! ```
//!
//! ## Available Models
//!
//! - `llama-3.1-70b-versatile` - Llama 3.1 70B, 128K context
//! - `llama-3.1-8b-instant` - Llama 3.1 8B, fast inference
//! - `llama-3.2-90b-text-preview` - Llama 3.2 90B
//! - `mixtral-8x7b-32768` - Mixtral 8x7B, 32K context
//! - `gemma2-9b-it` - Gemma 2 9B instruction-tuned

use async_trait::async_trait;
use reqwest::Client;

use crate::error::ModelError;
use crate::model::{Model, ModelRequestParameters, StreamedResponse};
use crate::openai::OpenAIChatModel;
use crate::profile::ModelProfile;
use serdes_ai_core::{ModelRequest, ModelResponse, ModelSettings};

/// Groq model client.
///
/// Groq uses an OpenAI-compatible API, so this wraps OpenAIChatModel
/// with the Groq-specific base URL and settings.
#[derive(Debug, Clone)]
pub struct GroqModel {
    /// Inner OpenAI-compatible model.
    inner: OpenAIChatModel,
}

impl GroqModel {
    /// Groq API base URL.
    pub const BASE_URL: &'static str = "https://api.groq.com/openai/v1";

    /// Create a new Groq model with an API key.
    pub fn new(model_name: impl Into<String>, api_key: impl Into<String>) -> Self {
        let inner = OpenAIChatModel::new(model_name, api_key).with_base_url(Self::BASE_URL);
        Self { inner }
    }

    /// Create from environment variable `GROQ_API_KEY`.
    pub fn from_env(model_name: impl Into<String>) -> Result<Self, ModelError> {
        let api_key = std::env::var("GROQ_API_KEY")
            .map_err(|_| ModelError::configuration("GROQ_API_KEY not set"))?;
        Ok(Self::new(model_name, api_key))
    }

    /// Set a custom HTTP client.
    #[must_use]
    pub fn with_client(mut self, client: Client) -> Self {
        self.inner = self.inner.with_client(client);
        self
    }

    /// Create a Llama 3.1 70B Versatile model.
    pub fn llama_70b(api_key: impl Into<String>) -> Self {
        Self::new("llama-3.1-70b-versatile", api_key)
    }

    /// Create a Llama 3.1 70B model from environment.
    pub fn llama_70b_from_env() -> Result<Self, ModelError> {
        Self::from_env("llama-3.1-70b-versatile")
    }

    /// Create a Llama 3.1 8B Instant model.
    pub fn llama_8b(api_key: impl Into<String>) -> Self {
        Self::new("llama-3.1-8b-instant", api_key)
    }

    /// Create a Mixtral 8x7B model.
    pub fn mixtral(api_key: impl Into<String>) -> Self {
        Self::new("mixtral-8x7b-32768", api_key)
    }

    /// Create a Gemma 2 9B model.
    pub fn gemma_9b(api_key: impl Into<String>) -> Self {
        Self::new("gemma2-9b-it", api_key)
    }

    /// Get the model name.
    pub fn model_name(&self) -> &str {
        self.inner.name()
    }
}

#[async_trait]
impl Model for GroqModel {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn system(&self) -> &str {
        "groq"
    }

    fn profile(&self) -> &ModelProfile {
        self.inner.profile()
    }

    async fn request(
        &self,
        messages: &[ModelRequest],
        settings: &ModelSettings,
        params: &ModelRequestParameters,
    ) -> Result<ModelResponse, ModelError> {
        self.inner.request(messages, settings, params).await
    }

    async fn request_stream(
        &self,
        messages: &[ModelRequest],
        settings: &ModelSettings,
        params: &ModelRequestParameters,
    ) -> Result<StreamedResponse, ModelError> {
        self.inner.request_stream(messages, settings, params).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serdes_ai_core::FinishReason;

    #[test]
    fn test_groq_model_creation() {
        let model = GroqModel::new("llama-3.1-70b-versatile", "test-key");
        assert_eq!(model.name(), "llama-3.1-70b-versatile");
        assert_eq!(model.system(), "groq");
    }

    #[test]
    fn test_groq_convenience_constructors() {
        let model = GroqModel::llama_70b("key");
        assert_eq!(model.name(), "llama-3.1-70b-versatile");

        let model = GroqModel::mixtral("key");
        assert_eq!(model.name(), "mixtral-8x7b-32768");

        let model = GroqModel::gemma_9b("key");
        assert_eq!(model.name(), "gemma2-9b-it");
    }

    /// Groq streams by delegating to an inner OpenAI chat model, so a
    /// streamed request over real HTTP inherits the terminal StreamComplete
    /// emission at [DONE] with the buffered usage.
    ///
    /// The Groq base URL is fixed, so the test points the inner model at a
    /// mock server and exercises the real `request_stream` delegation.
    #[tokio::test]
    async fn request_stream_inherits_terminal_stream_complete() {
        use futures::StreamExt;
        use serdes_ai_core::messages::ModelResponseStreamEvent;

        let sse_body = concat!(
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1234567890,\"model\":\"llama-3.1-70b-versatile\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello\"}}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1234567890,\"model\":\"llama-3.1-70b-versatile\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":1234567890,\"model\":\"llama-3.1-70b-versatile\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n\n",
            "data: [DONE]\n\n",
        );

        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse_body),
            )
            .mount(&server)
            .await;

        let model = GroqModel {
            inner: OpenAIChatModel::new("llama-3.1-70b-versatile", "test-key")
                .with_base_url(server.uri()),
        };
        let mut req = ModelRequest::new();
        req.add_user_prompt("Hello");
        let mut stream = model
            .request_stream(
                &[req],
                &ModelSettings::new(),
                &ModelRequestParameters::new(),
            )
            .await
            .unwrap();

        let mut events = Vec::new();
        while let Some(result) = stream.next().await {
            events.push(result.unwrap());
        }

        let terminals: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, ModelResponseStreamEvent::StreamComplete(_)))
            .collect();
        assert_eq!(terminals.len(), 1, "expected exactly one terminal event");

        match events.last() {
            Some(ModelResponseStreamEvent::StreamComplete(complete)) => {
                assert_eq!(complete.finish_reason, FinishReason::Stop);
                assert_eq!(complete.input_tokens, Some(10));
                assert_eq!(complete.output_tokens, Some(5));
                assert_eq!(complete.cache_creation_tokens, None);
                assert_eq!(complete.cache_read_tokens, None);
            }
            other => panic!("expected terminal StreamComplete last, got {:?}", other),
        }
    }
}
