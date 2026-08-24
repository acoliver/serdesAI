use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;
use crossterm::style::Color;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::input::InputRequestRegistry;
use crate::render::InlineConsole;

use crate::messages::{
    AgentReasoningMessage, AgentResponseMessage, BaseMessage, ConfirmationRequest,
    ConfirmationResponse, DiffMessage, DividerMessage, FileContentMessage, FileListingMessage,
    GrepResultMessage, MessageCategory, MessageLevel, SelectionRequest, SelectionResponse,
    ShellLineMessage, ShellOutputMessage, ShellStartMessage, SkillActivateMessage,
    SkillListMessage, SpinnerControl, StatusPanelMessage, SubAgentInvocationMessage,
    SubAgentResponseMessage, SubAgentStatusMessage, TextMessage, UniversalConstructorMessage,
    UserInputRequest, UserInputResponse, VersionCheckMessage,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AnyMessage {
    Text(TextMessage),
    FileListing(FileListingMessage),
    FileContent(FileContentMessage),
    GrepResult(GrepResultMessage),
    Diff(DiffMessage),
    ShellStart(ShellStartMessage),
    ShellLine(ShellLineMessage),
    ShellOutput(ShellOutputMessage),
    AgentReasoning(AgentReasoningMessage),
    AgentResponse(AgentResponseMessage),
    UserInputRequest(UserInputRequest),
    UserInputResponse(UserInputResponse),
    ConfirmationRequest(ConfirmationRequest),
    ConfirmationResponse(ConfirmationResponse),
    SelectionRequest(SelectionRequest),
    SelectionResponse(SelectionResponse),
    SpinnerControl(SpinnerControl),
    Divider(DividerMessage),
    StatusPanel(StatusPanelMessage),
    SkillList(SkillListMessage),
    SkillActivate(SkillActivateMessage),
    SubAgentInvocation(SubAgentInvocationMessage),
    SubAgentResponse(SubAgentResponseMessage),
    SubAgentStatus(SubAgentStatusMessage),
    VersionCheck(VersionCheckMessage),
    UniversalConstructor(UniversalConstructorMessage),
}

impl AnyMessage {
    pub fn base(&self) -> &BaseMessage {
        match self {
            AnyMessage::Text(m) => &m.base,
            AnyMessage::FileListing(m) => &m.base,
            AnyMessage::FileContent(m) => &m.base,
            AnyMessage::GrepResult(m) => &m.base,
            AnyMessage::Diff(m) => &m.base,
            AnyMessage::ShellStart(m) => &m.base,
            AnyMessage::ShellLine(m) => &m.base,
            AnyMessage::ShellOutput(m) => &m.base,
            AnyMessage::AgentReasoning(m) => &m.base,
            AnyMessage::AgentResponse(m) => &m.base,
            AnyMessage::UserInputRequest(m) => &m.base,
            AnyMessage::UserInputResponse(m) => &m.base,
            AnyMessage::ConfirmationRequest(m) => &m.base,
            AnyMessage::ConfirmationResponse(m) => &m.base,
            AnyMessage::SelectionRequest(m) => &m.base,
            AnyMessage::SelectionResponse(m) => &m.base,
            AnyMessage::SpinnerControl(m) => &m.base,
            AnyMessage::Divider(m) => &m.base,
            AnyMessage::StatusPanel(m) => &m.base,
            AnyMessage::SkillList(m) => &m.base,
            AnyMessage::SkillActivate(m) => &m.base,
            AnyMessage::SubAgentInvocation(m) => &m.base,
            AnyMessage::SubAgentResponse(m) => &m.base,
            AnyMessage::SubAgentStatus(m) => &m.base,
            AnyMessage::VersionCheck(m) => &m.base,
            AnyMessage::UniversalConstructor(m) => &m.base,
        }
    }

    fn base_mut(&mut self) -> &mut BaseMessage {
        match self {
            AnyMessage::Text(m) => &mut m.base,
            AnyMessage::FileListing(m) => &mut m.base,
            AnyMessage::FileContent(m) => &mut m.base,
            AnyMessage::GrepResult(m) => &mut m.base,
            AnyMessage::Diff(m) => &mut m.base,
            AnyMessage::ShellStart(m) => &mut m.base,
            AnyMessage::ShellLine(m) => &mut m.base,
            AnyMessage::ShellOutput(m) => &mut m.base,
            AnyMessage::AgentReasoning(m) => &mut m.base,
            AnyMessage::AgentResponse(m) => &mut m.base,
            AnyMessage::UserInputRequest(m) => &mut m.base,
            AnyMessage::UserInputResponse(m) => &mut m.base,
            AnyMessage::ConfirmationRequest(m) => &mut m.base,
            AnyMessage::ConfirmationResponse(m) => &mut m.base,
            AnyMessage::SelectionRequest(m) => &mut m.base,
            AnyMessage::SelectionResponse(m) => &mut m.base,
            AnyMessage::SpinnerControl(m) => &mut m.base,
            AnyMessage::Divider(m) => &mut m.base,
            AnyMessage::StatusPanel(m) => &mut m.base,
            AnyMessage::SkillList(m) => &mut m.base,
            AnyMessage::SkillActivate(m) => &mut m.base,
            AnyMessage::SubAgentInvocation(m) => &mut m.base,
            AnyMessage::SubAgentResponse(m) => &mut m.base,
            AnyMessage::SubAgentStatus(m) => &mut m.base,
            AnyMessage::VersionCheck(m) => &mut m.base,
            AnyMessage::UniversalConstructor(m) => &mut m.base,
        }
    }
}

#[derive(Debug)]
struct BusState {
    message_queue: VecDeque<AnyMessage>,
    buffered_messages: Vec<AnyMessage>,
    renderer_active: bool,
    session_context: Option<String>,
    renderer: Arc<Mutex<InlineConsole>>,
}

impl Default for BusState {
    fn default() -> Self {
        Self {
            message_queue: VecDeque::new(),
            buffered_messages: Vec::new(),
            renderer_active: false,
            session_context: None,
            renderer: Arc::new(Mutex::new(InlineConsole::new())),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessageBus {
    state: Arc<Mutex<BusState>>,
    input_registry: Arc<Mutex<InputRequestRegistry>>,
}

impl Default for MessageBus {
    fn default() -> Self {
        Self::new()
    }
}

impl MessageBus {
    pub fn new() -> Self {
        let renderer = Arc::new(Mutex::new(InlineConsole::new()));
        Self {
            state: Arc::new(Mutex::new(BusState {
                renderer,
                ..BusState::default()
            })),
            input_registry: Arc::new(Mutex::new(InputRequestRegistry::new())),
        }
    }

    pub fn emit(&self, mut message: AnyMessage) {
        let renderer = {
            let mut state = self
                .state
                .lock()
                .expect("message bus lock poisoned while emitting message");

            if message.base_mut().session_id.is_none() {
                message.base_mut().session_id = state.session_context.clone();
            }

            // Keep history for replay/debugging.
            state.buffered_messages.push(message.clone());
            // Keep queue behavior for any remaining consumers.
            state.message_queue.push_back(message.clone());

            Arc::clone(&state.renderer)
        };

        self.try_route_input_response(&message);
        self.render_inline(renderer, &message);
    }

    pub fn emit_text(&self, level: MessageLevel, text: String) {
        let message = TextMessage {
            base: BaseMessage::with_category(MessageCategory::System),
            level,
            text,
        };
        self.emit(AnyMessage::Text(message));
    }

    pub fn get_message_nowait(&self) -> Option<AnyMessage> {
        let mut state = self
            .state
            .lock()
            .expect("message bus lock poisoned while receiving message");
        state.message_queue.pop_front()
    }

    pub async fn get_message(&self) -> AnyMessage {
        loop {
            if let Some(message) = self.get_message_nowait() {
                return message;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    pub fn get_buffered_messages(&self) -> Vec<AnyMessage> {
        let state = self
            .state
            .lock()
            .expect("message bus lock poisoned while reading buffer");
        state.buffered_messages.clone()
    }

    pub fn clear_buffer(&self) {
        let mut state = self
            .state
            .lock()
            .expect("message bus lock poisoned while clearing buffer");
        state.buffered_messages.clear();
    }

    pub fn mark_renderer_active(&self) {
        let mut state = self
            .state
            .lock()
            .expect("message bus lock poisoned while marking renderer active");
        state.renderer_active = true;
    }

    pub fn mark_renderer_inactive(&self) {
        let mut state = self
            .state
            .lock()
            .expect("message bus lock poisoned while marking renderer inactive");
        state.renderer_active = false;
    }

    pub fn set_session_context(&self, session_id: String) {
        let mut state = self
            .state
            .lock()
            .expect("message bus lock poisoned while setting session context");
        state.session_context = Some(session_id);
    }

    pub fn get_session_context(&self) -> Option<String> {
        let state = self
            .state
            .lock()
            .expect("message bus lock poisoned while getting session context");
        state.session_context.clone()
    }

    pub fn reset_session_context(&self) {
        let mut state = self
            .state
            .lock()
            .expect("message bus lock poisoned while resetting session context");
        state.session_context = None;
    }

    fn render_inline(&self, renderer: Arc<Mutex<InlineConsole>>, message: &AnyMessage) {
        let mut renderer = renderer
            .lock()
            .expect("inline renderer lock poisoned while rendering message");

        match message {
            AnyMessage::Text(msg) => {
                let _ = renderer.print(&msg.text, Self::level_color(&msg.level));
                let _ = renderer.print("\n", None);
            }
            AnyMessage::ShellStart(msg) => {
                let content = format!("$ {}\nwd: {}", msg.command, msg.cwd);
                let _ = renderer.print_panel("Shell Command", &content, Color::Cyan);
            }
            AnyMessage::ShellLine(msg) => {
                let _ = renderer.print(&msg.line, None);
                let _ = renderer.print("\n", None);
            }
            AnyMessage::FileContent(msg) => {
                let _ =
                    renderer.print_panel(&format!("File: {}", msg.path), &msg.content, Color::Blue);
            }
            AnyMessage::StatusPanel(msg) => {
                let style = match msg.status_type {
                    crate::messages::StatusType::Error => "error",
                    crate::messages::StatusType::Warning => "warning",
                    crate::messages::StatusType::Success => "success",
                    crate::messages::StatusType::Info => "info",
                };
                let text = format!("{}: {}", msg.title, msg.content);
                let _ = renderer.print_status(&text, style);
            }
            AnyMessage::AgentReasoning(msg) => {
                let _ = renderer.print_panel("Agent Reasoning", &msg.reasoning, Color::DarkGrey);
            }
            AnyMessage::AgentResponse(msg) => {
                let _ = renderer.print(&msg.content, None);
                let _ = renderer.print("\n", None);
            }
            AnyMessage::Diff(msg) => {
                let content = format!("{}\n+{} -{}", msg.file_path, msg.additions, msg.deletions);
                let _ = renderer.print_panel("Diff", &content, Color::Green);
            }
            AnyMessage::GrepResult(msg) => {
                let content = format!(
                    "directory: {}\npattern: {}\nmatches: {}",
                    msg.directory,
                    msg.search_term,
                    msg.matches.len()
                );
                let _ = renderer.print_panel("Grep", &content, Color::Magenta);
            }
            AnyMessage::FileListing(msg) => {
                let content = format!(
                    "{}\nfiles: {} dirs: {} size: {} bytes",
                    msg.directory, msg.file_count, msg.dir_count, msg.total_size
                );
                let _ = renderer.print_panel("Directory Listing", &content, Color::Yellow);
            }
            AnyMessage::Divider(msg) => {
                let title = msg.title.as_deref().unwrap_or("────────────────");
                let _ = renderer.print(&format!("\n{title}\n"), Some(Color::DarkGrey));
            }
            _ => {}
        }
    }

    fn level_color(level: &MessageLevel) -> Option<Color> {
        match level {
            MessageLevel::Error => Some(Color::Red),
            MessageLevel::Warning => Some(Color::Yellow),
            MessageLevel::Success => Some(Color::Green),
            MessageLevel::Info => Some(Color::White),
            MessageLevel::Debug => Some(Color::Grey),
        }
    }
}

impl MessageBus {
    pub fn emit_info(&self, text: impl Into<String>) {
        self.emit_text(MessageLevel::Info, text.into());
    }

    pub fn emit_error(&self, text: impl Into<String>) {
        self.emit_text(MessageLevel::Error, text.into());
    }

    pub fn emit_warning(&self, text: impl Into<String>) {
        self.emit_text(MessageLevel::Warning, text.into());
    }

    pub fn emit_success(&self, text: impl Into<String>) {
        self.emit_text(MessageLevel::Success, text.into());
    }

    pub fn emit_debug(&self, text: impl Into<String>) {
        self.emit_text(MessageLevel::Debug, text.into());
    }

    pub fn emit_shell_line(&self, command_id: impl Into<String>, line: impl Into<String>) {
        let message = ShellLineMessage {
            base: BaseMessage::with_category(MessageCategory::ToolOutput),
            command_id: command_id.into(),
            line: line.into(),
        };
        self.emit(AnyMessage::ShellLine(message));
    }

    pub fn register_input_request(&self, request_id: String) -> oneshot::Receiver<AnyMessage> {
        let mut registry = self
            .input_registry
            .lock()
            .expect("input registry lock poisoned while registering request");
        registry.register(request_id)
    }

    pub fn respond_to_input_request(&self, request_id: &str, response: AnyMessage) -> Result<()> {
        let mut registry = self
            .input_registry
            .lock()
            .expect("input registry lock poisoned while responding to request");
        registry.respond(request_id, response)
    }

    pub fn response_message_for(&self, request_id: &str) -> oneshot::Receiver<AnyMessage> {
        self.register_input_request(request_id.to_string())
    }

    fn try_route_input_response(&self, message: &AnyMessage) {
        if !matches!(
            message,
            AnyMessage::UserInputResponse(_)
                | AnyMessage::ConfirmationResponse(_)
                | AnyMessage::SelectionResponse(_)
        ) {
            return;
        }

        let request_id = message.base().id.clone();
        let mut registry = self
            .input_registry
            .lock()
            .expect("input registry lock poisoned while routing response");

        if let Err(err) = registry.respond(&request_id, message.clone()) {
            tracing::debug!(
                request_id = %request_id,
                error = %err,
                "received input response without pending waiter"
            );
        }
    }
}

static GLOBAL_BUS: OnceLock<Arc<MessageBus>> = OnceLock::new();

pub fn get_message_bus() -> Arc<MessageBus> {
    GLOBAL_BUS
        .get_or_init(|| Arc::new(MessageBus::new()))
        .clone()
}

pub fn reset_message_bus() {
    if let Some(bus) = GLOBAL_BUS.get() {
        let mut state = bus
            .state
            .lock()
            .expect("message bus lock poisoned while resetting global bus");
        state.message_queue.clear();
        state.buffered_messages.clear();
        state.renderer_active = false;
        state.session_context = None;
        state.renderer = Arc::new(Mutex::new(InlineConsole::new()));
    }
}

pub fn emit(message: AnyMessage) {
    get_message_bus().emit(message);
}

pub fn emit_info(text: impl Into<String>) {
    get_message_bus().emit_info(text);
}

pub fn emit_error(text: impl Into<String>) {
    get_message_bus().emit_error(text);
}

pub fn emit_warning(text: impl Into<String>) {
    get_message_bus().emit_warning(text);
}

pub fn emit_success(text: impl Into<String>) {
    get_message_bus().emit_success(text);
}

pub fn emit_debug(text: impl Into<String>) {
    get_message_bus().emit_debug(text);
}

pub fn emit_shell_line(command_id: impl Into<String>, line: impl Into<String>) {
    get_message_bus().emit_shell_line(command_id, line);
}
