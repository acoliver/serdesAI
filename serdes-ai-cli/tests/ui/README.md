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

**Failures show the screen.** Every assertion prints the rendered screen on
failure, because "expected X, not found" is useless when the question is what
the interface actually did.

## What this found

Writing the first thirteen tests surfaced a real defect: the startup banner
promises "Ctrl+C to cancel current processing, Ctrl+D to exit cleanly", but the
input loop handled neither. Raw mode delivers them as ordinary key events, so
Ctrl-C fell through to the `Char(c)` arm and typed a literal `c` into the
buffer. Both keys now work, and three tests hold that in place.
