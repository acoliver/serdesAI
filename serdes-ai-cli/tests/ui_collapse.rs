//! Terminal UI tests for shortening long tool output.
//!
//! A command that prints thousands of lines used to put all of them on screen,
//! burying the conversation they belong to. Output is now shown as its first
//! few lines and a count of the rest, and Ctrl-O expands it in place: what
//! follows moves down, and pressing it again brings that back up.

#[path = "ui/harness.rs"]
mod harness;

use std::time::Duration;

use harness::{Key, TerminalApp, script, text_turn, tool_turn, workspace};

const SETTLED: Duration = Duration::from_millis(400);

/// What the model says after the command, so its answer can be looked for.
const ANSWER: &str = "THE-ANSWER";

/// Run a shell command through the agent, then sit at the prompt.
fn after_running(command: &str) -> TerminalApp {
    let dir = workspace().expect("no workspace");

    let mut app = TerminalApp::builder()
        .cwd(dir.path())
        .size(100, 30)
        .script(script(vec![
            tool_turn("bash", serde_json::json!({ "command": command })),
            text_turn(ANSWER),
        ]))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("run it").expect("could not send the prompt");
    app.wait_for(ANSWER).expect("the turn never finished");
    app.wait_until_idle(SETTLED).expect("never settled");

    // The workspace must outlive the app, which is still running in it.
    std::mem::forget(dir);
    app
}

/// The numbers on visible lines that hold nothing else.
fn numbers_on_screen(app: &TerminalApp) -> Vec<u32> {
    app.screen()
        .into_iter()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
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
fn the_note_says_how_many_lines_were_left_out() {
    let app = after_running("seq 1 200");

    app.wait_for("196 more lines")
        .expect("the note did not count the hidden lines");
}

#[test]
fn the_bulk_of_a_long_output_stays_off_screen() {
    let app = after_running("seq 1 200");

    app.wait_for("more lines").expect("not shortened");
    assert!(
        !numbers_on_screen(&app).contains(&150),
        "a line from deep in the output reached the screen"
    );
}

#[test]
fn ctrl_o_expands_the_block() {
    let mut app = after_running("seq 1 40");

    app.wait_for("more lines").expect("not shortened");

    // A line well past the preview: naming one proves the output actually
    // opened, where counting rows could be satisfied by an unrelated redraw.
    assert!(
        !numbers_on_screen(&app).contains(&20),
        "line 20 was visible before expanding"
    );

    app.send_key(Key::CtrlO).expect("could not press ctrl-o");
    app.wait_until_idle(SETTLED).expect("never settled");

    assert!(
        numbers_on_screen(&app).contains(&20),
        "Ctrl-O did not reveal the hidden lines:\n{}",
        app.screen_text()
    );
}

#[test]
fn the_answer_stays_visible_while_expanded() {
    // The point of expanding in place: the block grows, the answer moves down,
    // and both are still on screen.
    let mut app = after_running("seq 1 40");

    app.wait_for("more lines").expect("not shortened");
    app.send_key(Key::CtrlO).expect("could not press ctrl-o");
    app.wait_until_idle(SETTLED).expect("never settled");

    // Both at once: the block really opened, and the answer moved down rather
    // than being overwritten or pushed off.
    let screen = app.screen_text();
    assert!(
        numbers_on_screen(&app).contains(&20),
        "the block did not expand:\n{screen}"
    );
    assert!(
        screen.contains(ANSWER),
        "expanding pushed the answer off the screen:\n{screen}"
    );
}

#[test]
fn ctrl_o_again_collapses_the_block() {
    let mut app = after_running("seq 1 40");

    app.wait_for("more lines").expect("not shortened");

    app.send_key(Key::CtrlO).expect("could not press ctrl-o");
    app.wait_until_idle(SETTLED).expect("never settled");
    let expanded = numbers_on_screen(&app).len();

    app.send_key(Key::CtrlO).expect("could not press ctrl-o");
    app.wait_until_idle(SETTLED).expect("never settled");
    let collapsed = numbers_on_screen(&app).len();

    assert!(
        collapsed < expanded,
        "the block did not collapse again: {collapsed} lines vs {expanded}"
    );
    assert!(
        app.screen_text().contains("more lines - ctrl+o to expand"),
        "the collapsed note did not come back:\n{}",
        app.screen_text()
    );
}

#[test]
fn the_answer_is_still_visible_after_collapsing() {
    let mut app = after_running("seq 1 40");

    app.wait_for("more lines").expect("not shortened");
    app.send_key(Key::CtrlO).expect("could not press ctrl-o");
    app.wait_until_idle(SETTLED).expect("never settled");
    assert!(
        numbers_on_screen(&app).contains(&20),
        "the block did not expand"
    );

    app.send_key(Key::CtrlO).expect("could not press ctrl-o");
    app.wait_until_idle(SETTLED).expect("never settled");

    let screen = app.screen_text();
    assert!(
        !numbers_on_screen(&app).contains(&20),
        "the block did not collapse again:\n{screen}"
    );
    assert!(
        screen.contains(ANSWER),
        "the answer was lost after collapsing:\n{screen}"
    );
}

#[test]
fn expanding_more_than_fits_says_how_much_is_shown() {
    // The region has to fit on screen to be redrawn, so a very long expansion
    // is capped rather than scrolling away where it cannot be collapsed.
    let mut app = after_running("seq 1 500");

    app.wait_for("more lines").expect("not shortened");
    app.send_key(Key::CtrlO).expect("could not press ctrl-o");
    app.wait_until_idle(SETTLED).expect("never settled");

    app.wait_for("ctrl+o to collapse")
        .expect("a capped expansion did not say so");
}

#[test]
fn ctrl_o_with_nothing_shortened_says_so() {
    let mut app = after_running("printf 'just one line\\n'");

    app.send_key(Key::CtrlO).expect("could not press ctrl-o");

    app.wait_for("Nothing to expand")
        .expect("Ctrl-O gave no answer when there was nothing to expand");
}

#[test]
fn the_session_still_works_after_toggling() {
    let mut app = after_running("seq 1 40");

    app.wait_for("more lines").expect("not shortened");
    app.send_key(Key::CtrlO).expect("could not press ctrl-o");
    app.wait_until_idle(SETTLED).expect("never settled");
    app.send_key(Key::CtrlO).expect("could not press ctrl-o");
    app.wait_until_idle(SETTLED).expect("never settled");

    app.type_line("/help")
        .expect("the prompt broke after toggling");
    app.wait_for("/exit")
        .expect("the session stopped responding");
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
            text_turn(ANSWER),
        ]))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(ANSWER).expect("the turn never finished");

    app.assert_contains("199");
    app.assert_not_contains("more lines");
    std::mem::forget(dir);
}
