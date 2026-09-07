//! Terminal UI tests for the input line.
//!
//! The input loop runs in raw mode and handles every key itself — insertion,
//! deletion, cursor movement, the completion dropdown — so nothing about it is
//! exercised by testing the agent. These drive the real keys.

#[path = "ui/harness.rs"]
mod harness;

use harness::{Key, TerminalApp, says};

/// An app sitting at the interactive prompt, ready for input.
fn at_prompt(reply: &str) -> TerminalApp {
    let app = TerminalApp::builder()
        .script(says(reply))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app
}

#[test]
fn typed_text_is_echoed_as_it_is_entered() {
    let mut app = at_prompt("reply");

    app.send("hello world").unwrap();

    app.wait_for("hello world")
        .expect("typed text was not echoed");
}

#[test]
fn enter_submits_the_line() {
    let mut app = at_prompt("SUBMITTED-OK");

    app.type_line("do the thing").unwrap();

    app.wait_for("SUBMITTED-OK")
        .expect("the line was never submitted");
}

#[test]
fn backspace_deletes_the_previous_character() {
    let mut app = at_prompt("reply");

    app.send("helloX").unwrap();
    app.wait_for("helloX").expect("text not echoed");
    app.send_key(Key::Backspace).unwrap();
    app.send("!").unwrap();

    app.wait_for("hello!")
        .expect("backspace did not remove the character");
}

#[test]
fn an_empty_line_is_not_sent_to_the_model() {
    let mut app = at_prompt("MODEL-WAS-CALLED");

    app.send_key(Key::Enter).unwrap();
    app.send_key(Key::Enter).unwrap();

    // Give the app a moment to have done the wrong thing.
    app.type_line("real input").unwrap();
    app.wait_for("MODEL-WAS-CALLED").expect("no reply");

    // Exactly one reply: the blank lines must not have produced their own.
    let transcript = app.transcript();
    assert_eq!(
        transcript.matches("MODEL-WAS-CALLED").count(),
        1,
        "a blank line was sent to the model.\n{transcript}"
    );
}

#[test]
fn a_slash_opens_the_command_dropdown() {
    let mut app = at_prompt("reply");

    app.send("/").unwrap();

    // The dropdown lists commands; /help is always among them.
    app.wait_for("/help")
        .expect("typing / did not offer completions");
}

#[test]
fn tab_completes_the_selected_command() {
    let mut app = at_prompt("reply");

    app.send("/he").unwrap();
    app.wait_for("/he").expect("text not echoed");
    app.send_key(Key::Tab).unwrap();

    app.wait_for("/help")
        .expect("tab did not complete the command");
}

#[test]
fn escape_dismisses_the_dropdown_without_clearing_the_line() {
    let mut app = at_prompt("reply");

    app.send("/he").unwrap();
    app.wait_for("/he").expect("text not echoed");
    app.send_key(Key::Esc).unwrap();

    // The typed text survives; only the dropdown goes away.
    app.assert_contains("/he");
}

#[test]
fn the_cursor_can_move_left_and_insert_mid_line() {
    let mut app = at_prompt("reply");

    app.send("helo").unwrap();
    app.wait_for("helo").expect("text not echoed");

    // Move back over the final 'o' and insert the missing 'l'.
    app.send_key(Key::Left).unwrap();
    app.send("l").unwrap();

    app.wait_for("hello")
        .expect("mid-line insertion did not work");
}

#[test]
fn multibyte_input_does_not_crash_the_process() {
    // cursor_pos advances one per character but indexes a String by bytes, so a
    // multi-byte character followed by any further edit lands inside it. String
    // insert/remove panic on a non-boundary index, which kills the process.
    let mut app = at_prompt("SURVIVED");

    app.send("é").unwrap();
    app.send("a").unwrap();
    app.send("b").unwrap();

    // Checking has_exited() here would race a panic still propagating. Requiring
    // real work afterwards proves the loop is both alive and still functioning.
    app.wait_for("éab")
        .expect("multi-byte input was not echoed correctly");
    app.send_key(Key::Enter).unwrap();
    app.wait_for("SURVIVED")
        .expect("the input loop died on multi-byte input");
}

#[test]
fn a_multibyte_line_round_trips() {
    let mut app = at_prompt("UNICODE-OK");

    app.send("héllo wörld").unwrap();
    app.wait_for("héllo wörld").expect("text not echoed");
    app.send_key(Key::Enter).unwrap();

    app.wait_for("UNICODE-OK")
        .expect("a line with accents was not submitted");
}

#[test]
fn backspace_over_a_multibyte_character_does_not_crash() {
    let mut app = at_prompt("BACKSPACE-OK");

    app.send("aé").unwrap();
    app.wait_for("aé").expect("text not echoed");
    app.send_key(Key::Backspace).unwrap();
    app.send_key(Key::Backspace).unwrap();
    app.type_line("ok").unwrap();

    app.wait_for("BACKSPACE-OK")
        .expect("backspacing over a multi-byte character broke the input loop");
}

#[test]
fn an_emoji_does_not_crash_the_input_loop() {
    // Four bytes rather than two, and outside the BMP.
    let mut app = at_prompt("EMOJI-OK");

    app.send("🐕").unwrap();
    app.send("x").unwrap();
    app.send_key(Key::Enter).unwrap();

    app.wait_for("EMOJI-OK")
        .expect("an emoji broke the input loop");
}

#[test]
fn backspace_on_an_empty_line_is_harmless() {
    let mut app = at_prompt("STILL-WORKS");

    for _ in 0..5 {
        app.send_key(Key::Backspace).unwrap();
    }
    app.type_line("after").unwrap();

    app.wait_for("STILL-WORKS")
        .expect("the input loop broke after backspacing an empty line");
}

#[test]
fn cursor_movement_past_the_ends_is_harmless() {
    let mut app = at_prompt("BOUNDS-OK");

    for _ in 0..5 {
        app.send_key(Key::Left).unwrap();
    }
    for _ in 0..10 {
        app.send_key(Key::Right).unwrap();
    }
    app.type_line("bounded").unwrap();

    app.wait_for("BOUNDS-OK")
        .expect("moving the cursor out of range broke the input loop");
}
