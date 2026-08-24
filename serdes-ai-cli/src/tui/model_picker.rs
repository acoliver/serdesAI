//! Interactive Model Picker TUI

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    backend::Backend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame, Terminal,
};

use crate::tui::theme::Theme;

/// Model information for display.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub name: String,
    pub provider: String,
    pub description: String,
    pub is_current: bool,
    pub is_pinned: bool,
}

/// Model picker state.
pub struct ModelPicker {
    models: Vec<ModelInfo>,
    filtered_indices: Vec<usize>,
    selected: usize,
    scroll_offset: usize,
    theme: Theme,
    filter: String,
}

impl ModelPicker {
    pub fn new(models: Vec<ModelInfo>) -> Self {
        let current_model_idx = models.iter().position(|m| m.is_current).unwrap_or(0);
        let filtered_indices: Vec<usize> = (0..models.len()).collect();
        let selected = if filtered_indices.is_empty() {
            0
        } else {
            current_model_idx.min(filtered_indices.len() - 1)
        };

        Self {
            models,
            filtered_indices,
            selected,
            scroll_offset: 0,
            theme: Theme::default(),
            filter: String::new(),
        }
    }

    /// Run the picker and return selected model name.
    pub fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> io::Result<Option<String>> {
        loop {
            terminal.draw(|f| self.draw(f))?;

            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') => return Ok(None),
                    KeyCode::Esc => {
                        if self.filter.is_empty() {
                            return Ok(None);
                        }
                        self.filter.clear();
                        self.apply_filter();
                    }
                    KeyCode::Enter => {
                        if let Some(&idx) = self.filtered_indices.get(self.selected) {
                            return Ok(Some(self.models[idx].name.clone()));
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                    KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                    KeyCode::Char(c) if c.is_alphanumeric() || c == '-' || c == '_' => {
                        self.filter.push(c.to_ascii_lowercase());
                        self.apply_filter();
                    }
                    KeyCode::Backspace => {
                        self.filter.pop();
                        self.apply_filter();
                    }
                    KeyCode::Char('p') => self.toggle_pin(),
                    _ => {}
                }
            }
        }
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.filtered_indices.len();
        if len == 0 {
            self.selected = 0;
            self.scroll_offset = 0;
            return;
        }

        let new_idx = (self.selected as i32 + delta).clamp(0, len as i32 - 1);
        self.selected = new_idx as usize;
        self.update_scroll();
    }

    fn update_scroll(&mut self) {
        const VISIBLE_ITEMS: usize = 12;

        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + VISIBLE_ITEMS {
            self.scroll_offset = self.selected.saturating_sub(VISIBLE_ITEMS - 1);
        }
    }

    fn apply_filter(&mut self) {
        if self.filter.is_empty() {
            self.filtered_indices = (0..self.models.len()).collect();
            self.selected = self
                .models
                .iter()
                .position(|m| m.is_current)
                .unwrap_or(0)
                .min(self.filtered_indices.len().saturating_sub(1));
            self.scroll_offset = 0;
            self.update_scroll();
            return;
        }

        let search = self.filter.to_ascii_lowercase();
        self.filtered_indices = self
            .models
            .iter()
            .enumerate()
            .filter_map(|(idx, model)| {
                let hay_name = model.name.to_ascii_lowercase();
                let hay_provider = model.provider.to_ascii_lowercase();
                let hay_description = model.description.to_ascii_lowercase();
                (hay_name.contains(&search)
                    || hay_provider.contains(&search)
                    || hay_description.contains(&search))
                .then_some(idx)
            })
            .collect();

        self.selected = 0;
        self.scroll_offset = 0;
    }

    fn toggle_pin(&mut self) {
        if let Some(&idx) = self.filtered_indices.get(self.selected) {
            let model = &mut self.models[idx];
            model.is_pinned = !model.is_pinned;
            // TODO: persist pinning action when model pin UX is finalized.
        }
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(Clear, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(12),
                Constraint::Length(3),
            ])
            .split(area);

        let current_model = self.models.iter().find(|m| m.is_current);
        let title_text = if let Some(model) = current_model {
            format!("🤖 Select a Model | Current: {}", model.name)
        } else {
            "🤖 Select a Model".to_string()
        };

        let title = Paragraph::new(title_text)
            .style(self.theme.title_style())
            .alignment(Alignment::Center);
        frame.render_widget(title, chunks[0]);

        let status_text = if self.filter.is_empty() {
            format!("{} models available", self.filtered_indices.len())
        } else {
            format!(
                "Filter: '{}' ({} of {} models)",
                self.filter,
                self.filtered_indices.len(),
                self.models.len()
            )
        };

        let status = Paragraph::new(status_text)
            .style(self.theme.muted_style())
            .alignment(Alignment::Center);
        frame.render_widget(status, chunks[1]);

        let visible_items = self
            .filtered_indices
            .iter()
            .skip(self.scroll_offset)
            .take(12)
            .enumerate()
            .map(|(row, &model_idx)| {
                let absolute_index = self.scroll_offset + row;
                let model = &self.models[model_idx];
                self.render_model_item(model, absolute_index == self.selected)
            })
            .collect::<Vec<_>>();

        let items = if visible_items.is_empty() {
            vec![ListItem::new(Line::from(vec![Span::styled(
                "No models match current filter",
                self.theme.muted_style(),
            )]))]
        } else {
            visible_items
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Available Models"),
            )
            .highlight_style(self.theme.selected_style())
            .highlight_symbol("→ ");

        frame.render_widget(list, chunks[2]);

        let help = Paragraph::new(
            "↑/k ↓/j: Move  Enter: Select  p: Toggle Pin  Type: Filter  Esc/q: Cancel",
        )
        .style(self.theme.muted_style())
        .alignment(Alignment::Center);
        frame.render_widget(help, chunks[3]);
    }

    fn render_model_item(&self, model: &ModelInfo, selected: bool) -> ListItem<'_> {
        let mut spans = Vec::new();

        let current_marker = if model.is_current { "✓" } else { " " };
        let pin_marker = if model.is_pinned { "📌" } else { " " };
        spans.push(Span::raw(format!("{} {} ", current_marker, pin_marker)));

        let provider_color = provider_color(&model.provider);
        spans.push(Span::styled(
            format!("[{}]", model.provider),
            Style::default().fg(provider_color),
        ));
        spans.push(Span::raw(" "));

        let name_style = if selected {
            self.theme.selected_style()
        } else {
            self.theme.normal_style()
        };
        spans.push(Span::styled(model.name.clone(), name_style));

        spans.push(Span::styled(
            format!(" - {}", truncate_description(&model.description, 48)),
            self.theme.muted_style(),
        ));

        ListItem::new(Line::from(spans))
    }
}

fn provider_color(provider: &str) -> Color {
    match provider {
        "OpenAI" => Color::Green,
        "Anthropic" => Color::Magenta,
        "Google" => Color::Yellow,
        "Mistral" => Color::Cyan,
        "Groq" => Color::Blue,
        "Ollama" => Color::LightBlue,
        "Bedrock" => Color::LightMagenta,
        "OpenRouter" => Color::LightYellow,
        "Cohere" => Color::LightCyan,
        "Hugging Face" => Color::LightGreen,
        _ => Color::Gray,
    }
}

fn truncate_description(description: &str, max_chars: usize) -> String {
    if description.chars().count() <= max_chars {
        return description.to_string();
    }

    let truncated = description.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

/// Helper function to run model picker.
pub fn interactive_model_picker(models: Vec<ModelInfo>) -> io::Result<Option<String>> {
    crate::tui::run_tui(|terminal| {
        let mut picker = ModelPicker::new(models);
        picker.run(terminal)
    })
}
