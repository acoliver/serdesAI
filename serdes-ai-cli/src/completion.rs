//! Live completion dropdown (like prompt_toolkit)

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, Clear, ClearType},
    ExecutableCommand, QueueableCommand,
};
use std::io::{self, Write};

/// Command completion entry
#[derive(Clone, Debug)]
pub struct Completion {
    pub text: String,
    pub display: String,
    pub description: String,
}

/// Available commands for completion
pub fn get_command_completions() -> Vec<Completion> {
    vec![
        Completion {
            text: "/help".to_string(),
            display: "/help".to_string(),
            description: "Show help".to_string(),
        },
        Completion {
            text: "/model".to_string(),
            display: "/model".to_string(),
            description: "Change model".to_string(),
        },
        Completion {
            text: "/agent".to_string(),
            display: "/agent".to_string(),
            description: "Change agent".to_string(),
        },
        Completion {
            text: "/show".to_string(),
            display: "/show".to_string(),
            description: "Show config".to_string(),
        },
        Completion {
            text: "/set".to_string(),
            display: "/set".to_string(),
            description: "Set config".to_string(),
        },
        Completion {
            text: "/compact".to_string(),
            display: "/compact".to_string(),
            description: "Compact session".to_string(),
        },
        Completion {
            text: "/truncate".to_string(),
            display: "/truncate".to_string(),
            description: "Truncate session".to_string(),
        },
        Completion {
            text: "/session".to_string(),
            display: "/session".to_string(),
            description: "Session info".to_string(),
        },
        Completion {
            text: "/wiggum".to_string(),
            display: "/wiggum".to_string(),
            description: "Start wiggum loop".to_string(),
        },
        Completion {
            text: "/paste".to_string(),
            display: "/paste".to_string(),
            description: "Paste from clipboard".to_string(),
        },
        Completion {
            text: "/colors".to_string(),
            display: "/colors".to_string(),
            description: "Configure colors".to_string(),
        },
        Completion {
            text: "/diff".to_string(),
            display: "/diff".to_string(),
            description: "Toggle diff mode".to_string(),
        },
        Completion {
            text: "/quit".to_string(),
            display: "/quit".to_string(),
            description: "Exit".to_string(),
        },
    ]
}

/// Input state with completion
pub struct CompletingInput {
    buffer: String,
    cursor_pos: usize,
    completions: Vec<Completion>,
    selected: usize,
    show_completions: bool,
}

impl CompletingInput {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor_pos: 0,
            completions: Vec::new(),
            selected: 0,
            show_completions: false,
        }
    }
}

/// What a key press did to the input line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputOutcome {
    /// The line was submitted.
    Submitted(String),
    /// The user cancelled the current line (Ctrl-C).
    Cancelled,
    /// The user asked to end the session (Ctrl-D on an empty line).
    EndOfInput,
}

impl CompletingInput {
    /// Handle a key with its modifiers.
    ///
    /// Raw mode is enabled while reading, so Ctrl-C and Ctrl-D arrive here as
    /// ordinary key events rather than as signals. Without handling them
    /// explicitly they fall through to the `Char(c)` arm and get typed into the
    /// buffer — Ctrl-C would insert a literal 'c' — despite the startup banner
    /// telling the user both keys work.
    pub fn handle_key_event(&mut self, key: KeyEvent) -> Option<InputOutcome> {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                // Ctrl-C abandons the line; on an already-empty line it means
                // "I want out", matching what every other terminal tool does.
                KeyCode::Char('c') => {
                    if self.buffer.is_empty() {
                        return Some(InputOutcome::Cancelled);
                    }
                    self.buffer.clear();
                    self.cursor_pos = 0;
                    self.show_completions = false;
                    self.completions.clear();
                    return None;
                }
                // Ctrl-D on an empty line is the conventional clean exit.
                KeyCode::Char('d') => {
                    if self.buffer.is_empty() {
                        return Some(InputOutcome::EndOfInput);
                    }
                    return None;
                }
                _ => return None,
            }
        }

        self.handle_key(key.code).map(InputOutcome::Submitted)
    }
}

impl CompletingInput {
    /// Handle key event, returns Some(result) if input complete
    pub fn handle_key(&mut self, key: KeyCode) -> Option<String> {
        match key {
            KeyCode::Enter => {
                return Some(self.buffer.clone());
            }
            KeyCode::Char('/') if self.buffer.is_empty() => {
                self.buffer.push('/');
                self.cursor_pos = 1;
                self.update_completions();
            }
            KeyCode::Char(c) => {
                self.buffer.insert(self.cursor_pos, c);
                self.cursor_pos += 1;
                self.update_completions();
            }
            KeyCode::Backspace => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                    self.buffer.remove(self.cursor_pos);
                    self.update_completions();
                }
            }
            KeyCode::Left => {
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                }
            }
            KeyCode::Right => {
                if self.cursor_pos < self.buffer.len() {
                    self.cursor_pos += 1;
                }
            }
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
            }
            KeyCode::Down => {
                if self.selected < self.completions.len().saturating_sub(1) {
                    self.selected += 1;
                }
            }
            KeyCode::Tab => {
                if let Some(comp) = self.completions.get(self.selected) {
                    self.buffer = comp.text.clone();
                    self.cursor_pos = self.buffer.len();
                    self.show_completions = false;
                }
            }
            KeyCode::Esc => {
                self.show_completions = false;
            }
            _ => {}
        }
        None
    }

    fn update_completions(&mut self) {
        if self.buffer.starts_with('/') {
            let all = get_command_completions();
            let prefix = &self.buffer[1..]; // Remove leading /
            self.completions = all
                .into_iter()
                .filter(|c| c.text[1..].starts_with(prefix))
                .collect();
            self.selected = 0;
            self.show_completions = !self.completions.is_empty();
        } else {
            self.show_completions = false;
        }
    }

    /// Render current state
    pub fn render(&self, stdout: &mut io::Stdout) -> io::Result<()> {
        // Clear line and redraw
        stdout.queue(cursor::MoveToColumn(0))?;
        stdout.queue(Clear(ClearType::UntilNewLine))?;

        // Print prompt + buffer
        stdout.queue(SetForegroundColor(Color::Cyan))?;
        stdout.queue(Print("serdes-ai > "))?;
        stdout.queue(ResetColor)?;
        stdout.queue(Print(&self.buffer))?;

        // Show completions dropdown
        if self.show_completions {
            stdout.queue(Print("\n"))?;
            for (i, comp) in self.completions.iter().take(8).enumerate() {
                if i == self.selected {
                    stdout.queue(SetForegroundColor(Color::Green))?;
                    stdout.queue(Print(format!(
                        "> {} - {}\n",
                        comp.display, comp.description
                    )))?;
                    stdout.queue(ResetColor)?;
                } else {
                    stdout.queue(Print(format!(
                        "  {} - {}\n",
                        comp.display, comp.description
                    )))?;
                }
            }
            // Move cursor back up
            let lines = self.completions.len().min(8) + 1;
            for _ in 0..lines {
                stdout.queue(cursor::MoveUp(1))?;
            }
            stdout.queue(cursor::MoveToColumn(13 + self.cursor_pos as u16))?;
        }

        stdout.flush()
    }
}

impl Default for CompletingInput {
    fn default() -> Self {
        Self::new()
    }
}

/// Read input with live completion
pub fn read_input_with_completion() -> io::Result<Option<String>> {
    let mut stdout = io::stdout();
    let mut input = CompletingInput::new();

    terminal::enable_raw_mode()?;
    stdout.execute(cursor::Show)?;

    loop {
        input.render(&mut stdout)?;

        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press {
                match input.handle_key_event(key) {
                    Some(InputOutcome::Submitted(result)) => {
                        terminal::disable_raw_mode()?;
                        stdout.queue(Print("\n"))?;
                        stdout.flush()?;
                        return Ok(Some(result));
                    }
                    // Cancelling one line is not the end of the session: the
                    // caller prints a notice and prompts again.
                    Some(InputOutcome::Cancelled) => {
                        terminal::disable_raw_mode()?;
                        stdout.queue(Print("\n"))?;
                        stdout.flush()?;
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "input cancelled by the user",
                        ));
                    }
                    // End of input means the session is over.
                    Some(InputOutcome::EndOfInput) => {
                        terminal::disable_raw_mode()?;
                        stdout.queue(Print("\n"))?;
                        stdout.flush()?;
                        return Ok(None);
                    }
                    None => {}
                }
            }
        }
    }
}
