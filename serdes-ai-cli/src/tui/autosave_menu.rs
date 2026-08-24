//! Autosave Menu TUI - Browse and load autosaved sessions

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    backend::Backend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::config;
use crate::session::{self, SessionInfo};
use crate::tui::theme::Theme;

/// Autosave menu state
pub struct AutosaveMenu {
    sessions: Vec<SessionInfo>,
    selected: usize,
    theme: Theme,
}

impl AutosaveMenu {
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: Self::load_sessions(),
            selected: 0,
            theme: Theme::default(),
        }
    }

    fn load_sessions() -> Vec<SessionInfo> {
        let autosave_dir = config::get_autosave_dir();
        session::list_sessions(&autosave_dir).unwrap_or_default()
    }

    pub fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> io::Result<Option<String>> {
        loop {
            terminal.draw(|f| self.draw(f))?;

            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(None),
                    KeyCode::Enter => {
                        if let Some(info) = self.sessions.get(self.selected) {
                            return Ok(Some(info.id.clone()));
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
                    KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
                    KeyCode::Char('d') => self.delete_selected(),
                    KeyCode::Char('r') => {
                        self.sessions = Self::load_sessions();
                        self.selected = self.selected.min(self.sessions.len().saturating_sub(1));
                    }
                    _ => {}
                }
            }
        }
    }

    fn move_selection(&mut self, delta: i32) {
        let len = self.sessions.len();
        if len == 0 {
            self.selected = 0;
            return;
        }

        let next = (self.selected as i32 + delta).clamp(0, len as i32 - 1);
        self.selected = next as usize;
    }

    fn delete_selected(&mut self) {
        let Some(info) = self.sessions.get(self.selected) else {
            return;
        };

        let autosave_dir = config::get_autosave_dir();
        if session::delete_session(&info.id, &autosave_dir).is_ok() {
            self.sessions.remove(self.selected);
            if self.sessions.is_empty() {
                self.selected = 0;
            } else {
                self.selected = self.selected.min(self.sessions.len() - 1);
            }
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
                Constraint::Min(8),
                Constraint::Length(7),
                Constraint::Length(3),
            ])
            .split(area);

        let title = Paragraph::new("💾 Autosaved Sessions")
            .style(self.theme.title_style())
            .alignment(Alignment::Center);
        frame.render_widget(title, chunks[0]);

        let info = if self.sessions.is_empty() {
            "No autosaved sessions found".to_string()
        } else {
            format!("{} sessions available", self.sessions.len())
        };
        let info_para = Paragraph::new(info)
            .style(self.theme.muted_style())
            .alignment(Alignment::Center);
        frame.render_widget(info_para, chunks[1]);

        if self.sessions.is_empty() {
            let empty = Paragraph::new("No sessions to display")
                .style(self.theme.muted_style())
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL).title("Sessions"));
            frame.render_widget(empty, chunks[2]);
        } else {
            let items: Vec<ListItem> = self
                .sessions
                .iter()
                .enumerate()
                .map(|(idx, s)| self.render_session_item(s, idx == self.selected))
                .collect();

            let list = List::new(items)
                .block(Block::default().borders(Borders::ALL).title("Sessions"))
                .highlight_style(self.theme.selected_style());

            let mut state = ListState::default();
            state.select(Some(self.selected));
            frame.render_stateful_widget(list, chunks[2], &mut state);
        }

        if let Some(info) = self.sessions.get(self.selected) {
            let details = self.render_session_details(info);
            frame.render_widget(details, chunks[3]);
        } else {
            let empty_details = Paragraph::new("No session selected")
                .style(self.theme.muted_style())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Session Details"),
                );
            frame.render_widget(empty_details, chunks[3]);
        }

        let help = if self.sessions.is_empty() {
            "r: Refresh  q/Esc: Cancel"
        } else {
            "↑/k: Up  ↓/j: Down  Enter: Load  d: Delete  r: Refresh  q/Esc: Cancel"
        };
        let help_para = Paragraph::new(help)
            .style(self.theme.muted_style())
            .alignment(Alignment::Center);
        frame.render_widget(help_para, chunks[4]);
    }

    fn render_session_item(&self, session: &SessionInfo, selected: bool) -> ListItem<'_> {
        let date_str = session.updated_at.format("%Y-%m-%d %H:%M").to_string();
        let prefix = if selected { "→ " } else { "  " };
        let style = if selected {
            self.theme.selected_style()
        } else {
            self.theme.normal_style()
        };

        let row = Line::from(vec![
            Span::styled(prefix, Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(format!("{:<24}", session.id), style),
            Span::styled(date_str, self.theme.muted_style()),
        ]);

        ListItem::new(row)
    }

    fn render_session_details(&self, session: &SessionInfo) -> Paragraph<'static> {
        let mut text = Text::default();

        text.push_line(Line::from(vec![
            Span::styled("ID: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(session.id.clone()),
        ]));

        text.push_line(Line::from(vec![
            Span::styled("Created: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(
                session
                    .created_at
                    .format("%Y-%m-%d %H:%M:%S UTC")
                    .to_string(),
            ),
        ]));

        text.push_line(Line::from(vec![
            Span::styled("Updated: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(
                session
                    .updated_at
                    .format("%Y-%m-%d %H:%M:%S UTC")
                    .to_string(),
            ),
        ]));

        text.push_line(Line::from(vec![
            Span::styled("Messages: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "{} (~{} tokens)",
                session.message_count, session.total_tokens
            )),
        ]));

        Paragraph::new(text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Session Details"),
            )
            .wrap(Wrap { trim: true })
    }
}

impl Default for AutosaveMenu {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper function to run autosave menu
pub fn interactive_autosave_menu() -> io::Result<Option<String>> {
    crate::tui::run_tui(|terminal| {
        let mut menu = AutosaveMenu::new();
        menu.run(terminal)
    })
}
