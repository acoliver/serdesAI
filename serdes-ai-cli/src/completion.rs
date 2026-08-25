//! Live completion dropdown (like prompt_toolkit)

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, Clear, ClearType},
    ExecutableCommand, QueueableCommand,
};
use std::io::{self, Write};

use unicode_width::UnicodeWidthStr;

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
    /// The prompt the caller asked for.
    ///
    /// Held rather than printed by the caller: every redraw rewrites the line,
    /// so a prompt printed once elsewhere is erased by the first redraw and
    /// replaced by whatever this draws instead.
    prompt: String,
    /// Whether the prompt has been drawn for this line.
    ///
    /// It is written once and then left alone: reprinting it on every keystroke
    /// makes the terminal flicker and fills the scrollback with one copy of the
    /// prompt per character typed.
    prompt_drawn: bool,
}

impl CompletingInput {
    pub fn new() -> Self {
        Self::with_prompt("serdes-ai > ")
    }

    /// An input line that draws `prompt` in front of what is typed.
    pub fn with_prompt(prompt: impl Into<String>) -> Self {
        Self {
            buffer: String::new(),
            cursor_pos: 0,
            completions: Vec::new(),
            selected: 0,
            show_completions: false,
            prompt: prompt.into(),
            prompt_drawn: false,
        }
    }

    /// How many columns the cursor sits from the left edge.
    ///
    /// `cursor_pos` is a byte offset, which is not a column: any character
    /// outside ASCII would put the cursor in the wrong place.
    fn cursor_column(&self) -> u16 {
        let width = UnicodeWidthStr::width(self.prompt.as_str())
            + UnicodeWidthStr::width(&self.buffer[..self.cursor_pos]);

        u16::try_from(width).unwrap_or(u16::MAX)
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
    /// The byte offset of the character before the cursor.
    fn previous_boundary(&self) -> Option<usize> {
        if self.cursor_pos == 0 {
            return None;
        }
        self.buffer[..self.cursor_pos]
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
    }

    /// The byte offset just past the character at the cursor.
    fn next_boundary(&self) -> Option<usize> {
        self.buffer[self.cursor_pos..]
            .chars()
            .next()
            .map(|c| self.cursor_pos + c.len_utf8())
    }

    /// Handle key event, returns Some(result) if input complete
    pub fn handle_key(&mut self, key: KeyCode) -> Option<String> {
        match key {
            KeyCode::Enter => {
                return Some(self.buffer.clone());
            }
            KeyCode::Char('/') if self.buffer.is_empty() => {
                self.buffer.push('/');
                self.cursor_pos = self.buffer.len();
                self.update_completions();
            }
            KeyCode::Char(c) => {
                // cursor_pos is a byte offset, so it must advance by the
                // character's encoded width. Advancing by one would leave it
                // inside a multi-byte character, and String::insert panics on a
                // non-boundary index — taking the whole process with it.
                self.buffer.insert(self.cursor_pos, c);
                self.cursor_pos += c.len_utf8();
                self.update_completions();
            }
            KeyCode::Backspace => {
                if let Some(previous) = self.previous_boundary() {
                    self.buffer.replace_range(previous..self.cursor_pos, "");
                    self.cursor_pos = previous;
                    self.update_completions();
                }
            }
            KeyCode::Left => {
                if let Some(previous) = self.previous_boundary() {
                    self.cursor_pos = previous;
                }
            }
            KeyCode::Right => {
                if let Some(next) = self.next_boundary() {
                    self.cursor_pos = next;
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

    /// Draw the prompt, what has been typed, and any completions.
    ///
    /// Raw mode does almost nothing on its own: a newline moves down a row but
    /// does not return to the first column, and nothing is erased unless it is
    /// erased here. Both were missing, so the list arrived as a diagonal
    /// staircase with the previous, longer list still visible underneath it.
    pub fn render(&mut self, stdout: &mut io::Stdout) -> io::Result<()> {
        let prompt_width =
            u16::try_from(UnicodeWidthStr::width(self.prompt.as_str())).unwrap_or(u16::MAX);

        if self.prompt_drawn {
            // Redraw only what can have changed. Everything from here down goes,
            // so a completion list that has grown shorter leaves no tail behind.
            stdout.queue(cursor::MoveToColumn(prompt_width))?;
            stdout.queue(Clear(ClearType::FromCursorDown))?;
        } else {
            stdout.queue(cursor::MoveToColumn(0))?;
            stdout.queue(Clear(ClearType::FromCursorDown))?;
            stdout.queue(SetForegroundColor(Color::Cyan))?;
            stdout.queue(Print(&self.prompt))?;
            stdout.queue(ResetColor)?;
            self.prompt_drawn = true;
        }

        stdout.queue(Print(&self.buffer))?;

        if self.show_completions {
            let shown = self.visible_completions();

            for (i, comp) in self.completions.iter().take(shown).enumerate() {
                // The carriage return is what puts each entry back at column 0.
                stdout.queue(Print("\r\n"))?;

                if i == self.selected {
                    stdout.queue(SetForegroundColor(Color::Green))?;
                    stdout.queue(Print(format!("> {} - {}", comp.display, comp.description)))?;
                    stdout.queue(ResetColor)?;
                } else {
                    stdout.queue(Print(format!("  {} - {}", comp.display, comp.description)))?;
                }
            }

            // Back to the line being edited, so typing continues where the user
            // is looking.
            if shown > 0 {
                stdout.queue(cursor::MoveUp(u16::try_from(shown).unwrap_or(u16::MAX)))?;
            }
        }

        stdout.queue(cursor::MoveToColumn(self.cursor_column()))?;
        stdout.flush()
    }

    /// How many entries there is room for below the prompt.
    ///
    /// Drawing past the last row scrolls the terminal, which moves the prompt
    /// out from under the cursor and leaves the next redraw erasing the wrong
    /// lines.
    fn visible_completions(&self) -> usize {
        const MAX_ENTRIES: usize = 8;

        let room = match cursor::position().map(|(_, row)| row) {
            Ok(row) => terminal::size()
                .map(|(_, rows)| rows.saturating_sub(row + 1) as usize)
                .unwrap_or(MAX_ENTRIES),
            Err(_) => MAX_ENTRIES,
        };

        self.completions.len().min(MAX_ENTRIES).min(room)
    }
}

impl Default for CompletingInput {
    fn default() -> Self {
        Self::new()
    }
}

/// Read input with live completion
pub fn read_input_with_completion() -> io::Result<Option<String>> {
    read_input_with_prompt("serdes-ai > ")
}

/// Read a line, drawing `prompt` in front of it.
pub fn read_input_with_prompt(prompt: &str) -> io::Result<Option<String>> {
    let mut stdout = io::stdout();
    let mut input = CompletingInput::with_prompt(prompt);

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
