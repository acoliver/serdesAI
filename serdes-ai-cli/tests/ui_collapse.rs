//! Terminal UI tests for shortening long tool output.
//!
//! A command that prints thousands of lines used to put all of them on screen,
//! burying the conversation they belong to. Output is now shown as its first
//! few lines and a count of the rest, with the whole of it a keypress away.

#[path = "ui/harness.rs"]
mod harness;

use std::time::Duration;

use harness::{Key, TerminalApp, script, text_turn, tool_turn, workspace};

const SETTLED: Duration = Duration::from_millis(400);

/// Run a shell command through the agent, then sit at the prompt.
fn after_running(command: &str) -> TerminalApp {
    let dir = workspace().expect("no workspace");

    let mut app = TerminalApp::builder()
        .cwd(dir.path())
        .script(script(vec![
            tool_turn("bash", serde_json::json!({ "command": command })),
            text_turn("done"),
        ]))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("run it").expect("could not send the prompt");
    app.wait_for("done").expect("the turn never finished");
    app.wait_until_idle(SETTLED).expect("never settled");

    // The workspace must outlive the app, which is still running in it.
    std::mem::forget(dir);
    app
}

#[test]
fn short_output_is_shown_in_full() {
    let app = after_running("printf 'alpha\\nbeta\\ngamma\\n'");

    app.wait_for("alpha").expect("the first line is missing");
    app.wait_for("gamma").expect("the last line is missing");
    app.assert_not_contains("more lines");
}

#[test]
fn long_output_is_shortened() {
    let app = after_running("seq 1 200");

    app.wait_for("more lines")
        .expect("two hundred lines were not shortened");
}

#[test]
fn the_shortened_output_keeps_its_first_lines() {
    let app = after_running("seq 1 200");

    app.wait_for("more lines").expect("not shortened");

    // Matched as whole lines: the raw transcript carries carriage returns, and
    // "1" appears inside plenty of other numbers.
    let lines = numbered_lines(&app);
    assert!(lines.contains(&1), "the first line is missing: {lines:?}");
    assert!(lines.contains(&2), "the second line is missing: {lines:?}");
}

#[test]
fn the_bulk_of_a_long_output_stays_off_screen() {
    // The point of shortening: the conversation must not be buried.
    let app = after_running("seq 1 200");

    app.wait_for("more lines").expect("not shortened");

    let lines = numbered_lines(&app);
    assert!(
        !lines.contains(&199),
        "a line from deep in the output reached the screen: {lines:?}"
    );
}

#[test]
fn the_note_says_how_many_lines_were_left_out() {
    let app = after_running("seq 1 200");

    app.wait_for("196 more lines")
        .expect("the note did not count the hidden lines");
}

#[test]
fn ctrl_o_shows_the_whole_output() {
    let mut app = after_running("seq 1 200");

    app.wait_for("more lines").expect("not shortened");
    assert!(!numbered_lines(&app).contains(&199));

    app.send_key(Key::CtrlO).expect("could not press ctrl-o");
    app.wait_until_idle(SETTLED).expect("never settled");

    assert!(
        numbered_lines(&app).contains(&199),
        "Ctrl-O did not show the rest of the output"
    );
}

#[test]
fn ctrl_o_with_nothing_shortened_says_so() {
    let mut app = after_running("printf 'just one line\\n'");

    app.send_key(Key::CtrlO).expect("could not press ctrl-o");

    app.wait_for("Nothing further")
        .expect("Ctrl-O gave no answer when there was nothing to show");
}

#[test]
fn the_session_still_works_after_expanding() {
    let mut app = after_running("seq 1 200");

    app.wait_for("more lines").expect("not shortened");
    app.send_key(Key::CtrlO).expect("could not press ctrl-o");
    app.wait_until_idle(SETTLED).expect("never settled");
    assert!(numbered_lines(&app).contains(&199), "nothing expanded");

    app.type_line("/help")
        .expect("the prompt broke after expanding");
    app.wait_for("/exit")
        .expect("the session stopped responding");
}

#[test]
fn expanding_twice_does_not_repeat_the_same_output() {
    let mut app = after_running("seq 1 200");

    app.wait_for("more lines").expect("not shortened");
    app.send_key(Key::CtrlO).expect("could not press ctrl-o");
    app.wait_until_idle(SETTLED).expect("never settled");
    assert!(numbered_lines(&app).contains(&199), "nothing expanded");

    let before = count_lines_reading(&app, 199);
    app.send_key(Key::CtrlO).expect("could not press ctrl-o");
    app.wait_until_idle(SETTLED).expect("never settled");

    assert_eq!(
        count_lines_reading(&app, 199),
        before,
        "the same output was shown again"
    );
}

#[test]
fn a_one_shot_run_prints_everything() {
    // Piped output has no keyboard to expand it with, and whatever is reading it
    // expects what the command actually printed.
    let dir = workspace().expect("no workspace");

    let app = TerminalApp::builder()
        .cwd(dir.path())
        .args(["-p", "run it"])
        .script(script(vec![
            tool_turn("bash", serde_json::json!({ "command": "seq 1 200" })),
            text_turn("done"),
        ]))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("done").expect("the turn never finished");

    app.assert_contains("199");
    app.assert_not_contains("more lines");
    std::mem::forget(dir);
}

/// The numbers on lines of the transcript that hold nothing else.
fn numbered_lines(app: &TerminalApp) -> Vec<u32> {
    app.transcript()
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

/// How many transcript lines read exactly `value`.
fn count_lines_reading(app: &TerminalApp, value: u32) -> usize {
    numbered_lines(app)
        .into_iter()
        .filter(|n| *n == value)
        .count()
}
