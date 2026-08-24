//! TUI Framework for interactive pickers and menus
//!
//! Uses ratatui for rendering and crossterm for input handling.

pub mod agent_picker;
pub mod autosave_menu;
pub mod colors_menu;
pub mod components;
pub mod model_picker;
pub mod model_settings;
pub mod theme;
pub mod tutorial;

use crossterm::{
    cursor::{Hide, Show},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, is_raw_mode_enabled, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, Write};

pub type TuiTerminal = Terminal<CrosstermBackend<io::Stdout>>;

/// Initialize TUI terminal
pub fn init_terminal() -> io::Result<TuiTerminal> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, Hide)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    terminal.hide_cursor()?;
    terminal.backend_mut().flush()?;

    Ok(terminal)
}

/// Restore terminal to normal mode
pub fn restore_terminal() -> io::Result<()> {
    let mut first_error: Option<io::Error> = None;

    if is_raw_mode_enabled().unwrap_or(false) {
        if let Err(err) = disable_raw_mode() {
            first_error = Some(err);
        }
    }

    let mut stdout = io::stdout();
    if let Err(err) = execute!(stdout, Show, LeaveAlternateScreen) {
        if first_error.is_none() {
            first_error = Some(err);
        }
    }

    if let Err(err) = stdout.flush() {
        if first_error.is_none() {
            first_error = Some(err);
        }
    }

    if let Some(err) = first_error {
        return Err(err);
    }

    Ok(())
}

/// Run a TUI app with proper cleanup
pub fn run_tui<F, T>(f: F) -> io::Result<T>
where
    F: FnOnce(&mut TuiTerminal) -> io::Result<T>,
{
    let mut terminal = init_terminal()?;
    let result = f(&mut terminal);

    let mut cleanup_error: Option<io::Error> = None;

    if let Err(err) = terminal.show_cursor() {
        cleanup_error = Some(err);
    }

    if let Err(err) = terminal.backend_mut().flush() {
        if cleanup_error.is_none() {
            cleanup_error = Some(err);
        }
    }

    if let Err(err) = restore_terminal() {
        if cleanup_error.is_none() {
            cleanup_error = Some(err);
        }
    }

    match (result, cleanup_error) {
        (Ok(value), None) => Ok(value),
        (Err(run_err), None) => Err(run_err),
        (Ok(_), Some(cleanup_err)) => Err(cleanup_err),
        (Err(run_err), Some(cleanup_err)) => Err(io::Error::other(format!(
            "TUI failed: {run_err}; restore failed: {cleanup_err}"
        ))),
    }
}

// Re-exports
// Deprecated: prefer inline picker in crate::picker
pub use autosave_menu::{interactive_autosave_menu, AutosaveMenu};
pub use colors_menu::{interactive_colors_menu, interactive_diff_menu};
pub use components::{ConfirmDialog, InputDialog, ListPicker, ProgressBar};
// Deprecated: prefer inline picker in crate::picker
pub use model_picker::ModelInfo;
pub use model_settings::{interactive_model_settings, ModelSettingsEditor};
pub use theme::Theme;
pub use tutorial::{
    mark_tutorial_complete, run_tutorial_wizard, should_run_tutorial, TutorialResult,
};
