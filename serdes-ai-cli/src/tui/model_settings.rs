//! Model Settings TUI - Configure per-model settings

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    backend::Backend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};

use crate::config::{ModelSettings, ReasoningEffort, Verbosity};
use crate::tui::theme::Theme;

/// Setting field type
#[derive(Debug, Clone)]
pub enum SettingField {
    Temperature { value: Option<f32> },
    Seed { value: Option<i32> },
    TopP { value: Option<f32> },
    MaxTokens { value: Option<usize> },
    ReasoningEffort { value: ReasoningEffort },
    Verbosity { value: Verbosity },
}

/// Model settings editor state
pub struct ModelSettingsEditor {
    model_name: String,
    settings: ModelSettings,
    fields: Vec<(String, SettingField)>,
    selected: usize,
    editing: Option<usize>,
    edit_buffer: String,
    theme: Theme,
    saved: bool,
    cancelled: bool,
}

impl ModelSettingsEditor {
    pub fn new(model_name: String, settings: ModelSettings) -> Self {
        let fields = vec![
            (
                "Temperature".to_string(),
                SettingField::Temperature {
                    value: settings.temperature,
                },
            ),
            (
                "Seed".to_string(),
                SettingField::Seed {
                    value: settings.seed,
                },
            ),
            (
                "Top P".to_string(),
                SettingField::TopP {
                    value: settings.top_p,
                },
            ),
            (
                "Max Tokens".to_string(),
                SettingField::MaxTokens {
                    value: settings.max_tokens,
                },
            ),
            (
                "Reasoning Effort".to_string(),
                SettingField::ReasoningEffort {
                    value: ReasoningEffort::Medium,
                },
            ),
            (
                "Verbosity".to_string(),
                SettingField::Verbosity {
                    value: Verbosity::Medium,
                },
            ),
        ];

        Self {
            model_name,
            settings,
            fields,
            selected: 0,
            editing: None,
            edit_buffer: String::new(),
            theme: Theme::default(),
            saved: false,
            cancelled: false,
        }
    }

    /// Run the settings editor
    pub fn run<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
    ) -> io::Result<Option<ModelSettings>> {
        loop {
            terminal.draw(|f| self.draw(f))?;

            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match self.editing {
                    None => self.handle_navigation(key.code),
                    Some(_) => self.handle_editing(key.code),
                }

                if self.cancelled {
                    return Ok(None);
                }

                if self.saved {
                    return Ok(Some(self.to_model_settings()));
                }
            }
        }
    }

    fn handle_navigation(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.cancelled = true,
            KeyCode::Char('s') => self.saved = true,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.fields.len() {
                    self.selected += 1;
                }
            }
            KeyCode::Enter => self.start_editing(),
            KeyCode::Char('d') => self.clear_field(),
            KeyCode::Char('r') => self.reset_to_defaults(),
            _ => {}
        }
    }

    fn handle_editing(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.cancel_editing(),
            KeyCode::Enter => self.confirm_editing(),
            KeyCode::Backspace => {
                self.edit_buffer.pop();
            }
            KeyCode::Char(c) => {
                self.edit_buffer.push(c);
            }
            _ => {}
        }
    }

    fn start_editing(&mut self) {
        self.editing = Some(self.selected);
        self.edit_buffer = self.get_field_value_string();
    }

    fn cancel_editing(&mut self) {
        self.editing = None;
        self.edit_buffer.clear();
    }

    fn confirm_editing(&mut self) {
        if let Some(idx) = self.editing {
            let input = self.edit_buffer.trim().to_string();
            self.parse_and_set_value(idx, &input);
        }
        self.editing = None;
        self.edit_buffer.clear();
    }

    fn clear_field(&mut self) {
        if let Some((_, field)) = self.fields.get_mut(self.selected) {
            *field = match field {
                SettingField::Temperature { .. } => SettingField::Temperature { value: None },
                SettingField::Seed { .. } => SettingField::Seed { value: None },
                SettingField::TopP { .. } => SettingField::TopP { value: None },
                SettingField::MaxTokens { .. } => SettingField::MaxTokens { value: None },
                _ => field.clone(),
            };
        }
    }

    fn reset_to_defaults(&mut self) {
        self.fields = vec![
            (
                "Temperature".to_string(),
                SettingField::Temperature { value: None },
            ),
            ("Seed".to_string(), SettingField::Seed { value: None }),
            ("Top P".to_string(), SettingField::TopP { value: None }),
            (
                "Max Tokens".to_string(),
                SettingField::MaxTokens { value: None },
            ),
            (
                "Reasoning Effort".to_string(),
                SettingField::ReasoningEffort {
                    value: ReasoningEffort::Medium,
                },
            ),
            (
                "Verbosity".to_string(),
                SettingField::Verbosity {
                    value: Verbosity::Medium,
                },
            ),
        ];
    }

    fn get_field_value_string(&self) -> String {
        if let Some((_, field)) = self.fields.get(self.selected) {
            match field {
                SettingField::Temperature { value } => {
                    value.map(|v| v.to_string()).unwrap_or_default()
                }
                SettingField::Seed { value } => value.map(|v| v.to_string()).unwrap_or_default(),
                SettingField::TopP { value } => value.map(|v| v.to_string()).unwrap_or_default(),
                SettingField::MaxTokens { value } => {
                    value.map(|v| v.to_string()).unwrap_or_default()
                }
                SettingField::ReasoningEffort { value } => format_reasoning(value).to_string(),
                SettingField::Verbosity { value } => format_verbosity(value).to_string(),
            }
        } else {
            String::new()
        }
    }

    fn parse_and_set_value(&mut self, idx: usize, input: &str) {
        if let Some((_, field)) = self.fields.get_mut(idx) {
            match field {
                SettingField::Temperature { value } => {
                    if input.is_empty() {
                        *value = None;
                    } else {
                        *value = input.parse::<f32>().ok();
                    }
                }
                SettingField::Seed { value } => {
                    if input.is_empty() {
                        *value = None;
                    } else {
                        *value = input.parse::<i32>().ok();
                    }
                }
                SettingField::TopP { value } => {
                    if input.is_empty() {
                        *value = None;
                    } else {
                        *value = input.parse::<f32>().ok();
                    }
                }
                SettingField::MaxTokens { value } => {
                    if input.is_empty() {
                        *value = None;
                    } else {
                        *value = input.parse::<usize>().ok();
                    }
                }
                SettingField::ReasoningEffort { value } => {
                    *value = parse_reasoning(input).unwrap_or(ReasoningEffort::Medium);
                }
                SettingField::Verbosity { value } => {
                    *value = parse_verbosity(input).unwrap_or(Verbosity::Medium);
                }
            }
        }
    }

    fn to_model_settings(&self) -> ModelSettings {
        let mut settings = self.settings.clone();

        for (_, field) in &self.fields {
            match field {
                SettingField::Temperature { value } => settings.temperature = *value,
                SettingField::Seed { value } => settings.seed = *value,
                SettingField::TopP { value } => settings.top_p = *value,
                SettingField::MaxTokens { value } => settings.max_tokens = *value,
                SettingField::ReasoningEffort { .. } | SettingField::Verbosity { .. } => {}
            }
        }

        settings
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(Clear, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints([
                Constraint::Length(3), // Title
                Constraint::Min(10),   // Settings list
                Constraint::Length(3), // Status/instructions
            ])
            .split(area);

        // Title
        let title = Paragraph::new(format!("⚙️ Model Settings: {}", self.model_name))
            .style(self.theme.title_style())
            .alignment(Alignment::Center);
        frame.render_widget(title, chunks[0]);

        // Settings list
        let items: Vec<ListItem> = self
            .fields
            .iter()
            .enumerate()
            .map(|(idx, (name, field))| self.render_setting_item(name, field, idx == self.selected))
            .collect();

        let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Settings"));

        let mut state = ListState::default();
        state.select(Some(self.selected));
        frame.render_stateful_widget(list, chunks[1], &mut state);

        // Instructions
        let help = if let Some(idx) = self.editing {
            format!(
                "Editing {}: {} | Enter: Confirm  Esc: Cancel",
                self.fields[idx].0, self.edit_buffer
            )
        } else {
            "↑/k: Up  ↓/j: Down  Enter: Edit  s: Save  d: Clear  r: Reset  q/Esc: Cancel"
                .to_string()
        };
        let help_para = Paragraph::new(help)
            .style(self.theme.muted_style())
            .alignment(Alignment::Center);
        frame.render_widget(help_para, chunks[2]);
    }

    fn render_setting_item(
        &self,
        name: &str,
        field: &SettingField,
        selected: bool,
    ) -> ListItem<'_> {
        let value_str = match field {
            SettingField::Temperature { value } => format_value(value, "(default)"),
            SettingField::Seed { value } => format_value(value, "(default)"),
            SettingField::TopP { value } => format_value(value, "(default)"),
            SettingField::MaxTokens { value } => format_value(value, "(default)"),
            SettingField::ReasoningEffort { value } => format_reasoning(value).to_string(),
            SettingField::Verbosity { value } => format_verbosity(value).to_string(),
        };

        let mut spans = vec![];

        // Selection indicator
        if selected {
            spans.push(Span::styled(
                "→ ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::raw("  "));
        }

        // Setting name
        let name_style = if selected {
            self.theme.title_style()
        } else {
            self.theme.normal_style()
        };
        spans.push(Span::styled(format!("{:<15}", name), name_style));
        spans.push(Span::raw(": "));

        // Value
        let value_style = if selected {
            self.theme.title_style()
        } else {
            self.theme.normal_style()
        };
        spans.push(Span::styled(value_str, value_style));

        ListItem::new(Line::from(spans))
    }
}

fn format_value<T: std::fmt::Display>(value: &Option<T>, default: &str) -> String {
    value
        .as_ref()
        .map(|v| v.to_string())
        .unwrap_or_else(|| default.to_string())
}

fn parse_reasoning(input: &str) -> Option<ReasoningEffort> {
    match input.trim().to_ascii_lowercase().as_str() {
        "minimal" => Some(ReasoningEffort::Minimal),
        "low" => Some(ReasoningEffort::Low),
        "medium" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        "xhigh" => Some(ReasoningEffort::Xhigh),
        _ => None,
    }
}

fn parse_verbosity(input: &str) -> Option<Verbosity> {
    match input.trim().to_ascii_lowercase().as_str() {
        "low" => Some(Verbosity::Low),
        "medium" => Some(Verbosity::Medium),
        "high" => Some(Verbosity::High),
        _ => None,
    }
}

fn format_reasoning(value: &ReasoningEffort) -> &'static str {
    match value {
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
    }
}

fn format_verbosity(value: &Verbosity) -> &'static str {
    match value {
        Verbosity::Low => "low",
        Verbosity::Medium => "medium",
        Verbosity::High => "high",
    }
}

/// Helper function to run model settings editor
pub fn interactive_model_settings(
    model_name: &str,
    current_settings: ModelSettings,
) -> io::Result<Option<ModelSettings>> {
    crate::tui::run_tui(|terminal| {
        let mut editor = ModelSettingsEditor::new(model_name.to_string(), current_settings);
        editor.run(terminal)
    })
}
