//! Terminal UI tests for showing the answer as it arrives.
//!
//! Two things have to hold. The answer must be *rendered* — a heading shown as
//! a styled line rather than a literal `#` — and it must still be complete,
//! since a streaming renderer that drops the last line or a buffered table
//! would be worse than the version that simply waited.

#[path = "ui/harness.rs"]
mod harness;

use harness::{TerminalApp, says};

/// Run one prompt and return once the turn has finished.
fn answered(reply: &str) -> TerminalApp {
    let app = TerminalApp::builder()
        .args(["-p", "hello"])
        .script(says(reply))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("request(s) in")
        .expect("the turn never finished");
    app
}

#[test]
fn a_heading_is_rendered_rather_than_shown_raw() {
    let app = answered("# A Heading");

    app.wait_for("A Heading")
        .expect("the heading text is missing");
    app.assert_not_contains("# A Heading");
}

#[test]
fn a_bullet_list_is_rendered_as_bullets() {
    let app = answered("- first item\n- second item");

    app.wait_for("first item")
        .expect("the first item is missing");
    app.wait_for("second item")
        .expect("the second item is missing");
    app.assert_not_contains("- first item");
}

#[test]
fn a_table_is_drawn_rather_than_left_as_pipes() {
    // Tables are held back by the parser until complete, so they are the most
    // likely thing to be lost when rendering incrementally.
    let app = answered("| Name | Value |\n|---|---|\n| alpha | one |");

    app.wait_for("alpha").expect("a table cell is missing");
    app.wait_for("one").expect("a table cell is missing");
    app.assert_not_contains("|---|---|");
}

#[test]
fn the_last_line_survives_when_there_is_no_trailing_newline() {
    // Models routinely stop without one, and that last line is usually the
    // answer.
    let app = answered("first line\nthe final word");

    app.wait_for("the final word")
        .expect("the last line was dropped");
}

#[test]
fn a_code_block_keeps_its_contents() {
    let app = answered("```rust\nlet x = 1;\n```");

    app.wait_for("let x = 1;")
        .expect("the code block contents were lost");
}

#[test]
fn plain_text_arrives_unchanged() {
    let app = answered("just a sentence with no markup at all");

    app.wait_for("just a sentence with no markup at all")
        .expect("plain text was altered or lost");
}

#[test]
fn the_answer_still_precedes_its_cost_summary() {
    // The summary describes the answer, so printing it first reads as a report
    // about something the user has not seen yet.
    let app = answered("THE-ANSWER");

    let transcript = app.transcript();
    let answer = transcript
        .find("THE-ANSWER")
        .expect("the answer never appeared");
    let summary = transcript
        .find("request(s) in")
        .expect("the summary never appeared");

    assert!(answer < summary, "the cost summary preceded the answer");
}

#[test]
fn the_answer_is_not_printed_twice() {
    // The streaming path writes the answer itself, so emitting it again at the
    // end of the turn would duplicate it.
    let app = answered("ONLY-ONCE");

    assert_eq!(
        app.transcript().matches("ONLY-ONCE").count(),
        1,
        "the answer appeared more than once"
    );
}

#[test]
fn turning_streaming_off_still_produces_the_answer() {
    // The non-streaming path is still reachable through configuration and must
    // keep working.
    let app = TerminalApp::builder()
        .args(["-p", "hello"])
        .config_line("enable_streaming = false")
        .script(says("NOT-STREAMED"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("NOT-STREAMED")
        .expect("the answer was lost with streaming disabled");
}

#[test]
fn an_empty_answer_does_not_break_the_turn() {
    let app = TerminalApp::builder()
        .args(["-p", "hello"])
        .script(says(""))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("request(s) in")
        .expect("an empty answer left the turn unfinished");
}

#[test]
fn a_streamed_turn_still_reports_what_it_cost() {
    let app = answered("counted");

    app.wait_for("Turn").expect("no summary panel");
    app.wait_for("request(s) in")
        .expect("the summary did not report the duration");
}

#[test]
fn an_interactive_session_streams_each_turn() {
    let mut app = TerminalApp::builder()
        .script(says("# Streamed Heading"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");

    app.type_line("first").unwrap();
    app.wait_for_additional("Streamed Heading", 0)
        .expect("the first turn did not render");

    app.type_line("second").unwrap();
    app.wait_for_additional("Streamed Heading", 1)
        .expect("the second turn did not render");

    app.assert_not_contains("# Streamed Heading");
}
