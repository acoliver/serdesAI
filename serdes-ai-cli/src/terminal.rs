use std::env;
use std::io::{self, Write};
#[cfg(windows)]
use std::sync::atomic::AtomicU32;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::{execute, terminal};

// We store original stdin mode so we can optionally restore it.
// 0 means "not captured yet".
#[cfg(windows)]
static ORIGINAL_STDIN_MODE: AtomicU32 = AtomicU32::new(0);
static KEEP_CTRL_C_DISABLED: AtomicBool = AtomicBool::new(false);

/// Check whether we are running on Windows.
#[must_use]
pub const fn is_windows() -> bool {
    cfg!(target_os = "windows")
}

/// Check whether we are running on macOS.
#[must_use]
pub const fn is_macos() -> bool {
    cfg!(target_os = "macos")
}

/// Check whether we are running on Linux.
#[must_use]
pub const fn is_linux() -> bool {
    cfg!(target_os = "linux")
}

/// Check if terminal supports truecolor and print warning if not
pub fn print_truecolor_warning() {
    if !supports_truecolor() {
        eprintln!("\n  Warning: Your terminal does not support truecolor (24-bit color).");
        eprintln!("Some visual features may not display correctly.");
        eprintln!("For best experience, use a modern terminal like:");
        eprintln!("   - iTerm2 (macOS)");
        eprintln!("   - Windows Terminal (Windows)");
        eprintln!("   - Alacritty, Kitty, or WezTerm (Linux/macOS)");
        eprintln!();
    }
}

/// Check if terminal supports truecolor
#[must_use]
fn supports_truecolor() -> bool {
    // Check COLORTERM environment variable
    if let Ok(colorterm) = std::env::var("COLORTERM") {
        if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            return true;
        }
    }

    // Check for known truecolor terminals
    if let Ok(term) = std::env::var("TERM") {
        let truecolor_terms = [
            "xterm-256color",
            "screen-256color",
            "tmux-256color",
            "alacritty",
            "kitty",
            "wezterm",
            "iterm",
            "vscode",
            "vscodium",
        ];

        for tt in &truecolor_terms {
            if term.to_lowercase().contains(tt) {
                return true;
            }
        }
    }

    // Check TERM_PROGRAM
    if let Ok(term_program) = std::env::var("TERM_PROGRAM") {
        match term_program.as_str() {
            "iTerm.app" | "Apple_Terminal" | "vscode" | "WezTerm" => return true,
            _ => {}
        }
    }

    false
}

/// Check if terminal supports 256 colors.
#[must_use]
pub fn supports_256color() -> bool {
    let term = env::var("TERM").unwrap_or_default().to_ascii_lowercase();
    let colorterm = env::var("COLORTERM")
        .unwrap_or_default()
        .to_ascii_lowercase();

    term.contains("256color")
        || colorterm.contains("256")
        || colorterm.contains("truecolor")
        || colorterm.contains("24bit")
}

/// Check if terminal supports at least 16 colors.
#[must_use]
pub fn supports_16color() -> bool {
    if supports_256color() {
        return true;
    }

    let term = env::var("TERM").unwrap_or_default().to_ascii_lowercase();

    // Conservative allow-list for common color-capable terminals.
    [
        "xterm", "screen", "tmux", "ansi", "color", "linux", "cygwin",
    ]
    .iter()
    .any(|token| term.contains(token))
}

/// Returns terminal size as (width, height), with a safe fallback.
#[must_use]
pub fn get_terminal_size() -> (u16, u16) {
    terminal::size().unwrap_or((80, 24))
}

/// Clear terminal screen and move cursor to top-left.
pub fn clear_screen() {
    print!("\x1B[2J\x1B[H");
    let _ = io::stdout().flush();
}

/// Reset ANSI state quickly on Windows by writing SGR reset to stdout/stderr.
pub fn reset_windows_terminal_ansi() {
    if !is_windows() {
        return;
    }

    let mut stdout = io::stdout();
    let _ = stdout.write_all(b"\x1b[0m");
    let _ = stdout.flush();

    let mut stderr = io::stderr();
    let _ = stderr.write_all(b"\x1b[0m");
    let _ = stderr.flush();
}

/// Reset Unix-like terminals to a sane state (best-effort).
pub fn reset_unix_terminal() {
    if is_windows() {
        return;
    }

    // Crossterm reset primitives (best effort).
    let mut stdout = io::stdout();
    let _ = execute!(
        stdout,
        terminal::LeaveAlternateScreen,
        terminal::DisableLineWrap,
        terminal::EnableLineWrap,
        terminal::Clear(terminal::ClearType::All)
    );

    let _ = terminal::disable_raw_mode();
}

/// Full Windows terminal reset (ANSI + console mode + input buffer flush + raw mode off).
pub fn reset_windows_terminal_full() {
    if !is_windows() {
        return;
    }

    reset_windows_terminal_ansi();

    #[cfg(windows)]
    {
        let _ = windows_impl::restore_sane_console_modes();
        let _ = windows_impl::flush_console_input_buffer();
    }

    let _ = terminal::disable_raw_mode();
}

/// Enable ANSI support where needed (mainly Windows).
/// On non-Windows this is effectively a no-op success path.
pub fn enable_ansi_support() {
    if is_windows() {
        #[cfg(windows)]
        {
            let _ = windows_impl::enable_windows_ansi_and_utf8();
        }
    }
}

/// Check if ANSI escape support is available.
#[must_use]
pub fn supports_ansi() -> bool {
    if !is_windows() {
        return true;
    }

    #[cfg(windows)]
    {
        return windows_impl::check_windows_ansi_support().unwrap_or(false);
    }

    #[allow(unreachable_code)]
    false
}

/// Windows+uvx detector used for alternate cancel key behavior.
#[must_use]
pub fn should_use_alternate_cancel_key() -> bool {
    if !is_windows() {
        return false;
    }

    // "uvx specific" heuristic: if UVX env vars are present, prefer alternate key.
    // We keep this conservative and explicit.
    env::var("UVX_ACTIVE").is_ok()
        || env::var("UVX_MODE").is_ok()
        || env::var("UV_TOOL_BIN_DIR")
            .map(|v| v.to_ascii_lowercase().contains("uvx"))
            .unwrap_or(false)
        || env::var("_")
            .map(|v| {
                let lower = v.to_ascii_lowercase();
                lower.contains("uvx") || lower.ends_with("\\uv.exe") || lower.ends_with("/uv")
            })
            .unwrap_or(false)
}

/// Disable Ctrl+C signal generation in Windows console input mode.
///
/// Returns true on success (or non-Windows no-op), false on failure.
#[must_use]
pub fn disable_windows_ctrl_c() -> bool {
    if !is_windows() {
        return true;
    }

    #[cfg(windows)]
    {
        return windows_impl::disable_ctrl_c_processed_input().unwrap_or(false);
    }

    #[allow(unreachable_code)]
    false
}

/// Set whether Ctrl+C should be kept disabled.
pub fn set_keep_ctrl_c_disabled(enabled: bool) {
    KEEP_CTRL_C_DISABLED.store(enabled, Ordering::SeqCst);
}

/// Re-disable Ctrl+C if requested.
///
/// Returns true if state is acceptable, false if re-disable failed.
#[must_use]
pub fn ensure_ctrl_c_disabled() -> bool {
    if !KEEP_CTRL_C_DISABLED.load(Ordering::SeqCst) {
        return true;
    }

    disable_windows_ctrl_c()
}

#[cfg(windows)]
mod windows_impl {
    use super::{env, Ordering, ORIGINAL_STDIN_MODE};
    use std::io;

    type Handle = *mut core::ffi::c_void;

    const STD_INPUT_HANDLE: i32 = -10;
    const STD_OUTPUT_HANDLE: i32 = -11;

    const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
    const ENABLE_LINE_INPUT: u32 = 0x0002;
    const ENABLE_ECHO_INPUT: u32 = 0x0004;

    const ENABLE_PROCESSED_OUTPUT: u32 = 0x0001;
    const ENABLE_WRAP_AT_EOL_OUTPUT: u32 = 0x0002;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;

    const CP_UTF8: u32 = 65001;

    #[link(name = "Kernel32")]
    extern "system" {
        fn GetStdHandle(nStdHandle: i32) -> Handle;
        fn GetConsoleMode(hConsoleHandle: Handle, lpMode: *mut u32) -> i32;
        fn SetConsoleMode(hConsoleHandle: Handle, dwMode: u32) -> i32;
        fn FlushConsoleInputBuffer(hConsoleInput: Handle) -> i32;
        fn GetConsoleOutputCP() -> u32;
        fn SetConsoleOutputCP(wCodePageID: u32) -> i32;
        fn SetConsoleCP(wCodePageID: u32) -> i32;
    }

    fn get_std_handle(which: i32) -> io::Result<Handle> {
        // SAFETY: Win32 API call with constant selector.
        let handle = unsafe { GetStdHandle(which) };
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(handle)
        }
    }

    fn get_console_mode(handle: Handle) -> io::Result<u32> {
        let mut mode = 0_u32;
        // SAFETY: valid pointer to writable u32, handle comes from GetStdHandle.
        let ok = unsafe { GetConsoleMode(handle, &mut mode as *mut u32) };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(mode)
        }
    }

    fn set_console_mode(handle: Handle, mode: u32) -> io::Result<()> {
        // SAFETY: handle from GetStdHandle, mode is value type.
        let ok = unsafe { SetConsoleMode(handle, mode) };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub(super) fn enable_windows_ansi_and_utf8() -> io::Result<()> {
        let stdout = get_std_handle(STD_OUTPUT_HANDLE)?;
        let mode = get_console_mode(stdout)?;

        let new_mode = mode
            | ENABLE_PROCESSED_OUTPUT
            | ENABLE_WRAP_AT_EOL_OUTPUT
            | ENABLE_VIRTUAL_TERMINAL_PROCESSING;

        let _ = set_console_mode(stdout, new_mode);

        // Legacy Windows handling: ensure UTF-8 code page.
        // Best effort only; ignore if unsupported.
        // SAFETY: direct Win32 call with constant value.
        let current_cp = unsafe { GetConsoleOutputCP() };
        if current_cp != CP_UTF8 {
            // SAFETY: direct Win32 call with constant value.
            let _ = unsafe { SetConsoleOutputCP(CP_UTF8) };
            // SAFETY: direct Win32 call with constant value.
            let _ = unsafe { SetConsoleCP(CP_UTF8) };
        }

        Ok(())
    }

    pub(super) fn check_windows_ansi_support() -> io::Result<bool> {
        // Windows Terminal and modern hosts usually expose this.
        if env::var("WT_SESSION").is_ok() {
            return Ok(true);
        }

        let stdout = get_std_handle(STD_OUTPUT_HANDLE)?;
        let mode = get_console_mode(stdout)?;
        Ok((mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING) != 0)
    }

    pub(super) fn disable_ctrl_c_processed_input() -> io::Result<bool> {
        let stdin = get_std_handle(STD_INPUT_HANDLE)?;
        let mode = get_console_mode(stdin)?;

        if ORIGINAL_STDIN_MODE.load(Ordering::SeqCst) == 0 {
            ORIGINAL_STDIN_MODE.store(mode, Ordering::SeqCst);
        }

        let new_mode = mode & !ENABLE_PROCESSED_INPUT;
        set_console_mode(stdin, new_mode)?;
        Ok(true)
    }

    pub(super) fn restore_sane_console_modes() -> io::Result<()> {
        let stdout = get_std_handle(STD_OUTPUT_HANDLE)?;
        let stdout_mode = get_console_mode(stdout)?;
        let stdout_new = stdout_mode
            | ENABLE_PROCESSED_OUTPUT
            | ENABLE_WRAP_AT_EOL_OUTPUT
            | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        let _ = set_console_mode(stdout, stdout_new);

        let stdin = get_std_handle(STD_INPUT_HANDLE)?;
        let current_in = get_console_mode(stdin)?;

        let mut stdin_new = current_in | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT;

        let original = ORIGINAL_STDIN_MODE.load(Ordering::SeqCst);
        if original != 0 {
            // Restore baseline, but keep line/echo to prevent broken shell input.
            stdin_new = original | ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT;
        }

        let _ = set_console_mode(stdin, stdin_new);
        Ok(())
    }

    pub(super) fn flush_console_input_buffer() -> io::Result<()> {
        let stdin = get_std_handle(STD_INPUT_HANDLE)?;
        // SAFETY: stdin handle from GetStdHandle.
        let ok = unsafe { FlushConsoleInputBuffer(stdin) };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
