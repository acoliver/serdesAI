//! A pseudo-terminal harness for driving the real CLI binary.
//!
//! The CLI writes ANSI escapes straight to stdout and reads keys in raw mode, so
//! neither piping stdout nor calling library functions tells you what a user
//! would actually see. This spawns the real binary on a PTY and feeds its output
//! through a terminal emulator, so assertions are made against the rendered
//! screen — the same grid of characters a person would be looking at.

#![allow(dead_code)]

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, NativePtySystem, PtySize, PtySystem};

/// How long assertions wait for the screen to reach an expected state.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// How often the screen is re-checked while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Default terminal size. Fixed so that layout assertions are reproducible
/// rather than depending on whatever terminal happens to run the tests.
pub const DEFAULT_COLS: u16 = 100;
/// Default terminal height.
pub const DEFAULT_ROWS: u16 = 30;

/// Builds a [`TerminalApp`].
pub struct AppBuilder {
    args: Vec<String>,
    env: Vec<(String, String)>,
    cwd: Option<PathBuf>,
    cols: u16,
    rows: u16,
    script: Option<String>,
    onboarded: bool,
    _fixture: Option<tempfile::TempDir>,
}

impl AppBuilder {
    fn new() -> Self {
        Self {
            args: Vec::new(),
            env: Vec::new(),
            cwd: None,
            cols: DEFAULT_COLS,
            rows: DEFAULT_ROWS,
            script: None,
            onboarded: true,
            _fixture: None,
        }
    }

    /// Present the app with a first-ever run, so onboarding is triggered.
    ///
    /// The default is an already-onboarded profile, since most tests want to
    /// reach the prompt rather than exercise the tutorial.
    pub fn fresh_install(mut self) -> Self {
        self.onboarded = false;
        self
    }

    /// Pass command-line arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Set an environment variable.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Run in `dir`.
    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    /// Use a specific terminal size.
    pub fn size(mut self, cols: u16, rows: u16) -> Self {
        self.cols = cols;
        self.rows = rows;
        self
    }

    /// Answer every model request from this script, given as JSON.
    ///
    /// Written to a temporary file and passed through `SERDES_AI_MOCK`, so the
    /// run is deterministic and never reaches a provider.
    pub fn script(mut self, json: impl Into<String>) -> Self {
        self.script = Some(json.into());
        self
    }

    /// Spawn the binary.
    pub fn spawn(mut self) -> anyhow::Result<TerminalApp> {
        let mut cmd = CommandBuilder::new(binary_path()?);
        for arg in &self.args {
            cmd.arg(arg);
        }

        // A predictable environment: no inherited API keys, no colour overrides,
        // and a terminal type the emulator understands.
        cmd.env("TERM", "xterm-256color");
        cmd.env("NO_COLOR", "");
        cmd.env("COLUMNS", self.cols.to_string());
        cmd.env("LINES", self.rows.to_string());
        for key in [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "GOOGLE_API_KEY",
            "GROQ_API_KEY",
        ] {
            cmd.env(key, "");
        }

        // Every run gets its own HOME. Without this the tests read and write the
        // real ~/.code_puppy: they would depend on the developer's settings and,
        // worse, could overwrite them.
        let sandbox = tempfile::tempdir()?;
        let home = sandbox.path().join("home");
        std::fs::create_dir_all(&home)?;
        cmd.env("HOME", &home);
        cmd.env("USERPROFILE", &home);
        cmd.env("XDG_CONFIG_HOME", home.join(".config"));
        cmd.env("XDG_DATA_HOME", home.join(".local/share"));

        if self.onboarded {
            let config_dir = home.join(".code_puppy");
            std::fs::create_dir_all(&config_dir)?;
            std::fs::write(
                config_dir.join("puppy.cfg"),
                "[puppy]\nonboarding_complete = true\n",
            )?;
        }

        if let Some(json) = &self.script {
            let path = sandbox.path().join("script.json");
            std::fs::write(&path, json)?;
            cmd.env("SERDES_AI_MOCK", &path);
        }

        self._fixture = Some(sandbox);

        for (key, value) in &self.env {
            cmd.env(key, value);
        }

        let cwd = match &self.cwd {
            Some(dir) => dir.clone(),
            None => std::env::temp_dir(),
        };
        cmd.cwd(&cwd);

        TerminalApp::launch(cmd, self.cols, self.rows, self._fixture)
    }
}

/// Locate the compiled `serdes-ai` binary next to the test executable.
fn binary_path() -> anyhow::Result<PathBuf> {
    let mut dir = std::env::current_exe()?;
    dir.pop(); // the test binary itself
    if dir.ends_with("deps") {
        dir.pop();
    }

    let candidate = dir.join(if cfg!(windows) {
        "serdes-ai.exe"
    } else {
        "serdes-ai"
    });

    if !candidate.exists() {
        anyhow::bail!(
            "the serdes-ai binary was not found at {}. UI tests need it built: \
             run `cargo build -p serdes-ai-cli` first, or use `cargo test -p serdes-ai-cli` \
             which builds it as a dependency.",
            candidate.display()
        );
    }

    Ok(candidate)
}

/// A running CLI process attached to a pseudo-terminal.
pub struct TerminalApp {
    writer: Box<dyn Write + Send>,
    parser: Arc<Mutex<vt100::Parser>>,
    /// Everything the process ever wrote.
    ///
    /// Separate from the emulator because the CLI clears the screen and leaves
    /// the alternate buffer as it exits; anything judged from the live screen
    /// alone would vanish at exactly the moment a test looks at it.
    raw: Arc<Mutex<Vec<u8>>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    output: Receiver<()>,
    _fixture: Option<tempfile::TempDir>,
    cols: u16,
    rows: u16,
}

impl TerminalApp {
    /// Start building an app to spawn.
    pub fn builder() -> AppBuilder {
        AppBuilder::new()
    }

    fn launch(
        cmd: CommandBuilder,
        cols: u16,
        rows: u16,
        fixture: Option<tempfile::TempDir>,
    ) -> anyhow::Result<Self> {
        let pty = NativePtySystem::default().openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let child = pty.slave.spawn_command(cmd)?;
        let writer = pty.master.take_writer()?;
        let mut reader = pty.master.try_clone_reader()?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
        let raw = Arc::new(Mutex::new(Vec::new()));
        let (tx, output) = mpsc::channel();

        // Read continuously: a PTY that is not drained will block the child once
        // its buffer fills, which would look like a hang rather than a failure.
        let sink = Arc::clone(&parser);
        let log = Arc::clone(&raw);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut parser) = sink.lock() {
                            parser.process(&buf[..n]);
                        }
                        if let Ok(mut log) = log.lock() {
                            log.extend_from_slice(&buf[..n]);
                        }
                        // Best-effort wake-up; a full channel is not a problem.
                        let _ = tx.send(());
                    }
                }
            }
        });

        Ok(Self {
            writer,
            parser,
            raw,
            child,
            output,
            _fixture: fixture,
            cols,
            rows,
        })
    }

    /// The visible screen, one entry per row, trailing spaces trimmed.
    pub fn screen(&self) -> Vec<String> {
        let parser = self.parser.lock().expect("parser lock poisoned");
        let screen = parser.screen();
        (0..self.rows)
            .map(|row| {
                (0..self.cols)
                    .map(|col| {
                        screen
                            .cell(row, col)
                            .map(|c| c.contents())
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| " ".to_string())
                    })
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// The whole screen as one string.
    pub fn screen_text(&self) -> String {
        self.screen().join("\n")
    }

    /// Everything the process has written, with escape sequences removed.
    ///
    /// This is what a user saw over the whole session, including lines that have
    /// since scrolled away or been cleared. Use it to ask "did this ever appear";
    /// use [`TerminalApp::screen`] to ask "what is displayed now".
    pub fn transcript(&self) -> String {
        let raw = self.raw.lock().expect("raw log lock poisoned");
        strip_ansi(&raw)
    }

    /// The raw bytes written, escape sequences intact.
    pub fn raw_output(&self) -> Vec<u8> {
        self.raw.lock().expect("raw log lock poisoned").clone()
    }

    /// Send raw bytes to the process.
    pub fn send(&mut self, input: impl AsRef<str>) -> anyhow::Result<()> {
        self.writer.write_all(input.as_ref().as_bytes())?;
        self.writer.flush()?;
        Ok(())
    }

    /// Send a line, followed by Enter.
    ///
    /// Prefer [`TerminalApp::type_line`] for interactive prompts: this sends the
    /// text and the newline together, which races against the application
    /// entering its read loop.
    pub fn send_line(&mut self, line: impl AsRef<str>) -> anyhow::Result<()> {
        self.send(format!("{}\r", line.as_ref()))
    }

    /// Type `line`, wait for the application to echo it, then press Enter.
    ///
    /// A prompt appearing on screen does not mean the application is reading
    /// yet: it prints the prompt, then enables raw mode, then blocks on input.
    /// Sending text and Enter into that gap makes tests fail intermittently
    /// under load. Waiting for the echo proves the input was received before the
    /// newline commits it.
    pub fn type_line(&mut self, line: impl AsRef<str>) -> anyhow::Result<()> {
        let line = line.as_ref();
        self.send(line)?;
        self.wait_for(line)?;
        self.send_key(Key::Enter)
    }

    /// Send a named key.
    pub fn send_key(&mut self, key: Key) -> anyhow::Result<()> {
        self.send(key.sequence())
    }

    /// Wait until `needle` appears anywhere on screen.
    ///
    /// Returns an error containing the final screen, so a failure shows what was
    /// actually displayed rather than only what was missing.
    pub fn wait_for(&self, needle: &str) -> anyhow::Result<()> {
        self.wait_for_with_timeout(needle, DEFAULT_TIMEOUT)
    }

    /// Wait for `needle`, giving up after `timeout`.
    pub fn wait_for_with_timeout(&self, needle: &str, timeout: Duration) -> anyhow::Result<()> {
        let deadline = Instant::now() + timeout;

        loop {
            if self.transcript().contains(needle) || self.screen_text().contains(needle) {
                return Ok(());
            }

            if Instant::now() >= deadline {
                anyhow::bail!(
                    "timed out after {:?} waiting for {needle:?}.\n\
                     ---- screen ----\n{}\n----------------",
                    timeout,
                    self.screen_text()
                );
            }

            // Wake on new output where possible, but poll regardless so a screen
            // that changed before this call still counts.
            let _ = self.output.recv_timeout(POLL_INTERVAL);
        }
    }

    /// Wait for the process to exit and report its status.
    pub fn wait_for_exit(&mut self) -> anyhow::Result<u32> {
        let status = self.child.wait()?;
        Ok(status.exit_code())
    }

    /// Whether the process has exited.
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Assert the screen contains `needle` right now, without waiting.
    pub fn assert_contains(&self, needle: &str) {
        let text = self.transcript();
        assert!(
            text.contains(needle),
            "expected {needle:?} on screen.\n---- screen ----\n{}\n----------------",
            self.screen_text()
        );
    }

    /// Assert `needle` is absent.
    pub fn assert_not_contains(&self, needle: &str) {
        let text = self.transcript();
        assert!(
            !text.contains(needle),
            "did not expect {needle:?} on screen.\n---- screen ----\n{}\n----------------",
            self.screen_text()
        );
    }

    /// Assert a specific row contains `needle`.
    pub fn assert_row_contains(&self, row: usize, needle: &str) {
        let screen = self.screen();
        let actual = screen.get(row).map(String::as_str).unwrap_or("");
        assert!(
            actual.contains(needle),
            "row {row} was {actual:?}, expected it to contain {needle:?}.\n\
             ---- screen ----\n{}\n----------------",
            self.screen_text()
        );
    }
}

impl Drop for TerminalApp {
    fn drop(&mut self) {
        // Never leave a child running after a failed assertion.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Remove ANSI escape sequences, leaving the text a user would have read.
///
/// Deliberately small: it handles the CSI, OSC and simple-escape forms a
/// terminal application emits, and passes anything else through rather than
/// guessing.
fn strip_ansi(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }

        match chars.peek() {
            // CSI: ends at the first byte in @..~
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: ends at BEL or ESC \
            Some(']') => {
                chars.next();
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            // Two-character escapes.
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }

    out
}

/// Keys that have no plain-text representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Return.
    Enter,
    /// Escape.
    Esc,
    /// Ctrl-C.
    CtrlC,
    /// Ctrl-D.
    CtrlD,
    /// Tab.
    Tab,
    /// Backspace.
    Backspace,
    /// Cursor up.
    Up,
    /// Cursor down.
    Down,
    /// Cursor left.
    Left,
    /// Cursor right.
    Right,
}

impl Key {
    /// The bytes a terminal sends for this key.
    pub fn sequence(self) -> &'static str {
        match self {
            Key::Enter => "\r",
            Key::Esc => "\x1b",
            Key::CtrlC => "\x03",
            Key::CtrlD => "\x04",
            Key::Tab => "\t",
            Key::Backspace => "\x7f",
            Key::Up => "\x1b[A",
            Key::Down => "\x1b[B",
            Key::Right => "\x1b[C",
            Key::Left => "\x1b[D",
        }
    }
}

/// Build a script that answers with a single line of text.
pub fn says(text: &str) -> String {
    serde_json::json!({ "turns": [{ "text": { "text": text } }] }).to_string()
}

/// Build a script from an ordered list of turns.
pub fn script(turns: Vec<serde_json::Value>) -> String {
    serde_json::json!({ "turns": turns }).to_string()
}

/// A turn that replies with text.
pub fn text_turn(text: &str) -> serde_json::Value {
    serde_json::json!({ "text": { "text": text } })
}

/// A turn that calls a tool.
pub fn tool_turn(tool: &str, args: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "tool": { "tool": tool, "args": args } })
}

/// A turn that fails the request.
pub fn error_turn(message: &str) -> serde_json::Value {
    serde_json::json!({ "error": { "error": message } })
}

/// A scratch directory that cleans itself up.
pub fn workspace() -> anyhow::Result<tempfile::TempDir> {
    Ok(tempfile::tempdir()?)
}

/// Fail a test if `path` does not exist, showing what is there instead.
pub fn assert_file_contains(path: &Path, needle: &str) {
    let actual = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", path.display()));
    assert!(
        actual.contains(needle),
        "{} did not contain {needle:?}; it holds:\n{actual}",
        path.display()
    );
}
