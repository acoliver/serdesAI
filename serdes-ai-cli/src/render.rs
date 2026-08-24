//! Inline console rendering primitives (rich.Console style).
//!
//! This module intentionally does **not** take over the full terminal screen.
//! It prints inline as events happen so interactive workflows and logs can
//! coexist naturally.

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use crossterm::{
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal, QueueableCommand,
};

const DEFAULT_MAX_WIDTH: usize = 80;
const MIN_PANEL_WIDTH: usize = 10;

/// Lightweight inline console renderer.
///
/// Similar in spirit to Python rich.Console usage, but focused on
/// append-only inline rendering and simple styled primitives.
#[derive(Debug, Clone)]
pub struct InlineConsole {
    stdout: Arc<Mutex<io::Stdout>>,
}

impl InlineConsole {
    /// Create a new inline console bound to stdout.
    #[must_use]
    pub fn new() -> Self {
        Self {
            stdout: Arc::new(Mutex::new(io::stdout())),
        }
    }

    /// Print text inline, optionally with a foreground color.
    pub fn print(&mut self, text: &str, color: Option<Color>) -> io::Result<()> {
        let mut stdout = self
            .stdout
            .lock()
            .expect("stdout lock poisoned in InlineConsole::print");

        if let Some(c) = color {
            stdout.queue(SetForegroundColor(c))?;
        }

        stdout.queue(Print(text))?;

        if color.is_some() {
            stdout.queue(ResetColor)?;
        }

        stdout.flush()
    }

    /// Print a newline-terminated status message using a named style.
    pub fn print_status(&mut self, text: &str, style: &str) -> io::Result<()> {
        let color = match style {
            "success" => Color::Green,
            "error" => Color::Red,
            "warning" => Color::Yellow,
            "info" => Color::Cyan,
            _ => Color::White,
        };

        let mut stdout = self
            .stdout
            .lock()
            .expect("stdout lock poisoned in InlineConsole::print_status");
        stdout.queue(SetForegroundColor(color))?;
        stdout.queue(Print(text))?;
        stdout.queue(Print("\n"))?;
        stdout.queue(ResetColor)?;
        stdout.flush()
    }

    /// Print an inline panel box with title and content.
    ///
    /// The panel width is capped to terminal width and `DEFAULT_MAX_WIDTH`.
    /// Content is wrapped to avoid border overflow.
    pub fn print_panel(
        &mut self,
        title: &str,
        content: &str,
        border_color: Color,
    ) -> io::Result<()> {
        let panel_width = resolve_panel_width();
        let inner_width = panel_width.saturating_sub(4); // `│ ` + ` │`

        let title_line = truncate_to_width(title, inner_width);
        let wrapped_content = wrap_text(content, inner_width);

        let mut stdout = self
            .stdout
            .lock()
            .expect("stdout lock poisoned in InlineConsole::print_panel");

        stdout.queue(SetForegroundColor(border_color))?;

        // Top border
        stdout.queue(Print("┌"))?;
        stdout.queue(Print("─".repeat(panel_width.saturating_sub(2))))?;
        stdout.queue(Print("┐\n"))?;

        // Title row
        stdout.queue(Print("│ "))?;
        stdout.queue(Print(&title_line))?;
        stdout.queue(Print(
            " ".repeat(inner_width.saturating_sub(display_width(&title_line))),
        ))?;
        stdout.queue(Print(" │\n"))?;

        // Separator
        stdout.queue(Print("├"))?;
        stdout.queue(Print("─".repeat(panel_width.saturating_sub(2))))?;
        stdout.queue(Print("┤\n"))?;

        // Content rows
        if wrapped_content.is_empty() {
            stdout.queue(Print("│ "))?;
            stdout.queue(Print(" ".repeat(inner_width)))?;
            stdout.queue(Print(" │\n"))?;
        } else {
            for line in wrapped_content {
                stdout.queue(Print("│ "))?;
                stdout.queue(Print(&line))?;
                stdout.queue(Print(
                    " ".repeat(inner_width.saturating_sub(display_width(&line))),
                ))?;
                stdout.queue(Print(" │\n"))?;
            }
        }

        // Bottom border
        stdout.queue(Print("└"))?;
        stdout.queue(Print("─".repeat(panel_width.saturating_sub(2))))?;
        stdout.queue(Print("┘\n"))?;

        stdout.queue(ResetColor)?;
        stdout.flush()
    }
}

fn resolve_panel_width() -> usize {
    let terminal_width = terminal::size()
        .map(|(w, _)| usize::from(w))
        .unwrap_or(DEFAULT_MAX_WIDTH);

    terminal_width.min(DEFAULT_MAX_WIDTH).max(MIN_PANEL_WIDTH)
}

fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return Vec::new();
    }

    let mut lines = Vec::new();

    for paragraph in text.lines() {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current = String::new();

        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                if display_width(word) <= max_width {
                    current.push_str(word);
                } else {
                    lines.extend(chunk_by_width(word, max_width));
                }
                continue;
            }

            let candidate = format!("{current} {word}");
            if display_width(&candidate) <= max_width {
                current = candidate;
            } else {
                lines.push(std::mem::take(&mut current));
                if display_width(word) <= max_width {
                    current = word.to_string();
                } else {
                    lines.extend(chunk_by_width(word, max_width));
                }
            }
        }

        if !current.is_empty() {
            lines.push(current);
        }
    }

    lines
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }

    if max_width <= 1 {
        return "…".to_string();
    }

    let mut out = String::new();
    for ch in text.chars() {
        let candidate = format!("{out}{ch}");
        if display_width(&candidate) >= max_width {
            break;
        }
        out.push(ch);
    }

    out.push('…');
    out
}

fn chunk_by_width(text: &str, max_width: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        let candidate = format!("{current}{ch}");
        if display_width(&candidate) > max_width {
            if !current.is_empty() {
                chunks.push(current.clone());
                current.clear();
            }
            current.push(ch);
        } else {
            current.push(ch);
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

fn display_width(text: &str) -> usize {
    text.chars().count()
}
