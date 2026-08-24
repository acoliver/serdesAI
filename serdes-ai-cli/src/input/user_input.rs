use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use crate::bus::{AnyMessage, MessageBus};
use crate::messages::{
    BaseMessage, ConfirmationRequest, ConfirmationResponse, MessageCategory, SelectionRequest,
    SelectionResponse, UserInputRequest, UserInputResponse,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct UserInputSystem {
    bus: Arc<MessageBus>,
}

impl UserInputSystem {
    pub fn new(bus: Arc<MessageBus>) -> Self {
        Self { bus }
    }

    /// Request confirmation (yes/no)
    /// Returns true if user confirms, false if declined
    pub async fn confirm(&self, prompt: &str) -> Result<bool> {
        let prompt_owned = prompt.to_string();
        let request = ConfirmationRequest {
            base: BaseMessage::new(MessageCategory::UserInteraction, None),
            prompt: prompt_owned.clone(),
        };

        let response = self
            .request_response(
                request.base.id.clone(),
                AnyMessage::ConfirmationRequest(request),
                move || read_confirmation(&prompt_owned),
            )
            .await?;

        match response {
            AnyMessage::ConfirmationResponse(msg) => Ok(msg.confirmed),
            _ => Err(anyhow!(
                "received unexpected response type for confirmation"
            )),
        }
    }

    /// Request selection from options (single select)
    /// Returns the index of selected option
    pub async fn select(&self, prompt: &str, options: &[String]) -> Result<usize> {
        if options.is_empty() {
            return Err(anyhow!("select requires at least one option"));
        }

        let prompt_owned = prompt.to_string();
        let request = SelectionRequest {
            base: BaseMessage::new(MessageCategory::UserInteraction, None),
            prompt: prompt_owned.clone(),
            options: options.to_vec(),
        };

        let options_owned = options.to_vec();
        let response = self
            .request_response(
                request.base.id.clone(),
                AnyMessage::SelectionRequest(request),
                move || read_single_selection(&prompt_owned, &options_owned),
            )
            .await?;

        match response {
            AnyMessage::SelectionResponse(msg) => msg
                .selected_indices
                .first()
                .copied()
                .ok_or_else(|| anyhow!("selection response did not contain any selected index")),
            _ => Err(anyhow!("received unexpected response type for selection")),
        }
    }

    /// Request multi-selection from options
    /// Returns indices of selected options
    pub async fn select_multi(&self, prompt: &str, options: &[String]) -> Result<Vec<usize>> {
        if options.is_empty() {
            return Err(anyhow!("select_multi requires at least one option"));
        }

        let prompt_owned = prompt.to_string();
        let request = SelectionRequest {
            base: BaseMessage::new(MessageCategory::UserInteraction, None),
            prompt: prompt_owned.clone(),
            options: options.to_vec(),
        };

        let options_owned = options.to_vec();
        let response = self
            .request_response(
                request.base.id.clone(),
                AnyMessage::SelectionRequest(request),
                move || read_multi_selection(&prompt_owned, &options_owned),
            )
            .await?;

        match response {
            AnyMessage::SelectionResponse(msg) => Ok(msg.selected_indices),
            _ => Err(anyhow!(
                "received unexpected response type for multi-selection"
            )),
        }
    }

    /// Request text input from user
    /// Returns the entered text
    pub async fn input(&self, prompt: &str, placeholder: Option<&str>) -> Result<String> {
        let prompt_owned = prompt.to_string();
        let request = UserInputRequest {
            base: BaseMessage::new(MessageCategory::UserInteraction, None),
            prompt: prompt_owned.clone(),
        };

        let placeholder_owned = placeholder.map(ToString::to_string);
        let response = self
            .request_response(
                request.base.id.clone(),
                AnyMessage::UserInputRequest(request),
                move || read_text_input(&prompt_owned, placeholder_owned.as_deref()),
            )
            .await?;

        match response {
            AnyMessage::UserInputResponse(msg) => Ok(msg.text),
            _ => Err(anyhow!("received unexpected response type for text input")),
        }
    }

    /// Request password input (hidden)
    /// Returns the entered password
    pub async fn password(&self, prompt: &str) -> Result<String> {
        let prompt_owned = prompt.to_string();
        let request = UserInputRequest {
            base: BaseMessage::new(MessageCategory::UserInteraction, None),
            prompt: prompt_owned.clone(),
        };

        let response = self
            .request_response(
                request.base.id.clone(),
                AnyMessage::UserInputRequest(request),
                move || read_password_input(&prompt_owned),
            )
            .await?;

        match response {
            AnyMessage::UserInputResponse(msg) => Ok(msg.text),
            _ => Err(anyhow!(
                "received unexpected response type for password input"
            )),
        }
    }

    async fn request_response<F>(
        &self,
        request_id: String,
        request: AnyMessage,
        collect_response: F,
    ) -> Result<AnyMessage>
    where
        F: FnOnce() -> Result<AnyMessage> + Send + 'static,
    {
        let response_rx = self.bus.response_message_for(&request_id);
        self.bus.emit(request);

        // Simple stdin collector for now (Phase 1.2).
        let bus = Arc::clone(&self.bus);
        let (tx, mut rx) = mpsc::channel::<AnyMessage>(1);
        tokio::task::spawn_blocking(move || {
            let result = collect_response();
            match result {
                Ok(msg) => {
                    let _ = tx.blocking_send(msg);
                }
                Err(err) => {
                    tracing::warn!(error = %err, "failed collecting user input");
                }
            }
        });

        tokio::spawn(async move {
            if let Some(response) = rx.recv().await {
                let response = with_request_id(response, &request_id);
                bus.emit(response);
            }
        });

        let message = timeout(DEFAULT_TIMEOUT, response_rx)
            .await
            .context("timed out waiting for user input response")?
            .context("input response channel closed before delivering result")?;

        Ok(message)
    }
}

/// Helper struct to manage pending input requests
pub struct PendingInput {
    pub request_id: String,
    pub response_tx: oneshot::Sender<AnyMessage>,
}

/// Registry for tracking pending input requests
#[derive(Debug)]
pub struct InputRequestRegistry {
    pending: HashMap<String, oneshot::Sender<AnyMessage>>,
}

impl InputRequestRegistry {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
        }
    }

    pub fn register(&mut self, id: String) -> oneshot::Receiver<AnyMessage> {
        let (tx, rx) = oneshot::channel();
        self.pending.insert(id, tx);
        rx
    }

    pub fn respond(&mut self, id: &str, response: AnyMessage) -> Result<()> {
        let tx = self
            .pending
            .remove(id)
            .ok_or_else(|| anyhow!("no pending input request found for id: {id}"))?;

        tx.send(response)
            .map_err(|_| anyhow!("failed to send input response for id: {id}"))
    }
}

impl Default for InputRequestRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn read_confirmation(prompt: &str) -> Result<AnyMessage> {
    let full_prompt = format!("{prompt} [y/N]: ");
    print!("{full_prompt}");
    io::stdout().flush().context("failed to flush stdout")?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read confirmation input")?;

    let normalized = input.trim().to_ascii_lowercase();
    let confirmed = normalized == "y" || normalized == "yes";

    Ok(AnyMessage::ConfirmationResponse(ConfirmationResponse {
        base: BaseMessage::new(MessageCategory::UserInteraction, None),
        confirmed,
    }))
}

fn read_single_selection(prompt: &str, options: &[String]) -> Result<AnyMessage> {
    println!("{prompt}");
    for (idx, option) in options.iter().enumerate() {
        println!("  {}. {}", idx + 1, option);
    }

    print!("Enter number (1-{}): ", options.len());
    io::stdout().flush().context("failed to flush stdout")?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read selection input")?;

    let parsed = input
        .trim()
        .parse::<usize>()
        .context("selection must be a number")?;

    if parsed == 0 || parsed > options.len() {
        return Err(anyhow!("selection out of range"));
    }

    let selected_index = parsed - 1;
    let selected_value = options[selected_index].clone();

    Ok(AnyMessage::SelectionResponse(SelectionResponse {
        base: BaseMessage::new(MessageCategory::UserInteraction, None),
        selected_indices: vec![selected_index],
        selected_values: vec![selected_value],
    }))
}

fn read_multi_selection(prompt: &str, options: &[String]) -> Result<AnyMessage> {
    println!("{prompt}");
    for (idx, option) in options.iter().enumerate() {
        println!("  [ ] {}. {}", idx + 1, option);
    }

    print!("Enter comma-separated numbers (e.g. 1,3): ");
    io::stdout().flush().context("failed to flush stdout")?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read multi-selection input")?;

    let mut selected_indices = Vec::new();
    let mut selected_values = Vec::new();

    for chunk in input
        .trim()
        .split(',')
        .filter(|part| !part.trim().is_empty())
    {
        let parsed = chunk
            .trim()
            .parse::<usize>()
            .with_context(|| format!("invalid selection index: {chunk}"))?;

        if parsed == 0 || parsed > options.len() {
            return Err(anyhow!("selection index out of range: {parsed}"));
        }

        let selected_index = parsed - 1;
        if selected_indices.contains(&selected_index) {
            continue;
        }

        selected_indices.push(selected_index);
        selected_values.push(options[selected_index].clone());
    }

    if selected_indices.is_empty() {
        return Err(anyhow!("at least one selection is required"));
    }

    Ok(AnyMessage::SelectionResponse(SelectionResponse {
        base: BaseMessage::new(MessageCategory::UserInteraction, None),
        selected_indices,
        selected_values,
    }))
}

fn read_text_input(prompt: &str, placeholder: Option<&str>) -> Result<AnyMessage> {
    match placeholder {
        Some(value) if !value.trim().is_empty() => print!("{prompt} ({value}): "),
        _ => print!("{prompt}: "),
    }
    io::stdout().flush().context("failed to flush stdout")?;

    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .context("failed to read text input")?;

    let text = input.trim().to_string();
    let text = if text.is_empty() {
        placeholder.unwrap_or_default().to_string()
    } else {
        text
    };

    Ok(AnyMessage::UserInputResponse(UserInputResponse {
        base: BaseMessage::new(MessageCategory::UserInteraction, None),
        text,
    }))
}

fn with_request_id(mut response: AnyMessage, request_id: &str) -> AnyMessage {
    match &mut response {
        AnyMessage::UserInputResponse(msg) => msg.base.id = request_id.to_string(),
        AnyMessage::ConfirmationResponse(msg) => msg.base.id = request_id.to_string(),
        AnyMessage::SelectionResponse(msg) => msg.base.id = request_id.to_string(),
        _ => {}
    }

    response
}

fn read_password_input(prompt: &str) -> Result<AnyMessage> {
    print!("{prompt}: ");
    io::stdout().flush().context("failed to flush stdout")?;

    let password = rpassword::read_password().context("failed to read password input")?;

    Ok(AnyMessage::UserInputResponse(UserInputResponse {
        base: BaseMessage::new(MessageCategory::UserInteraction, None),
        text: password,
    }))
}
