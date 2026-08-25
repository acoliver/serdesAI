//! Interactive Agent Picker TUI

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

/// Agent information for display.
#[derive(Debug, Clone)]
pub struct AgentInfo {
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub is_current: bool,
}

/// Agent picker state.
pub struct AgentPicker {
    all_agents: Vec<AgentInfo>,
    visible_indices: Vec<usize>,
    selected: usize,
    theme: Theme,
    filter: String,
}

impl AgentPicker {
    pub fn new(agents: Vec<AgentInfo>) -> Self {
        let current_idx = agents.iter().position(|a| a.is_current).unwrap_or(0);
        let visible_indices = (0..agents.len()).collect::<Vec<_>>();

        Self {
            all_agents: agents,
            visible_indices,
            selected: current_idx,
            theme: Theme::default(),
            filter: String::new(),
        }
    }

    /// Run the picker and return selected agent name.
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
                        if let Some(agent) = self.selected_agent() {
                            return Ok(Some(agent.name.clone()));
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
                    _ => {}
                }
            }
        }
    }

    fn selected_agent(&self) -> Option<&AgentInfo> {
        self.visible_indices
            .get(self.selected)
            .and_then(|idx| self.all_agents.get(*idx))
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.visible_indices.len();
        if len == 0 {
            self.selected = 0;
            return;
        }

        let next = (self.selected as i32 + delta).clamp(0, len as i32 - 1);
        self.selected = next as usize;
    }

    fn apply_filter(&mut self) {
        if self.filter.is_empty() {
            self.visible_indices = (0..self.all_agents.len()).collect();
            self.selected = self
                .all_agents
                .iter()
                .position(|a| a.is_current)
                .unwrap_or(0)
                .min(self.visible_indices.len().saturating_sub(1));
            return;
        }

        let needle = self.filter.to_ascii_lowercase();
        self.visible_indices = self
            .all_agents
            .iter()
            .enumerate()
            .filter_map(|(idx, agent)| {
                let hay_name = agent.name.to_ascii_lowercase();
                let hay_display = agent.display_name.to_ascii_lowercase();
                let hay_desc = agent.description.to_ascii_lowercase();
                (hay_name.contains(&needle)
                    || hay_display.contains(&needle)
                    || hay_desc.contains(&needle))
                .then_some(idx)
            })
            .collect();

        self.selected = 0;
    }

    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(Clear, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(2)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(3),
            ])
            .split(area);

        let title = Paragraph::new("Select an Agent")
            .style(self.theme.title_style())
            .alignment(Alignment::Center);
        frame.render_widget(title, chunks[0]);

        let items: Vec<ListItem> = if self.visible_indices.is_empty() {
            vec![ListItem::new(Line::from(vec![Span::styled(
                "No agents match current filter",
                self.theme.muted_style(),
            )]))]
        } else {
            self.visible_indices
                .iter()
                .enumerate()
                .map(|(row_idx, agent_idx)| {
                    let agent = &self.all_agents[*agent_idx];
                    self.render_agent_item(agent, row_idx == self.selected)
                })
                .collect()
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Available Agents"),
            )
            .highlight_style(self.theme.selected_style())
            .highlight_symbol("→ ");

        frame.render_widget(list, chunks[1]);

        let help_text = if self.filter.is_empty() {
            "↑/k: Up  ↓/j: Down  Enter: Select  q/Esc: Cancel  Type to filter".to_string()
        } else {
            format!(
                "Filter: {} | Backspace: Edit | Esc: Clear filter | Enter: Select",
                self.filter
            )
        };

        let help = Paragraph::new(help_text)
            .style(self.theme.muted_style())
            .alignment(Alignment::Center);
        frame.render_widget(help, chunks[2]);
    }

    fn render_agent_item(&self, agent: &AgentInfo, selected: bool) -> ListItem<'_> {
        let mut spans = vec![];

        if agent.is_current {
            spans.push(Span::styled("✓ ", Style::default().fg(Color::Green)));
        } else {
            spans.push(Span::raw(" "));
        }

        let name_style = if selected {
            self.theme.selected_style()
        } else {
            self.theme.normal_style()
        };
        spans.push(Span::styled(agent.display_name.clone(), name_style));

        spans.push(Span::styled(
            format!(" - {}", agent.description),
            self.theme.muted_style(),
        ));

        ListItem::new(Line::from(spans))
    }
}

/// Helper function to run agent picker.
pub fn interactive_agent_picker(agents: Vec<AgentInfo>) -> io::Result<Option<String>> {
    crate::tui::run_tui(|terminal| {
        let mut picker = AgentPicker::new(agents);
        picker.run(terminal)
    })
}
