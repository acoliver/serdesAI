use std::any::{type_name, TypeId};
use std::collections::HashMap;
use std::io::{stderr, stdout, Stdout, Write};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor};
use crossterm::{ExecutableCommand, QueueableCommand};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::bus::{AnyMessage, MessageBus};
use crate::config;
use crate::messages::{
    AgentReasoningMessage, DiffMessage, DividerMessage, FileContentMessage, FileListingMessage,
    GrepResultMessage, MessageLevel, ShellLineMessage, ShellOutputMessage, ShellStartMessage,
    SpinnerAction, SpinnerControl, StatusPanelMessage, StatusType, SubAgentInvocationMessage,
    TextMessage, UniversalConstructorMessage,
};

pub const DEFAULT_STYLES: &[(MessageLevel, &str)] = &[
    (MessageLevel::Error, "bold red"),
    (MessageLevel::Warning, "yellow"),
    (MessageLevel::Success, "green"),
    (MessageLevel::Info, "white"),
    (MessageLevel::Debug, "dim"),
];

pub const DIFF_STYLES: &[(&str, &str)] = &[("add", "green"), ("remove", "red"), ("context", "dim")];

const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Debug, Clone, Default)]
pub struct SpinnerState {
    pub active: bool,
    pub message: Option<String>,
    pub frame_index: usize,
}

pub struct RichConsoleRendererV2 {
    bus: Arc<MessageBus>,
    console: Option<Terminal<CrosstermBackend<Stdout>>>,
    styles: HashMap<MessageLevel, String>,
    running: bool,
    thread: Option<thread::JoinHandle<()>>,
    spinners: HashMap<String, SpinnerState>,
    last_rendered_type: Option<TypeId>,
    reasoning_banner_rendered: bool,
}

impl RichConsoleRendererV2 {
    pub fn new(bus: Arc<MessageBus>) -> Self {
        let backend = CrosstermBackend::new(stdout());
        let console = Terminal::new(backend).ok();

        let styles = DEFAULT_STYLES
            .iter()
            .map(|(lvl, style)| (lvl.clone(), (*style).to_string()))
            .collect();

        Self {
            bus,
            console,
            styles,
            running: false,
            thread: None,
            spinners: HashMap::new(),
            last_rendered_type: None,
            reasoning_banner_rendered: false,
        }
    }

    pub fn start(&mut self) {
        if self.running {
            return;
        }

        self.running = true;
        self.bus.mark_renderer_active();

        let bus = Arc::clone(&self.bus);

        self.thread = Some(thread::spawn(move || {
            let mut renderer = RichConsoleRendererV2::new(bus);
            renderer.running = true;
            renderer.consume_loop_sync();
        }));
    }

    pub fn stop(&mut self) {
        self.running = false;
        self.bus.mark_renderer_inactive();

        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }

    fn consume_loop_sync(&mut self) {
        for msg in self.bus.get_buffered_messages() {
            self.render_sync(msg);
        }
        self.bus.clear_buffer();

        while self.running {
            if let Some(message) = self.bus.get_message_nowait() {
                self.render_sync(message);
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn render_sync(&mut self, message: AnyMessage) {
        if let Err(error) = self.do_render(&message) {
            self.last_rendered_type = None;
            self.print_line(
                "dim red",
                &format!("Render error: {}", Self::escape_markup(&error.to_string())),
            );
            return;
        }

        let msg_type = Self::message_type_id(&message);
        if !Self::is_transparent_type(msg_type) {
            self.last_rendered_type = Some(msg_type);
        }
    }

    fn do_render(&mut self, message: &AnyMessage) -> anyhow::Result<()> {
        match message {
            AnyMessage::Text(msg) => self.render_text(msg),
            AnyMessage::FileListing(msg) => self.render_file_listing(msg),
            AnyMessage::FileContent(msg) => self.render_file_content(msg),
            AnyMessage::GrepResult(msg) => self.render_grep_result(msg),
            AnyMessage::Diff(msg) => self.render_diff(msg),
            AnyMessage::ShellStart(msg) => self.render_shell_start(msg),
            AnyMessage::ShellLine(msg) => self.render_shell_line(msg),
            AnyMessage::ShellOutput(msg) => self.render_shell_output(msg),
            AnyMessage::AgentReasoning(msg) => self.render_agent_reasoning(msg),
            AnyMessage::StatusPanel(msg) => self.render_status_panel(msg),
            AnyMessage::UniversalConstructor(msg) => self.render_universal_constructor(msg),
            AnyMessage::SubAgentInvocation(msg) => self.render_subagent_invocation(msg),
            AnyMessage::SpinnerControl(msg) => self.render_spinner_control(msg),

            // Streaming handled elsewhere
            AnyMessage::AgentResponse(_) => Ok(()),

            // Skip async user-interaction flows in sync renderer
            AnyMessage::UserInputRequest(_)
            | AnyMessage::ConfirmationRequest(_)
            | AnyMessage::SelectionRequest(_) => Ok(()),

            AnyMessage::Divider(msg) => self.render_divider(msg),

            // Not implemented yet in this core pass
            _ => {
                self.print_line(
                    "dim",
                    &format!(
                        "Unknown/unhandled message in v2 renderer: {}",
                        Self::message_type_name(message)
                    ),
                );
                Ok(())
            }
        }
    }

    fn is_continuation(&self, msg_type: TypeId) -> bool {
        Self::is_groupable_type(msg_type) && self.last_rendered_type == Some(msg_type)
    }

    fn is_groupable_type(msg_type: TypeId) -> bool {
        msg_type == TypeId::of::<FileContentMessage>()
            || msg_type == TypeId::of::<GrepResultMessage>()
            || msg_type == TypeId::of::<DiffMessage>()
            || msg_type == TypeId::of::<FileListingMessage>()
            || msg_type == TypeId::of::<ShellStartMessage>()
    }

    fn is_transparent_type(msg_type: TypeId) -> bool {
        msg_type == TypeId::of::<SpinnerControl>()
    }

    fn should_suppress_subagent_output(&self) -> bool {
        // Rust port currently has no explicit `is_subagent()` runtime hook.
        // Keep logic equivalent enough for now: only render fully when verbose is on.
        // Later task can wire real subagent context.
        !config::get_subagent_verbose()
    }

    fn get_level_prefix(level: MessageLevel) -> &'static str {
        match level {
            MessageLevel::Error => "✗ ",
            MessageLevel::Warning => "⚠ ",
            MessageLevel::Success => "✓ ",
            MessageLevel::Info => "ℹ ",
            MessageLevel::Debug => "• ",
        }
    }

    fn format_size(size: u64) -> String {
        const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

        if size < 1024 {
            return format!("{} B", size);
        }

        let mut value = size as f64;
        let mut unit_idx = 0;
        while value >= 1024.0 && unit_idx < UNITS.len() - 1 {
            value /= 1024.0;
            unit_idx += 1;
        }

        format!("{value:.1} {}", UNITS[unit_idx])
    }

    fn get_file_icon(path: &str) -> &'static str {
        if path.ends_with(".rs") {
            "🦀"
        } else if path.ends_with(".py") {
            "🐍"
        } else if path.ends_with(".md") {
            "📝"
        } else if path.ends_with(".json") {
            "🧾"
        } else if path.ends_with(".toml") {
            "⚙️"
        } else {
            "📄"
        }
    }

    fn format_banner(&self, banner_name: &str, text: &str) -> String {
        let color = config::get_banner_color(banner_name);
        let total_width = 50usize;
        let pad_len = total_width.saturating_sub(text.chars().count() + 4).max(1);
        let rule = "─".repeat(pad_len);
        format!(
            "[{}]▸[/{}] [bold]{}[/bold] [{}]{}[/{}]",
            color, color, text, color, rule, color
        )
    }

    fn render_text(&mut self, msg: &TextMessage) -> anyhow::Result<()> {
        let mut style_key = self
            .styles
            .get(&msg.level)
            .cloned()
            .unwrap_or_else(|| "white".to_string());

        if msg.text.contains("Current version:") || msg.text.contains("Latest version:") {
            style_key = "dim".to_string();
        }

        let prefix = Self::get_level_prefix(msg.level.clone());
        let safe_text = Self::escape_markup(&msg.text);
        self.print_line(&style_key, &format!("{}{}", prefix, safe_text));
        Ok(())
    }

    fn render_file_listing(&mut self, msg: &FileListingMessage) -> anyhow::Result<()> {
        if self.should_suppress_subagent_output() {
            return Ok(());
        }

        if !self.is_continuation(TypeId::of::<FileListingMessage>()) {
            let banner = self.format_banner("directory_listing", "DIRECTORY LISTING");
            self.print_markup_line(&format!("\n{}", banner));
        }

        self.print_markup_line(&format!(
            "  ├─ 📂 [bold cyan]{}[/bold cyan] [dim](recursive={})[/dim]",
            Self::escape_markup(&msg.directory),
            msg.recursive
        ));

        self.print_markup_line(&format!(
            "  │     [dim]Summary: {} directories, {} files ({})[/dim]",
            msg.dir_count,
            msg.file_count,
            Self::format_size(msg.total_size)
        ));

        Ok(())
    }

    fn render_file_content(&mut self, msg: &FileContentMessage) -> anyhow::Result<()> {
        if self.should_suppress_subagent_output() {
            return Ok(());
        }

        let line_info = match (msg.start_line, msg.num_lines) {
            (Some(start), Some(count)) if count > 0 => {
                let end = start + count - 1;
                format!(" [dim](lines {}-{})[/dim]", start, end)
            }
            _ => String::new(),
        };

        if !self.is_continuation(TypeId::of::<FileContentMessage>()) {
            let banner = self.format_banner("read_file", "READ FILE");
            self.print_markup_line(&format!("\n{}", banner));
        }

        self.print_markup_line(&format!(
            "  ├─ 📂 [bold cyan]{}[/bold cyan]{}",
            Self::escape_markup(&msg.path),
            line_info
        ));

        Ok(())
    }

    fn render_grep_result(&mut self, msg: &GrepResultMessage) -> anyhow::Result<()> {
        if self.should_suppress_subagent_output() {
            return Ok(());
        }

        if !self.is_continuation(TypeId::of::<GrepResultMessage>()) {
            let banner = self.format_banner("grep", "GREP");
            self.print_markup_line(&format!("\n{}", banner));
        }

        self.print_markup_line(&format!(
            "  ├─ 📂 [dim]{} for '{}'[/dim]",
            Self::escape_markup(&msg.directory),
            Self::escape_markup(&msg.search_term)
        ));

        if msg.matches.is_empty() {
            self.print_markup_line("  │     [dim]No matches found[/dim]");
            return Ok(());
        }

        self.print_markup_line(&format!(
            "  │     [dim]Found {} matches[/dim]",
            msg.matches.len()
        ));

        Ok(())
    }

    fn render_diff(&mut self, msg: &DiffMessage) -> anyhow::Result<()> {
        if self.should_suppress_subagent_output() {
            return Ok(());
        }

        if !self.is_continuation(TypeId::of::<DiffMessage>()) {
            let banner = self.format_banner("edit_file", "EDIT FILE");
            self.print_markup_line(&format!("\n{}", banner));
        }

        self.print_markup_line(&format!(
            "  ├─ ✏️ [bold cyan]{}[/bold cyan] [green]+{}[/green] [red]-{}[/red]",
            Self::escape_markup(&msg.file_path),
            msg.additions,
            msg.deletions
        ));

        Ok(())
    }

    fn render_shell_start(&mut self, msg: &ShellStartMessage) -> anyhow::Result<()> {
        if self.should_suppress_subagent_output() {
            return Ok(());
        }

        if !self.is_continuation(TypeId::of::<ShellStartMessage>()) {
            let banner = self.format_banner("shell_command", "SHELL COMMAND");
            self.print_markup_line(&format!("\n{}", banner));
        }

        self.print_markup_line(&format!(
            "  ├─ 🚀 [dim]$ {}[/dim]",
            Self::escape_markup(&msg.command)
        ));
        self.print_markup_line(&format!(
            "  │  [dim]📂 Working directory: {}[/dim]",
            Self::escape_markup(&msg.cwd)
        ));

        Ok(())
    }

    fn render_shell_line(&mut self, msg: &ShellLineMessage) -> anyhow::Result<()> {
        self.print_line("dim", &Self::escape_markup(&msg.line));
        Ok(())
    }

    fn render_shell_output(&mut self, _msg: &ShellOutputMessage) -> anyhow::Result<()> {
        self.print_plain_line("");
        Ok(())
    }

    fn render_spinner_control(&mut self, msg: &SpinnerControl) -> anyhow::Result<()> {
        match msg.action {
            SpinnerAction::Start => self.start_spinner(&msg.spinner_id, msg.message.clone()),
            SpinnerAction::Update => {
                if let Some(state) = self.spinners.get_mut(&msg.spinner_id) {
                    if let Some(message) = &msg.message {
                        state.message = Some(message.clone());
                    }
                } else {
                    self.start_spinner(&msg.spinner_id, msg.message.clone())?;
                    return Ok(());
                }
                self.print_spinner_frame(&msg.spinner_id)
            }
            SpinnerAction::Stop => {
                if let Some(message) = &msg.message {
                    if let Some(state) = self.spinners.get_mut(&msg.spinner_id) {
                        state.message = Some(message.clone());
                    }
                }
                self.stop_spinner(&msg.spinner_id)
            }
        }
    }

    fn render_status_panel(&mut self, msg: &StatusPanelMessage) -> anyhow::Result<()> {
        let (style, icon) = match msg.status_type {
            StatusType::Info => ("cyan", "ℹ"),
            StatusType::Success => ("green", "✓"),
            StatusType::Warning => ("yellow", "⚠"),
            StatusType::Error => ("red", "✗"),
        };

        let title = format!("{} {}", icon, msg.title.trim());
        let content_lines: Vec<String> = if msg.content.trim().is_empty() {
            vec![String::from("(empty)")]
        } else {
            msg.content.lines().map(|line| line.to_string()).collect()
        };

        let inner_width = content_lines
            .iter()
            .map(|line| line.chars().count())
            .chain(std::iter::once(title.chars().count()))
            .max()
            .unwrap_or(0)
            .max(20);

        let top = format!("┌{}┐", "─".repeat(inner_width + 2));
        let title_line = format!(
            "│ {:<width$} │",
            Self::escape_markup(&title),
            width = inner_width
        );
        let divider = format!("├{}┤", "─".repeat(inner_width + 2));
        let bottom = format!("└{}┘", "─".repeat(inner_width + 2));

        self.print_line(style, &top);
        self.print_line(style, &title_line);
        self.print_line(style, &divider);
        for line in content_lines {
            let safe = Self::escape_markup(&line);
            self.print_line(style, &format!("│ {:<width$} │", safe, width = inner_width));
        }
        self.print_line(style, &bottom);

        Ok(())
    }

    fn render_universal_constructor(
        &mut self,
        msg: &UniversalConstructorMessage,
    ) -> anyhow::Result<()> {
        let banner = self.format_banner("tool_output", "UNIVERSAL CONSTRUCTOR");
        self.print_markup_line(&format!("\n{}", banner));

        self.print_line(
            "cyan",
            &format!(
                "Language: {} (syntax hint)",
                Self::escape_markup(&msg.language)
            ),
        );
        self.print_plain_line("┌──────────────────────────────────────────────────");

        for (idx, line) in msg.code.lines().enumerate() {
            self.print_line(
                "dim",
                &format!("{:>4} │ {}", idx + 1, Self::escape_markup(line)),
            );
        }

        self.print_plain_line("└──────────────────────────────────────────────────");
        Ok(())
    }

    fn render_subagent_invocation(
        &mut self,
        msg: &SubAgentInvocationMessage,
    ) -> anyhow::Result<()> {
        self.print_line(
            "magenta",
            &format!(
                "Invoking sub-agent: {}",
                Self::escape_markup(&msg.agent_name)
            ),
        );

        let flattened_prompt = msg.prompt.replace('\n', " ");
        let preview = if flattened_prompt.chars().count() > 120 {
            format!(
                "{}…",
                flattened_prompt.chars().take(119).collect::<String>()
            )
        } else {
            flattened_prompt
        };

        self.print_line("dim", &format!("Prompt: {}", Self::escape_markup(&preview)));
        Ok(())
    }

    fn render_agent_reasoning(&mut self, msg: &AgentReasoningMessage) -> anyhow::Result<()> {
        if !self.reasoning_banner_rendered {
            let banner = self.format_banner("reasoning", "AGENT REASONING");
            self.print_markup_line(&format!("\n{}", banner));
            self.reasoning_banner_rendered = true;
        }

        for line in msg.reasoning.lines() {
            if line.trim().is_empty() {
                self.print_plain_line("");
                continue;
            }
            self.print_line("dim", &format!("🧠 {}", Self::escape_markup(line)));
        }

        Ok(())
    }

    fn render_divider(&mut self, msg: &DividerMessage) -> anyhow::Result<()> {
        match msg.title.as_deref() {
            Some(title) if !title.trim().is_empty() => {
                let safe = Self::escape_markup(title.trim());
                self.print_markup_line(&format!("\n[dim]── {} ──[/dim]", safe));
            }
            _ => self.print_markup_line(
                "\n[dim]──────────────────────────────────────────────────[/dim]",
            ),
        }
        Ok(())
    }

    fn print_spinner_frame(&mut self, spinner_id: &str) -> anyhow::Result<()> {
        let Some(state) = self.spinners.get_mut(spinner_id) else {
            return Ok(());
        };

        if !state.active {
            return Ok(());
        }

        let frame = SPINNER_FRAMES[state.frame_index % SPINNER_FRAMES.len()];
        state.frame_index = (state.frame_index + 1) % SPINNER_FRAMES.len();

        let message = state.message.clone().unwrap_or_default();

        let mut err = stderr();
        if message.trim().is_empty() {
            write!(err, "\r{}", frame)?;
        } else {
            write!(err, "\r{} {}", frame, Self::escape_markup(&message))?;
        }
        err.flush()?;

        Ok(())
    }

    fn start_spinner(&mut self, id: &str, message: Option<String>) -> anyhow::Result<()> {
        self.spinners.insert(
            id.to_string(),
            SpinnerState {
                active: true,
                message,
                frame_index: 0,
            },
        );

        self.print_spinner_frame(id)
    }

    fn stop_spinner(&mut self, id: &str) -> anyhow::Result<()> {
        let message = self
            .spinners
            .get(id)
            .and_then(|state| state.message.clone())
            .unwrap_or_default();

        if let Some(state) = self.spinners.get_mut(id) {
            state.active = false;
        }

        let mut err = stderr();
        if message.trim().is_empty() {
            writeln!(err, "\r✓")?;
        } else {
            writeln!(err, "\r✓ {}", Self::escape_markup(&message))?;
        }
        err.flush()?;

        Ok(())
    }

    fn message_type_id(message: &AnyMessage) -> TypeId {
        match message {
            AnyMessage::Text(_) => TypeId::of::<TextMessage>(),
            AnyMessage::FileListing(_) => TypeId::of::<FileListingMessage>(),
            AnyMessage::FileContent(_) => TypeId::of::<FileContentMessage>(),
            AnyMessage::GrepResult(_) => TypeId::of::<GrepResultMessage>(),
            AnyMessage::Diff(_) => TypeId::of::<DiffMessage>(),
            AnyMessage::ShellStart(_) => TypeId::of::<ShellStartMessage>(),
            AnyMessage::ShellLine(_) => TypeId::of::<ShellLineMessage>(),
            AnyMessage::ShellOutput(_) => TypeId::of::<ShellOutputMessage>(),
            AnyMessage::SpinnerControl(_) => TypeId::of::<SpinnerControl>(),
            AnyMessage::Divider(_) => TypeId::of::<DividerMessage>(),
            AnyMessage::AgentResponse(_) => TypeId::of::<crate::messages::AgentResponseMessage>(),
            AnyMessage::UserInputRequest(_) => TypeId::of::<crate::messages::UserInputRequest>(),
            AnyMessage::UserInputResponse(_) => TypeId::of::<crate::messages::UserInputResponse>(),
            AnyMessage::ConfirmationRequest(_) => {
                TypeId::of::<crate::messages::ConfirmationRequest>()
            }
            AnyMessage::ConfirmationResponse(_) => {
                TypeId::of::<crate::messages::ConfirmationResponse>()
            }
            AnyMessage::SelectionRequest(_) => TypeId::of::<crate::messages::SelectionRequest>(),
            AnyMessage::SelectionResponse(_) => TypeId::of::<crate::messages::SelectionResponse>(),
            AnyMessage::AgentReasoning(_) => TypeId::of::<crate::messages::AgentReasoningMessage>(),
            AnyMessage::StatusPanel(_) => TypeId::of::<crate::messages::StatusPanelMessage>(),
            AnyMessage::SubAgentInvocation(_) => {
                TypeId::of::<crate::messages::SubAgentInvocationMessage>()
            }
            AnyMessage::SubAgentResponse(_) => {
                TypeId::of::<crate::messages::SubAgentResponseMessage>()
            }
            AnyMessage::SubAgentStatus(_) => TypeId::of::<crate::messages::SubAgentStatusMessage>(),
            AnyMessage::UniversalConstructor(_) => {
                TypeId::of::<crate::messages::UniversalConstructorMessage>()
            }
        }
    }

    fn message_type_name(message: &AnyMessage) -> &'static str {
        match message {
            AnyMessage::Text(_) => type_name::<TextMessage>(),
            AnyMessage::FileListing(_) => type_name::<FileListingMessage>(),
            AnyMessage::FileContent(_) => type_name::<FileContentMessage>(),
            AnyMessage::GrepResult(_) => type_name::<GrepResultMessage>(),
            AnyMessage::Diff(_) => type_name::<DiffMessage>(),
            AnyMessage::ShellStart(_) => type_name::<ShellStartMessage>(),
            AnyMessage::ShellLine(_) => type_name::<ShellLineMessage>(),
            AnyMessage::ShellOutput(_) => type_name::<ShellOutputMessage>(),
            AnyMessage::SpinnerControl(_) => type_name::<SpinnerControl>(),
            AnyMessage::Divider(_) => type_name::<DividerMessage>(),
            AnyMessage::AgentReasoning(_) => "AgentReasoningMessage",
            AnyMessage::AgentResponse(_) => "AgentResponseMessage",
            AnyMessage::UserInputRequest(_) => "UserInputRequest",
            AnyMessage::UserInputResponse(_) => "UserInputResponse",
            AnyMessage::ConfirmationRequest(_) => "ConfirmationRequest",
            AnyMessage::ConfirmationResponse(_) => "ConfirmationResponse",
            AnyMessage::SelectionRequest(_) => "SelectionRequest",
            AnyMessage::SelectionResponse(_) => "SelectionResponse",
            AnyMessage::StatusPanel(_) => "StatusPanelMessage",
            AnyMessage::SubAgentInvocation(_) => "SubAgentInvocationMessage",
            AnyMessage::SubAgentResponse(_) => "SubAgentResponseMessage",
            AnyMessage::SubAgentStatus(_) => "SubAgentStatusMessage",
            AnyMessage::UniversalConstructor(_) => "UniversalConstructorMessage",
        }
    }

    fn escape_markup(input: &str) -> String {
        input
            .replace('[', "\\[")
            .replace(']', "\\]")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    fn print_line(&mut self, style: &str, line: &str) {
        let (fg, bold, dim) = Self::parse_style(style);

        // Keep this field used and ready for future TUI draws.
        let _ = self.console.as_ref();

        let mut out = stdout();
        let _ = out.execute(SetForegroundColor(fg));
        if bold {
            let _ = out.execute(SetAttribute(Attribute::Bold));
        }
        if dim {
            let _ = out.execute(SetAttribute(Attribute::Dim));
        }
        let _ = out.execute(Print(line));
        let _ = out.execute(Print("\n"));
        let _ = out.execute(ResetColor);
        let _ = out.execute(SetAttribute(Attribute::Reset));
    }

    fn print_plain_line(&mut self, line: &str) {
        let mut out = stdout();
        let _ = out.queue(Print(line));
        let _ = out.queue(Print("\n"));
        let _ = out.flush();
    }

    fn print_markup_line(&mut self, line: &str) {
        // Minimal fallback: we currently print plain text and keep escaped content safe.
        self.print_plain_line(line);
    }

    fn parse_style(style: &str) -> (Color, bool, bool) {
        let normalized = style.to_ascii_lowercase();
        let bold = normalized.contains("bold");
        let dim = normalized.contains("dim");

        let fg = if normalized.contains("bright_red") || normalized.contains("red") {
            Color::Red
        } else if normalized.contains("bright_green") || normalized.contains("green") {
            Color::Green
        } else if normalized.contains("bright_yellow") || normalized.contains("yellow") {
            Color::Yellow
        } else if normalized.contains("bright_blue") || normalized.contains("blue") {
            Color::Blue
        } else if normalized.contains("bright_magenta") || normalized.contains("magenta") {
            Color::Magenta
        } else if normalized.contains("bright_cyan") || normalized.contains("cyan") {
            Color::Cyan
        } else if normalized.contains("black") {
            Color::Black
        } else {
            Color::White
        };

        (fg, bold, dim)
    }

    #[allow(dead_code)]
    pub fn active_spinner_count(&self) -> usize {
        self.spinners.values().filter(|s| s.active).count()
    }

    #[allow(dead_code)]
    pub fn get_file_icon_public(path: &str) -> &'static str {
        Self::get_file_icon(path)
    }
}
