//! Terminal UI tests for the input region at the bottom of the screen.
//!
//! The interesting case is a full screen: the prompt sits on the last row, and
//! the completion list needs rows that are not there. The region has to make
//! room rather than draw past the edge, and output arriving while the user
//! types must go above the region instead of over it.

#[path = "ui/harness.rs"]
mod harness;

use std::time::Duration;

use harness::{TerminalApp, says};

/// The pause after which the application is taken to have finished drawing.
const SETTLED: Duration = Duration::from_millis(300);

/// The description of the first entry, which is always visible when the list
/// opens. The list comes from the command registry and is alphabetical, so
/// naming a command further down would depend on how many commands exist.
const FIRST_ENTRY: &str = "Show or set active agent";

/// A short terminal with one turn behind it, so the prompt is at the bottom.
fn after_a_turn(reply: &str) -> TerminalApp {
    let mut app = TerminalApp::builder()
        .size(80, 12)
        .script(says(reply))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt");
    app.type_line("go").unwrap();
    app.wait_for("request(s) in")
        .expect("the turn did not finish");

    // Between turns the application is briefly not reading, and a byte sent
    // into that gap is discarded when raw mode is re-enabled.
    app.wait_until_idle(SETTLED).expect("output never settled");
    app
}

#[test]
fn the_completion_list_is_visible_with_the_prompt_at_the_bottom() {
    let mut app = after_a_turn("line one\nline two\nline three\nline four\nline five");

    app.send("/").expect("could not type");
    app.wait_for(FIRST_ENTRY)
        .expect("the completion list was drawn off the bottom of the screen");
}

#[test]
fn the_whole_list_fits_on_screen() {
    // Making room means scrolling the conversation up, not clipping the list.
    let mut app = after_a_turn("line one\nline two\nline three\nline four\nline five");

    app.send("/").expect("could not type");
    app.wait_for(FIRST_ENTRY).expect("no list");
    app.wait_until_idle(SETTLED).expect("never settled");

    let screen = app.screen_text();
    let shown = screen.matches(" - ").count();
    assert!(shown >= 4, "only {shown} entries fit on screen:\n{screen}");
}

#[test]
fn the_line_being_typed_stays_visible() {
    let mut app = after_a_turn("some output");

    app.send("/mod").expect("could not type");
    app.wait_until_idle(SETTLED).expect("never settled");

    let screen = app.screen_text();
    assert!(
        screen.contains(">>> /mod"),
        "the line being typed is not on screen:\n{screen}"
    );
}

#[test]
fn output_arriving_while_typing_goes_above_the_prompt() {
    // The defect this guards: a message printed while the user was typing
    // landed on the input line, because nothing owned the screen.
    let mut app = after_a_turn("some output");

    app.send("/").expect("could not type");
    app.wait_for(FIRST_ENTRY).expect("no list");
    app.wait_until_idle(SETTLED).expect("never settled");

    let rows: Vec<String> = app
        .screen()
        .into_iter()
        .filter(|row| !row.trim().is_empty())
        .collect();
    let prompt_row = rows
        .iter()
        .rposition(|row| row.contains(">>>"))
        .expect("the prompt is not on screen");

    // Everything above the prompt is conversation; nothing below it but the list.
    assert!(
        rows[..prompt_row].iter().all(|row| !row.contains(" - /")),
        "a completion entry was drawn above the prompt:\n{}",
        app.screen_text()
    );
}

#[test]
fn the_session_still_works_after_the_list_has_opened_and_closed() {
    let mut app = after_a_turn("REPLY-AGAIN");

    app.send("/").expect("could not type");
    app.wait_for(FIRST_ENTRY).expect("no list");
    app.send_key(harness::Key::Backspace)
        .expect("could not clear");
    app.wait_until_gone(FIRST_ENTRY).expect("the list stayed");

    app.type_line("again").expect("could not type a new line");
    app.wait_for_additional("REPLY-AGAIN", 1)
        .expect("the session stopped responding");
}
