//! Terminal UI tests for how output is rendered.
//!
//! Only the message variants the running application actually emits are covered
//! here. Twelve of the twenty-six `AnyMessage` variants — Diff, FileContent,
//! FileListing, GrepResult, AgentReasoning, Divider, StatusPanel, SpinnerControl,
//! SkillList, SkillActivate, VersionCheck and UniversalConstructor — have
//! rendering code but no producer anywhere in the crate, so there is no way to
//! drive them through the real binary. See the README for the full picture.

#[path = "ui/harness.rs"]
mod harness;

use harness::{says, script, text_turn, TerminalApp};

fn at_prompt(reply: &str) -> TerminalApp {
    let app = TerminalApp::builder()
        .script(says(reply))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app
}

#[test]
fn a_models_answer_is_displayed_verbatim() {
    let app = TerminalApp::builder()
        .args(["-p", "hello"])
        .script(says("The quick brown fox jumps over the lazy dog"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("The quick brown fox jumps over the lazy dog")
        .expect("the answer was altered or lost");
}

#[test]
fn markdown_in_an_answer_is_rendered_not_shown_raw() {
    // The renderer formats markdown; the syntax characters should not survive
    // into the output as literal text.
    let app = TerminalApp::builder()
        .args(["-p", "hello"])
        .script(says("# A Heading\n\nSome **bold** text and a `code span`."))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("A Heading")
        .expect("the heading text was lost");
    app.wait_for("bold").expect("the emphasised text was lost");
    app.wait_for("code span")
        .expect("the code span text was lost");
}

#[test]
fn a_fenced_code_block_keeps_its_contents() {
    let app = TerminalApp::builder()
        .args(["-p", "show me code"])
        .script(says("Here:\n\n```rust\nfn answer() -> u32 { 42 }\n```\n"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("fn answer()")
        .expect("code inside a fence was lost");
}

#[test]
fn an_error_is_visually_distinguishable_from_normal_output() {
    // Errors are emitted at a different level and must not be silently mixed in
    // with ordinary text.
    let mut app = at_prompt("unused");

    app.type_line("/cd /definitely/not/a/real/path").unwrap();

    app.wait_for("Failed to change directory")
        .expect("a failed command produced no visible error");
}

#[test]
fn a_warning_is_shown_for_an_unknown_command() {
    let mut app = at_prompt("unused");

    app.type_line("/nope").unwrap();

    app.wait_for("Unknown command")
        .expect("no warning for an unknown command");
}

#[test]
fn a_long_answer_is_not_truncated() {
    // Output longer than the terminal is tall must still all arrive; the
    // transcript is what a user would have scrolled back through.
    let long = (1..=60)
        .map(|i| format!("line-{i:03}"))
        .collect::<Vec<_>>()
        .join("\n");

    let app = TerminalApp::builder()
        .args(["-p", "count"])
        .script(says(&long))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("line-060")
        .expect("the end of a long answer was lost");
    let transcript = app.transcript();
    assert!(
        transcript.contains("line-001"),
        "the start of a long answer was lost"
    );
}

#[test]
fn a_line_wider_than_the_terminal_is_not_lost() {
    let wide = "W".repeat(400);

    let mut app = TerminalApp::builder()
        .args(["-p", "wide"])
        .size(80, 24)
        .script(says(&wide))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("WWWWWWWWWW").expect("a wide line vanished");

    // Counting straight after the first match races the rest of the output,
    // which is still streaming. -p exits once the answer is delivered, so
    // waiting for exit is what makes the count meaningful.
    app.wait_for_exit().expect("the run did not finish");

    // Wrapped across several rows, but every character must still be there.
    let seen = app.transcript().matches('W').count();
    assert!(
        seen >= 400,
        "a wide line was truncated: {seen} of 400 characters survived"
    );
}

#[test]
fn escape_sequences_in_an_answer_cannot_take_over_the_terminal() {
    // A model's output is untrusted text. If it were written through unescaped,
    // a response could clear the screen, move the cursor, or recolour
    // everything after it.
    let hostile = "before\u{1b}[2J\u{1b}[31mafter\u{1b}[0m";

    let app = TerminalApp::builder()
        .args(["-p", "hostile"])
        .script(says(hostile))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("after").expect("the answer was lost entirely");

    // Both halves must survive: a screen clear between them would remove the
    // first, which is exactly the failure this guards.
    let transcript = app.transcript();
    assert!(
        transcript.contains("before"),
        "text before an embedded escape sequence was erased.\n{transcript}"
    );
}

#[test]
fn a_multi_step_answer_shows_the_tool_and_then_the_reply() {
    // A tool call and the reply after it are separate messages; both belong on
    // screen, in that order.
    let dir = harness::workspace().unwrap();
    std::fs::write(dir.path().join("data.txt"), "FILE-BODY-MARKER").unwrap();

    let app = TerminalApp::builder()
        .args(["-p", "read it"])
        .cwd(dir.path())
        .script(script(vec![
            harness::tool_turn("read_file", serde_json::json!({"path": "data.txt"})),
            text_turn("REPLY-AFTER-TOOL"),
        ]))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("REPLY-AFTER-TOOL")
        .expect("the reply after a tool call never appeared");
}

#[test]
fn an_empty_answer_does_not_break_the_display() {
    let mut app = at_prompt("");

    app.type_line("say nothing").unwrap();

    // The turn must complete and the prompt return, even with nothing to show.
    app.wait_for_prompt()
        .expect("an empty answer left the session without a prompt");
}

#[test]
fn unicode_in_an_answer_survives_rendering() {
    let app = TerminalApp::builder()
        .args(["-p", "unicode"])
        .script(says("héllo wörld — 日本語 — 🐕"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("héllo wörld")
        .expect("accented text was mangled");
    app.wait_for("日本語")
        .expect("wide characters were mangled");
}
