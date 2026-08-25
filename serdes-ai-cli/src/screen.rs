//! The single owner of the terminal.
//!
//! Three things used to write to stdout with no coordination: the message bus,
//! the streaming answer renderer, and the input line. Each assumed the cursor
//! was where it had left it, so a message arriving while the user was typing
//! landed on the input line, and the completion list drew itself over whatever
//! happened to be below.
//!
//! Everything now goes through here. Output destined for scrollback is handed
//! to [`emit`], the input area is drawn into a region pinned at the bottom, and
//! because one place owns both it can erase exactly what it drew.
//!
//! The region is a ratatui inline viewport, so the conversation stays in the
//! terminal's own scrollback: it can be scrolled and selected with the mouse
//! like any other command output, which a full-screen interface would take
//! away.

use std::io::{self, Stdout, Write};
use std::sync::{Mutex, OnceLock};

use crossterm::style::{Color, Print, ResetColor, SetForegroundColor};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{cursor, QueueableCommand};

/// The most completion entries to show at once.
pub const MAX_ENTRIES: usize = 8;

/// The active screen, if the session is interactive.
///
/// A one-shot run has no input area to manage, so it writes straight to stdout
/// and never activates this.
static SCREEN: OnceLock<Mutex<Option<Screen>>> = OnceLock::new();

fn screen() -> &'static Mutex<Option<Screen>> {
    SCREEN.get_or_init(|| Mutex::new(None))
}

/// What to draw in the region at the bottom.
#[derive(Debug, Clone, Default)]
pub struct InputView {
    /// The prompt, drawn before what has been typed.
    pub prompt: String,
    /// What has been typed.
    pub buffer: String,
    /// Where the cursor sits, in columns from the start of the buffer.
    pub cursor_column: u16,
    /// The completion entries to offer, already filtered.
    pub entries: Vec<String>,
    /// Which entry is selected.
    pub selected: usize,
}

impl InputView {
    /// How many rows the region occupies: the line being typed, plus entries.
    fn rows(&self) -> u16 {
        let entries = self.entries.len().min(MAX_ENTRIES);
        u16::try_from(entries + 1).unwrap_or(u16::MAX)
    }
}

/// Owns the terminal for an interactive session.
///
/// The region at the bottom is drawn directly rather than through ratatui's
/// inline viewport: that viewport's height is fixed when it is created, and this
/// region grows and shrinks as the completion list opens and closes. Emulating
/// the change by rebuilding the viewport reserves rows by scrolling, so every
/// keystroke that opened or closed the list scrolled the conversation away.
///
/// Drawing it here keeps the one property that matters — a single owner that
/// knows exactly how many rows it drew, and so can erase exactly those.
struct Screen {
    out: Stdout,
    /// How many rows the region currently occupies on screen.
    drawn_rows: u16,
    view: InputView,
    /// Output not yet ending in a newline.
    ///
    /// Callers emit fragments — some text, then a newline as a separate call —
    /// and scrollback is written a whole line at a time.
    pending: String,
}

impl Screen {
    fn new() -> io::Result<Self> {
        Ok(Self {
            out: io::stdout(),
            drawn_rows: 0,
            view: InputView::default(),
            pending: String::new(),
        })
    }

    /// Erase the region, leaving the cursor where it began.
    fn erase(&mut self) -> io::Result<()> {
        if self.drawn_rows == 0 {
            return Ok(());
        }

        self.out.queue(cursor::MoveToColumn(0))?;
        self.out.queue(Clear(ClearType::FromCursorDown))?;
        self.drawn_rows = 0;
        self.out.flush()
    }

    /// Draw the prompt, what has been typed, and any completions.
    fn draw(&mut self) -> io::Result<()> {
        self.erase()?;

        let view = self.view.clone();
        let entries: Vec<&String> = view.entries.iter().take(MAX_ENTRIES).collect();

        self.out.queue(cursor::MoveToColumn(0))?;
        self.out.queue(SetForegroundColor(Color::Cyan))?;
        self.out.queue(Print(&view.prompt))?;
        self.out.queue(ResetColor)?;
        self.out.queue(Print(&view.buffer))?;

        for (index, entry) in entries.iter().enumerate() {
            // The carriage return is what puts each entry back at column 0:
            // in raw mode a newline moves down without returning.
            self.out.queue(Print("\r\n"))?;

            if index == view.selected {
                self.out.queue(SetForegroundColor(Color::Green))?;
                self.out.queue(Print(format!("> {entry}")))?;
                self.out.queue(ResetColor)?;
            } else {
                self.out.queue(Print(format!("  {entry}")))?;
            }
        }

        // Back up to the line being typed, so the cursor is where the user is
        // looking and the next erase starts from the top of the region.
        let entry_rows = view.rows().saturating_sub(1);
        if entry_rows > 0 {
            self.out.queue(cursor::MoveUp(entry_rows))?;
        }

        let column = display_width(&view.prompt).saturating_add(view.cursor_column);
        self.out.queue(cursor::MoveToColumn(column))?;

        self.drawn_rows = view.rows();
        self.out.flush()
    }

    /// Put text into the scrollback above the region.
    ///
    /// Only complete lines are written; a trailing fragment is held until the
    /// newline that finishes it arrives.
    fn emit(&mut self, text: &str) -> io::Result<()> {
        self.pending.push_str(text);

        let mut lines = Vec::new();
        while let Some(index) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=index).collect();
            lines.push(
                line.trim_end_matches('\n')
                    .trim_end_matches('\r')
                    .to_string(),
            );
        }

        if lines.is_empty() {
            return Ok(());
        }

        self.write_lines(&lines)
    }

    /// Flush a partial line, for output that never ends in a newline.
    fn flush_pending(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }

        let line = std::mem::take(&mut self.pending);
        self.write_lines(&[line])
    }

    fn write_lines(&mut self, lines: &[String]) -> io::Result<()> {
        self.erase()?;

        for line in lines {
            self.out.queue(cursor::MoveToColumn(0))?;
            self.out.queue(Print(line))?;
            self.out.queue(Print("\r\n"))?;
        }
        self.out.flush()?;

        self.draw()
    }
}

/// The number of columns a string occupies.
fn display_width(text: &str) -> u16 {
    use unicode_width::UnicodeWidthStr;
    u16::try_from(UnicodeWidthStr::width(text)).unwrap_or(u16::MAX)
}

/// Take ownership of the terminal for an interactive session.
pub fn activate() -> io::Result<()> {
    let mut guard = screen().lock().expect("screen lock poisoned");
    if guard.is_none() {
        *guard = Some(Screen::new()?);
    }
    Ok(())
}

/// Release the terminal.
pub fn deactivate() {
    let mut guard = screen().lock().expect("screen lock poisoned");
    if let Some(mut active) = guard.take() {
        let _ = active.flush_pending();
        let _ = active.erase();
    }
}

/// Forget what the region drew, without erasing anything.
///
/// A full-screen interface — a picker, the colour chooser — takes the terminal
/// and wipes whatever was there. The region would otherwise still believe its
/// rows were on screen and erase that many lines of something else.
pub fn invalidate() {
    if let Ok(mut guard) = screen().lock() {
        if let Some(active) = guard.as_mut() {
            active.drawn_rows = 0;
        }
    }
}

/// Whether an interactive session owns the terminal.
pub fn is_active() -> bool {
    screen()
        .lock()
        .map(|guard| guard.is_some())
        .unwrap_or(false)
}

/// Write styled text, into scrollback when a session is active.
///
/// The fallback is a plain write, so a one-shot run — which has no input area
/// to protect — behaves exactly as it did before.
pub fn emit(text: &str) {
    if text.is_empty() {
        return;
    }

    let mut guard = match screen().lock() {
        Ok(guard) => guard,
        Err(_) => return,
    };

    match guard.as_mut() {
        Some(active) => {
            let _ = active.emit(text);
        }
        None => {
            let mut stdout = io::stdout();
            let _ = write!(stdout, "{text}");
            let _ = stdout.flush();
        }
    }
}

/// Update what the input region shows.
pub fn set_input(view: InputView) {
    let mut guard = match screen().lock() {
        Ok(guard) => guard,
        Err(_) => return,
    };

    if let Some(active) = guard.as_mut() {
        active.view = view;
        if let Err(err) = active.draw() {
            tracing::warn!("could not draw the input region: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_region_is_one_row_when_nothing_is_being_completed() {
        let view = InputView {
            prompt: ">>> ".to_string(),
            ..InputView::default()
        };

        assert_eq!(view.rows(), 1);
    }

    #[test]
    fn the_region_grows_by_one_row_per_entry() {
        let view = InputView {
            prompt: ">>> ".to_string(),
            entries: vec!["/help".to_string(), "/model".to_string()],
            ..InputView::default()
        };

        assert_eq!(view.rows(), 3);
    }

    #[test]
    fn the_region_stops_growing_at_the_entry_limit() {
        // Otherwise a long list would take the whole terminal, leaving nowhere
        // for the conversation it is meant to sit beneath.
        let view = InputView {
            entries: (0..40).map(|i| i.to_string()).collect(),
            ..InputView::default()
        };

        assert_eq!(view.rows(), u16::try_from(MAX_ENTRIES + 1).unwrap());
    }

    #[test]
    fn width_is_measured_in_columns_not_bytes() {
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("日本"), 4);
    }
}
