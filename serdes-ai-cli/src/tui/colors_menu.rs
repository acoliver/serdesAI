//! Colors/Diff Menu TUI - Configure banner colors and diff display mode

use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    Frame, Terminal,
    backend::Backend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};

use crate::config::{self, ColorsConfig};
use crate::tui::theme::Theme;

#[derive(Debug, Clone)]
pub struct ColorSetting {
    pub name: String,
    pub key: String,
    pub current: String,
    pub options: Vec<String>,
}

pub struct ColorsMenu {
    settings: Vec<ColorSetting>,
    selected: usize,
    theme: Theme,
}

impl ColorsMenu {
    pub fn new() -> Self {
        let colors = config::get_colors_config();
        let options = color_options();

        let settings = vec![
            ColorSetting {
                name: "Thinking Banner".to_string(),
                key: "banner_thinking".to_string(),
                current: colors.banner_thinking,
                options: options.clone(),
            },
            ColorSetting {
                name: "Shell Command Banner".to_string(),
                key: "banner_shell_command".to_string(),
                current: colors.banner_shell_command,
                options: options.clone(),
            },
            ColorSetting {
                name: "Edit File Banner".to_string(),
                key: "banner_edit_file".to_string(),
                current: colors.banner_edit_file,
                options: options.clone(),
            },
            ColorSetting {
                name: "Directory Listing Banner".to_string(),
                key: "banner_directory_listing".to_string(),
                current: colors.banner_directory_listing,
                options: options.clone(),
            },
            ColorSetting {
                name: "Grep Banner".to_string(),
                key: "banner_grep".to_string(),
                current: colors.banner_grep,
                options,
            },
        ];

        Self {
            settings,
            selected: 0,
            theme: Theme::default(),
        }
    }

    pub fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> io::Result<bool> {
        loop {
            terminal.draw(|f| self.draw(f))?;

            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(false),
                    KeyCode::Char('s') => {
                        self.save();
                        return Ok(true);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.selected = self.selected.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if self.selected + 1 < self.settings.len() {
                            self.selected += 1;
                        }
                    }
                    KeyCode::Left | KeyCode::Char('h') => self.prev_color(),
                    KeyCode::Right | KeyCode::Char('l') => self.next_color(),
                    KeyCode::Char('r') => self.reset_to_default(),
                    _ => {}
                }
            }
        }
    }

    fn prev_color(&mut self) {
        if let Some(setting) = self.settings.get_mut(self.selected) {
            let idx = setting
                .options
                .iter()
                .position(|o| o == &setting.current)
                .unwrap_or(0);
            let new_idx = if idx == 0 {
                setting.options.len() - 1
            } else {
                idx - 1
            };
            setting.current = setting.options[new_idx].clone();
        }
    }

    fn next_color(&mut self) {
        if let Some(setting) = self.settings.get_mut(self.selected) {
            let idx = setting
                .options
                .iter()
                .position(|o| o == &setting.current)
                .unwrap_or(0);
            let new_idx = (idx + 1) % setting.options.len();
            setting.current = setting.options[new_idx].clone();
        }
    }

    fn reset_to_default(&mut self) {
        let defaults = ColorsConfig::default();

        if let Some(setting) = self.settings.get_mut(0) {
            setting.current = defaults.banner_thinking.clone();
        }
        if let Some(setting) = self.settings.get_mut(1) {
            setting.current = defaults.banner_shell_command.clone();
        }
        if let Some(setting) = self.settings.get_mut(2) {
            setting.current = defaults.banner_edit_file.clone();
        }
        if let Some(setting) = self.settings.get_mut(3) {
            setting.current = defaults.banner_directory_listing.clone();
        }
        if let Some(setting) = self.settings.get_mut(4) {
            setting.current = defaults.banner_grep;
        }
    }

    fn save(&self) {
        for setting in &self.settings {
            config::set_banner_color(&setting.key, &setting.current);
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
                Constraint::Min(10),
                Constraint::Length(5),
            ])
            .split(area);

        let title = Paragraph::new("Color Configuration")
            .style(self.theme.title_style())
            .alignment(Alignment::Center);
        frame.render_widget(title, chunks[0]);

        let items: Vec<ListItem> = self
            .settings
            .iter()
            .enumerate()
            .map(|(idx, setting)| self.render_setting(setting, idx == self.selected))
            .collect();

        let list = List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Banner Colors"),
        );

        let mut state = ListState::default();
        state.select(Some(self.selected));
        frame.render_stateful_widget(list, chunks[1], &mut state);

        frame.render_widget(self.render_preview(), chunks[2]);
    }

    fn render_setting(&self, setting: &ColorSetting, selected: bool) -> ListItem<'_> {
        let color = parse_color(&setting.current);

        let mut spans = Vec::new();
        if selected {
            spans.push(Span::styled(
                "→ ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::raw(" "));
        }

        spans.push(Span::styled(
            format!("{:<25}", setting.name),
            if selected {
                self.theme.title_style()
            } else {
                self.theme.normal_style()
            },
        ));

        spans.push(Span::raw(": "));
        spans.push(Span::styled(
            setting.current.clone(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));

        ListItem::new(Line::from(spans))
    }

    fn render_preview(&self) -> Paragraph<'static> {
        let mut text = Text::default();

        if let Some(setting) = self.settings.get(self.selected) {
            text.push_line(Line::from(vec![
                Span::styled("Preview: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!(" {} banner ", setting.name),
                    Style::default()
                        .fg(Color::White)
                        .bg(parse_color(&setting.current))
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }

        text.push_line(Line::from(""));
        text.push_line(Line::from(
            "↑/k ↓/j: Move  ←/h →/l: Color  r: Reset  s: Save  q/Esc: Cancel",
        ));

        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title("Controls"))
    }
}

impl Default for ColorsMenu {
    fn default() -> Self {
        Self::new()
    }
}

pub struct DiffMenu {
    compact: bool,
    theme: Theme,
}

impl DiffMenu {
    pub fn new() -> Self {
        Self {
            compact: config::get_compact_diffs(),
            theme: Theme::default(),
        }
    }

    pub fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> io::Result<bool> {
        loop {
            terminal.draw(|f| self.draw(f))?;

            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(false),
                    KeyCode::Char('s') | KeyCode::Enter => {
                        config::set_compact_diffs(self.compact);
                        return Ok(true);
                    }
                    KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Char('h')
                    | KeyCode::Char('l')
                    | KeyCode::Char(' ') => {
                        self.compact = !self.compact;
                    }
                    _ => {}
                }
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
                Constraint::Length(8),
                Constraint::Length(4),
            ])
            .split(area);

        let title = Paragraph::new("Diff Display Mode")
            .style(self.theme.title_style())
            .alignment(Alignment::Center);
        frame.render_widget(title, chunks[0]);

        let mode = if self.compact { "compact" } else { "full" };
        let mode_desc = if self.compact {
            "Compact: concise hunks and reduced context"
        } else {
            "Full: complete diff output with full context"
        };

        let body = Paragraph::new(Text::from(vec![
            Line::from(vec![
                Span::styled("Current Mode: ", self.theme.normal_style()),
                Span::styled(
                    mode,
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from(mode_desc),
            Line::from(""),
            Line::from("Use ←/→, h/l, or Space to toggle mode."),
        ]))
        .block(Block::default().borders(Borders::ALL).title("Mode"));
        frame.render_widget(body, chunks[1]);

        let controls = Paragraph::new("s/Enter: Save  q/Esc: Cancel")
            .style(self.theme.muted_style())
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title("Controls"));
        frame.render_widget(controls, chunks[2]);
    }
}

impl Default for DiffMenu {
    fn default() -> Self {
        Self::new()
    }
}

fn color_options() -> Vec<String> {
    vec![
        "black".to_string(),
        "red".to_string(),
        "green".to_string(),
        "yellow".to_string(),
        "blue".to_string(),
        "magenta".to_string(),
        "cyan".to_string(),
        "white".to_string(),
        "bright_red".to_string(),
        "bright_green".to_string(),
        "bright_yellow".to_string(),
        "bright_blue".to_string(),
        "bright_magenta".to_string(),
        "bright_cyan".to_string(),
    ]
}

fn parse_color(name: &str) -> Color {
    match name.trim().to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "white" => Color::White,
        "bright_red" => Color::LightRed,
        "bright_green" => Color::LightGreen,
        "bright_yellow" => Color::LightYellow,
        "bright_blue" => Color::LightBlue,
        "bright_magenta" => Color::LightMagenta,
        "bright_cyan" => Color::LightCyan,
        _ => Color::White,
    }
}

pub fn interactive_colors_menu() -> io::Result<bool> {
    crate::tui::run_tui(|terminal| {
        let mut menu = ColorsMenu::new();
        menu.run(terminal)
    })
}

pub fn interactive_diff_menu() -> io::Result<bool> {
    crate::tui::run_tui(|terminal| {
        let mut menu = DiffMenu::new();
        menu.run(terminal)
    })
}
