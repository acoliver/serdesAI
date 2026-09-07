use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum MessageLevel {
    Debug,
    Info,
    Warning,
    Error,
    Success,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageCategory {
    System,
    ToolOutput,
    Agent,
    UserInteraction,
    Divider,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaseMessage {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub category: MessageCategory,
    pub session_id: Option<String>,
}

impl BaseMessage {
    pub fn new(category: MessageCategory, session_id: Option<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            category,
            session_id,
        }
    }

    pub fn with_category(category: MessageCategory) -> Self {
        Self::new(category, None)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextMessage {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub level: MessageLevel,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileEntryType {
    File,
    Dir,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    #[serde(rename = "type")]
    pub entry_type: FileEntryType,
    pub size: u64,
    pub depth: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileListingMessage {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub directory: String,
    pub files: Vec<FileEntry>,
    pub recursive: bool,
    pub total_size: u64,
    pub dir_count: u32,
    pub file_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContentMessage {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub path: String,
    pub content: String,
    pub start_line: Option<u32>,
    pub num_lines: Option<u32>,
    pub total_lines: u32,
    pub num_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepMatch {
    pub file_path: String,
    pub line_number: u32,
    pub line_content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrepResultMessage {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub search_term: String,
    pub directory: String,
    pub matches: Vec<GrepMatch>,
    pub verbose: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffLineType {
    Add,
    Remove,
    Context,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffLine {
    pub line_type: DiffLineType,
    pub content: String,
    pub old_line_num: Option<u32>,
    pub new_line_num: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffMessage {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub file_path: String,
    pub lines: Vec<DiffLine>,
    pub additions: u32,
    pub deletions: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellStartMessage {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub command: String,
    pub cwd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellLineMessage {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub command_id: String,
    pub line: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellOutputMessage {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub command_id: String,
    pub output: String,
    pub success: bool,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentReasoningMessage {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub reasoning: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponseMessage {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub content: String,
    pub is_markdown: bool,
    pub is_streaming: bool,
}

impl AgentResponseMessage {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            base: BaseMessage::new(MessageCategory::Agent, None),
            content: content.into(),
            is_markdown: true,
            is_streaming: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInputRequest {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfirmationRequest {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionRequest {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub prompt: String,
    pub options: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfirmationResponse {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub confirmed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionResponse {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub selected_indices: Vec<usize>,
    pub selected_values: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInputResponse {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpinnerAction {
    Start,
    Stop,
    Update,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpinnerControl {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub spinner_id: String,
    pub action: SpinnerAction,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DividerMessage {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StatusType {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusPanelMessage {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub title: String,
    pub content: String,
    pub status_type: StatusType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentInvocationMessage {
    #[serde(flatten)]
    pub base: BaseMessage,
    /// Identifies this agent instance.
    ///
    /// A role name alone cannot tell three concurrent verifiers apart, so
    /// display state is keyed off this rather than off `agent_name`.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// The agent that spawned this one, if any.
    #[serde(default)]
    pub parent_id: Option<String>,
    pub agent_name: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentResponseMessage {
    #[serde(flatten)]
    pub base: BaseMessage,
    /// Identifies which agent instance replied.
    #[serde(default)]
    pub agent_id: Option<String>,
    pub agent_name: String,
    pub response: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubAgentStatus {
    Starting,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentStatusMessage {
    #[serde(flatten)]
    pub base: BaseMessage,
    /// Identifies which agent instance changed state.
    #[serde(default)]
    pub agent_id: Option<String>,
    pub agent_name: String,
    pub status: SubAgentStatus,
    pub progress: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UniversalConstructorMessage {
    #[serde(flatten)]
    pub base: BaseMessage,
    pub code: String,
    pub language: String,
}
