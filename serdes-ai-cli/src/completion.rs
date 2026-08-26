//! Live completion dropdown (like prompt_toolkit)

use crossterm::{
    QueueableCommand, cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, Clear, ClearType},
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
/// Every command the application knows, for the completion list.
///
/// Taken from the command registry rather than a list kept here. The list here
/// held fourteen of them, so most commands could never be completed and any
/// command added later would have been missing too.
pub fn get_command_completions() -> Vec<Completion> {
    let mut completions: Vec<Completion> = crate::commands::registry::get_unique_commands()
        .into_iter()
        .map(|info| Completion {
            text: format!("/{}", info.name),
            display: format!("/{}", info.name),
            description: info.description,
        })
        .collect();

    completions.sort_by(|a, b| a.text.cmp(&b.text));
    completions
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
    /// Lines entered before, oldest first.
    history: Vec<String>,
    /// How far back through `history` the user has stepped.
    ///
    /// `None` means the line being typed rather than a recalled one, which is
    /// what Down returns to at the end.
    history_index: Option<usize>,
    /// The line being typed, kept while a previous one is being looked at.
    draft: String,
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
            history: Vec::new(),
            history_index: None,
            draft: String::new(),
            prompt_drawn: false,
        }
    }

    /// Give the line access to what was entered before.
    pub fn with_history(mut self, history: Vec<String>) -> Self {
        self.history = history;
        self
    }

    /// Step back to an earlier line.
    ///
    /// Returns whether there was one, so the caller can tell this apart from a
    /// key that should do something else.
    fn recall_earlier(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }

        let index = match self.history_index {
            // Keep what is being typed, so Down can come back to it.
            None => {
                self.draft = self.buffer.clone();
                self.history.len() - 1
            }
            Some(0) => return true,
            Some(index) => index - 1,
        };

        self.history_index = Some(index);
        self.buffer = self.history[index].clone();
        self.cursor_pos = self.buffer.len();
        self.show_completions = false;
        true
    }

    /// Step forward towards the line being typed.
    fn recall_later(&mut self) -> bool {
        let Some(index) = self.history_index else {
            return false;
        };

        if index + 1 < self.history.len() {
            self.history_index = Some(index + 1);
            self.buffer = self.history[index + 1].clone();
        } else {
            // Past the newest entry is the line that was being typed.
            self.history_index = None;
            self.buffer = std::mem::take(&mut self.draft);
        }

        self.cursor_pos = self.buffer.len();
        self.show_completions = false;
        true
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
                // Ctrl-O shows the most recent summarised output in full.
                KeyCode::Char('o') => {
                    show_full_output();
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
                // While the list is open, Enter chooses the highlighted entry
                // rather than submitting. Submitting there sent the "/" that had
                // been typed so far and threw the selection away, which made the
                // arrow keys pointless.
                if self.accept_completion() {
                    return None;
                }
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
            // While the list is open the arrows move the selection. With no
            // list they step through what was entered before, which is what
            // they do at every other shell prompt.
            KeyCode::Up => {
                if self.show_completions {
                    self.selected = self.selected.saturating_sub(1);
                } else {
                    self.recall_earlier();
                }
            }
            KeyCode::Down => {
                if self.show_completions {
                    if self.selected < self.completions.len().saturating_sub(1) {
                        self.selected += 1;
                    }
                } else {
                    self.recall_later();
                }
            }
            KeyCode::Tab => {
                self.accept_completion();
            }
            KeyCode::Esc => {
                self.show_completions = false;
            }
            _ => {}
        }
        None
    }

    /// Put the highlighted completion on the line and close the list.
    ///
    /// Returns whether there was a selection to take, so a caller can tell an
    /// accepted completion from a key that should do something else.
    fn accept_completion(&mut self) -> bool {
        if !self.show_completions {
            return false;
        }

        let Some(comp) = self.completions.get(self.selected) else {
            return false;
        };

        // Nothing to take when the line already says exactly this. Otherwise a
        // command typed out in full would be "completed" to itself instead of
        // running, and would need a second Enter.
        if self.buffer.trim_end() == comp.text {
            return false;
        }

        self.buffer = comp.text.clone();
        self.cursor_pos = self.buffer.len();
        self.show_completions = false;

        // A trailing space so an argument can be typed straight away: most of
        // these commands take one.
        if !self.buffer.ends_with(' ') {
            self.buffer.push(' ');
            self.cursor_pos = self.buffer.len();
        }

        true
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

    /// Hand the current state to whatever draws the input region.
    pub fn show(&self, draw: &dyn Fn(crate::screen::InputView)) {
        draw(self.view());
    }

    /// What the input region should show.
    pub fn view(&self) -> crate::screen::InputView {
        let entries = if self.show_completions {
            self.completions
                .iter()
                .take(crate::screen::MAX_ENTRIES)
                .map(|comp| format!("{} - {}", comp.display, comp.description))
                .collect()
        } else {
            Vec::new()
        };

        crate::screen::InputView {
            prompt: self.prompt.clone(),
            buffer: self.buffer.clone(),
            cursor_column: u16::try_from(UnicodeWidthStr::width(&self.buffer[..self.cursor_pos]))
                .unwrap_or(u16::MAX),
            entries,
            selected: self.selected,
        }
    }

    /// Put the finished line into the scrollback.
    ///
    /// The region is reused for the next prompt, so without this the line the
    /// user just entered would be overwritten and the session would lose its
    /// transcript.
    pub fn commit_to_scrollback(&self) {
        crate::screen::set_input(crate::screen::InputView::default());
        crate::screen::emit(&format!("{}{}\n", self.prompt, self.buffer));
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

/// Print the most recent summarised output in full.
///
/// It appears below rather than in place of the summary: the conversation is in
/// the terminal's own scrollback, which cannot be rewritten after the fact.
fn show_full_output() {
    match crate::collapse::take_latest() {
        Some(block) => {
            crate::screen::emit(&format!("\n{}\n{}\n", block.label, block.text));
        }
        None => {
            crate::screen::emit("\nNothing further to show.\n");
        }
    }
}

/// The most recent lines to make available to the arrow keys.
const HISTORY_LIMIT: usize = 500;

/// What was entered in previous sessions, oldest first.
///
/// Read from the same file `/history` shows. A missing or unreadable file is
/// simply an empty history: not being able to recall a previous line is no
/// reason to refuse to read a new one.
fn load_history() -> Vec<String> {
    let path = crate::config::get_command_history_file();
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };

    let mut lines: Vec<String> = Vec::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A line repeated immediately is one entry: holding Up through a run of
        // the same command is not useful.
        if lines.last().map(String::as_str) == Some(line) {
            continue;
        }
        lines.push(line.to_string());
    }

    if lines.len() > HISTORY_LIMIT {
        lines.drain(..lines.len() - HISTORY_LIMIT);
    }

    lines
}

/// Read input with live completion
pub fn read_input_with_completion() -> io::Result<Option<String>> {
    read_input_with_prompt("serdes-ai > ")
}

/// Read a line, drawing `prompt` in front of it.
///
/// The line is drawn into the region [`crate::screen`] keeps at the bottom, so
/// output arriving while the user types is placed above it rather than over it.
pub fn read_input_with_prompt(prompt: &str) -> io::Result<Option<String>> {
    let mut input = CompletingInput::with_prompt(prompt).with_history(load_history());

    terminal::enable_raw_mode()?;
    crate::screen::activate()?;

    // Anything that took the whole terminal since the last prompt — a picker,
    // the colour chooser — has wiped the region without telling it.
    crate::screen::invalidate();

    let finish = |outcome: io::Result<Option<String>>| -> io::Result<Option<String>> {
        // The finished line is pushed into the scrollback so the session reads
        // as a transcript, and the region is released for the next prompt.
        let _ = terminal::disable_raw_mode();
        outcome
    };

    loop {
        input.show(&crate::screen::set_input);

        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press {
                match input.handle_key_event(key) {
                    Some(InputOutcome::Submitted(result)) => {
                        input.commit_to_scrollback();
                        return finish(Ok(Some(result)));
                    }
                    // Cancelling one line is not the end of the session: the
                    // caller prints a notice and prompts again.
                    Some(InputOutcome::Cancelled) => {
                        input.commit_to_scrollback();
                        return finish(Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "input cancelled by the user",
                        )));
                    }
                    // End of input means the session is over.
                    Some(InputOutcome::EndOfInput) => {
                        input.commit_to_scrollback();
                        return finish(Ok(None));
                    }
                    None => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod completion_source_tests {
    #[tokio::test]
    async fn the_registry_supplies_the_completions() {
        crate::commands::init_all();
        let completions = super::get_command_completions();
        assert!(
            completions.len() > 14,
            "only {} completions came from the registry",
            completions.len()
        );
        assert!(completions.iter().any(|c| c.text == "/help"));
    }
}
