# Terminal UI tests

These drive the real `serdes-ai` binary on a pseudo-terminal and assert on what
a user would actually see. They exist because neither unit tests nor piping
stdout tell you whether the interface works: the CLI writes ANSI escapes, reads
keys in raw mode, clears the screen on exit, and reads configuration from the
user's home directory.

## Running them

    cargo test -p serdes-ai-cli --test ui_smoke

The binary is built automatically as a dependency of the test target.

## Writing one

```rust
let mut app = TerminalApp::builder()
    .args(["-p", "say hello"])
    .script(says("Hello"))     // answers every model request
    .spawn()?;

app.wait_for("Hello")?;
assert_eq!(app.wait_for_exit()?, 0);
```

For interactive flows, prefer `type_line` over `send_line`:

```rust
app.wait_for(">>>")?;          // the prompt is "(model) >>> "
app.type_line("do something")?;
app.wait_for("done")?;
```

## Why the harness works the way it does

Each of these solves a failure that actually happened while building it.

**Runs are hermetic.** Every spawn gets its own `HOME`, so tests never read or
write the developer's real `~/.code_puppy`. Without it they depended on local
settings and could overwrite them. `fresh_install()` opts into first-run
behaviour when a test wants to exercise onboarding.

**No network, ever.** `script(...)` writes a fixture and points `SERDES_AI_MOCK`
at it, which makes the CLI replay it instead of calling a provider. A malformed
script is an error rather than a fallback: a test that quietly started calling a
real API would be worse than one that fails.

**The transcript is separate from the screen.** The CLI clears the screen and
leaves the alternate buffer as it exits, so anything judged from the live screen
alone vanishes at exactly the moment a test looks at it. `transcript()` is
everything ever written, with escapes stripped; `screen()` is what is displayed
right now. Use the first for "did this appear", the second for layout.

**Input is echo-verified.** A prompt on screen does not mean the application is
reading yet — it prints the prompt, enables raw mode, then blocks. Sending text
and Enter into that gap fails intermittently under load. `type_line` waits for
the application to echo the text before committing it with Enter.

**Input waits for a fresh prompt.** Output is written by a separate message bus,
so a command's text can still be streaming when its echo appears. Typing into
that gap interleaves with the output and the resulting Enter lands on a clobbered
line, which never submits. `type_line` waits for the prompt for the next turn
before typing, and submissions are counted inside `send` so a bare
`send_key(Enter)` cannot drift from the count.

**No bare Escape.** A lone escape byte is ambiguous — a terminal has to wait to
see whether it begins a sequence — so sending one can swallow the characters
typed straight after it. An early version of the command sweep did this and
reported a different set of "broken" commands on every run.

**Concurrency is bounded.** Each test spawns a real process on its own
pseudo-terminal, and cargo runs the suites in parallel. Without a cap the
machine ends up with dozens of debug-build processes competing for PTYs, and
startup alone can exceed the assertion timeout — a failure that looks like an
application bug but is only contention. At most eight applications run at once.

**Failures show the screen.** Every assertion prints the rendered screen on
failure, because "expected X, not found" is useless when the question is what
the interface actually did.

## What this found

**Ctrl-C and Ctrl-D did nothing.** The startup banner promises "Ctrl+C to cancel
current processing, Ctrl+D to exit cleanly", but the input loop handled neither.
Raw mode delivers them as ordinary key events, so Ctrl-C fell through to the
`Char(c)` arm and typed a literal `c` into the buffer.

**The input buffer was indexed by bytes but stepped by characters.** `cursor_pos`
advanced one per character while indexing a `String` by byte offset, so after any
multi-byte character the cursor sat inside it — and `String::insert` and
`String::remove` panic on a non-boundary index. Typing an accent or an emoji and
then continuing to edit killed the process.

Both are fixed and held in place by tests. Note that several apparent findings
turned out to be defects in this harness rather than in the CLI — a test that
names a different culprit on each run is worse than no test, so treat an
intermittent failure here as a bug in the harness until proven otherwise.

## Not covered, and why

**Twelve of the twenty-six `AnyMessage` variants have no producer.** `Diff`,
`FileContent`, `FileListing`, `GrepResult`, `AgentReasoning`, `Divider`,
`StatusPanel`, `SpinnerControl`, `SkillList`, `SkillActivate`, `VersionCheck` and
`UniversalConstructor` are all rendered by `renderer/v2.rs` and emitted by
nothing, so no test can reach them through the real binary.

The cause is that `src/tools/mod.rs` never touches the bus: `read_file`,
`list_files` and `grep` return their results as plain tool text rather than
emitting the structured messages the renderer knows how to draw. `src/shell.rs`
does emit `ShellStart`/`ShellLine`/`ShellOutput`, but nothing calls
`execute_shell_command`, so that path is unreachable too.

This is a wiring gap rather than a rendering bug — the display code exists and
looks reasonable. Connecting the tools to the bus would both improve the output
and make those variants testable.

**Two of the eight modules under `src/tui/` cannot be opened.** `agent_picker`
and `model_picker` are not referenced anywhere outside that directory, so no
command reaches them. The other five — the colours menu, model settings, the
diff menu, the autosave menu and the tutorial — are covered in `ui_screens.rs`.

Those tests assert that the session still works *after* a screen closes, not
just that it drew. A screen that leaves the terminal in raw mode or in the
alternate buffer strands the user, and a test that only checked rendering would
pass straight through that.
