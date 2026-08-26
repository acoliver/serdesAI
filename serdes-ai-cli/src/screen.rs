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
use crossterm::{QueueableCommand, cursor};

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
    /// How many of those rows are the block, as drawn.
    ///
    /// Recorded rather than recomputed: the input view can change between a
    /// draw and the erase that follows it — the completion list opening is
    /// enough — and deriving it from the current view then walks the cursor
    /// back by the wrong number of rows.
    drawn_block_rows: u16,
    view: InputView,
    /// Output not yet ending in a newline.
    ///
    /// Callers emit fragments — some text, then a newline as a separate call —
    /// and scrollback is written a whole line at a time.
    pending: String,
    /// The block that can currently be expanded, and what has been shown since.
    ///
    /// Held in the region rather than written to scrollback, because that is
    /// what makes it redrawable: expanding has to move what follows it down,
    /// and collapsing has to bring it back up.
    live: Option<LiveBlock>,
}

/// Output shown in summary, with the whole of it available.
#[derive(Debug, Clone)]
struct LiveBlock {
    /// The summary lines, shown when collapsed.
    summary: Vec<String>,
    /// Every line, shown when expanded.
    full: Vec<String>,
    /// Which of the two is being shown.
    expanded: bool,
    /// Everything displayed after the block — the model's answer, the turn
    /// summary — which has to move as the block grows and shrinks.
    trailing: Vec<String>,
}

impl LiveBlock {
    /// The lines to draw, in order.
    fn lines(&self, available: usize) -> Vec<String> {
        let body = if self.expanded {
            &self.full
        } else {
            &self.summary
        };

        let mut lines: Vec<String> = Vec::new();
        let room = available.saturating_sub(self.trailing.len());

        if body.len() > room && room > 0 {
            // An expansion taller than the screen cannot be drawn whole and
            // still be retractable: the region has to fit to be redrawn.
            lines.extend(body[..room.saturating_sub(1)].iter().cloned());
            lines.push(format!(
                "... showing {} of {} lines - ctrl+o to collapse",
                room.saturating_sub(1),
                body.len()
            ));
        } else {
            lines.extend(body.iter().cloned());
        }

        lines.extend(self.trailing.iter().cloned());
        lines
    }
}

impl Screen {
    fn new() -> io::Result<Self> {
        Ok(Self {
            out: io::stdout(),
            drawn_rows: 0,
            drawn_block_rows: 0,
            view: InputView::default(),
            pending: String::new(),
            live: None,
        })
    }

    /// Erase the region, leaving the cursor where it began.
    /// Erase the region, leaving the cursor at its first row.
    ///
    /// The cursor sits on the prompt line, which is below the block, so it has
    /// to walk back up before clearing or the block would be left behind.
    fn erase(&mut self) -> io::Result<()> {
        if self.drawn_rows == 0 {
            return Ok(());
        }

        if self.drawn_block_rows > 0 {
            self.out.queue(cursor::MoveUp(self.drawn_block_rows))?;
        }

        self.out.queue(cursor::MoveToColumn(0))?;
        self.out.queue(Clear(ClearType::FromCursorDown))?;
        self.drawn_rows = 0;
        self.drawn_block_rows = 0;
        self.out.flush()
    }

    /// How many rows the region may use, leaving the screen room to breathe.
    fn available_rows(&self) -> usize {
        let height = crossterm::terminal::size()
            .map(|(_, rows)| rows as usize)
            .unwrap_or(24);

        // One row is kept free so the region never sits flush against the top,
        // which would leave nothing of the conversation visible.
        height.saturating_sub(2)
    }

    /// Draw the expandable block, the prompt, what has been typed, and any
    /// completions.
    fn draw(&mut self) -> io::Result<()> {
        self.erase()?;

        let view = self.view.clone();
        let entries: Vec<&String> = view.entries.iter().take(MAX_ENTRIES).collect();

        // The block sits above the prompt and is redrawn with it, which is what
        // lets it grow and shrink in place.
        let available = self.available_rows().saturating_sub(view.rows() as usize);
        let block_lines = self
            .live
            .as_ref()
            .map(|live| live.lines(available))
            .unwrap_or_default();

        self.out.queue(cursor::MoveToColumn(0))?;
        for line in &block_lines {
            self.out.queue(Print(line))?;
            self.out.queue(Print("\r\n"))?;
        }
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

        self.drawn_block_rows = u16::try_from(block_lines.len()).unwrap_or(u16::MAX);
        self.drawn_rows = view.rows().saturating_add(self.drawn_block_rows);
        self.out.flush()
    }

    /// Move the expandable block into scrollback.
    ///
    /// It is written as it currently appears — expanded if the user expanded it
    /// — so committing does not change what is on screen.
    fn commit_live(&mut self) -> io::Result<()> {
        let Some(live) = self.live.take() else {
            return Ok(());
        };

        let lines = live.lines(self.available_rows());
        self.erase()?;

        for line in &lines {
            self.out.queue(cursor::MoveToColumn(0))?;
            self.out.queue(Print(line))?;
            self.out.queue(Print("\r\n"))?;
        }
        self.out.flush()?;

        self.draw()
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
        // While a block is expandable, what follows it belongs to the region:
        // expanding has to push it down and collapsing has to bring it back.
        if let Some(live) = self.live.as_mut() {
            live.trailing.extend(lines.iter().cloned());
            return self.draw();
        }

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

/// Forget the region and everything in it.
///
/// Called when a full-screen interface is about to take the terminal: it wipes
/// the rows the region was drawn in, so both the row count and the expandable
/// block it held are meaningless afterwards.
pub fn discard_region() {
    if let Ok(mut guard) = screen().lock() {
        if let Some(active) = guard.as_mut() {
            active.drawn_rows = 0;
            active.drawn_block_rows = 0;
            active.live = None;
        }
    }
}

/// Show `summary` in the region, keeping `full` for expansion.
///
/// Any block already expandable is written to scrollback first: only the most
/// recent one can be toggled, because only it is still being redrawn.
pub fn show_block(summary: &str, full: &str) {
    let Ok(mut guard) = screen().lock() else {
        return;
    };
    let Some(active) = guard.as_mut() else {
        return;
    };

    let _ = active.commit_live();

    active.live = Some(LiveBlock {
        summary: summary.lines().map(str::to_string).collect(),
        full: full.lines().map(str::to_string).collect(),
        expanded: false,
        trailing: Vec::new(),
    });

    let _ = active.draw();
}

/// Expand the block if it is collapsed, collapse it if it is expanded.
///
/// Returns whether there was one to toggle.
pub fn toggle_block() -> bool {
    let Ok(mut guard) = screen().lock() else {
        return false;
    };
    let Some(active) = guard.as_mut() else {
        return false;
    };
    let Some(live) = active.live.as_mut() else {
        return false;
    };

    live.expanded = !live.expanded;
    let _ = active.draw();
    true
}

/// Write the expandable block to scrollback, so it stops being redrawn.
///
/// Called when the user enters a new line: what has already happened should
/// stay where it is rather than move as later blocks come and go.
pub fn commit_block() {
    if let Ok(mut guard) = screen().lock() {
        if let Some(active) = guard.as_mut() {
            let _ = active.commit_live();
        }
    }
}

/// Whether a block can currently be expanded.
pub fn has_block() -> bool {
    screen()
        .lock()
        .map(|guard| guard.as_ref().is_some_and(|active| active.live.is_some()))
        .unwrap_or(false)
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
            active.drawn_block_rows = 0;
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
