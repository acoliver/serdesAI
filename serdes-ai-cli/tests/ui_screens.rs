//! Terminal UI tests for the full-screen interfaces.
//!
//! These take over the terminal and wait for keys, so they need a different
//! shape from the other suites: open the screen, confirm it drew, dismiss it,
//! and confirm the session is usable afterwards. A screen that leaves the
//! terminal in raw mode or in the alternate buffer would strand the user, so
//! "the prompt still works" is the assertion that matters most.
//!
//! Two of the eight modules under `src/tui/` — `agent_picker` and
//! `model_picker` — are not referenced outside that directory and cannot be
//! opened, so they are not covered here.

#[path = "ui/harness.rs"]
mod harness;

use harness::{says, Key, TerminalApp};

/// An app sitting at the interactive prompt.
fn at_prompt() -> TerminalApp {
    let app = TerminalApp::builder()
        .script(says("STILL-ALIVE"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app
}

/// Open `command`, wait for `marker` to appear, then leave with Escape.
///
/// Returns the app so a test can check the session afterwards.
fn open_and_dismiss(command: &str, marker: &str) -> TerminalApp {
    let mut app = at_prompt();

    app.type_line(command)
        .unwrap_or_else(|e| panic!("could not send {command}: {e}"));

    app.wait_for(marker)
        .unwrap_or_else(|e| panic!("{command} did not draw its screen: {e}"));

    app.send_key(Key::Esc)
        .unwrap_or_else(|e| panic!("could not dismiss {command}: {e}"));

    app
}

#[test]
fn the_colours_screen_opens() {
    let mut app = open_and_dismiss("/colors", "olor");

    assert!(!app.has_exited(), "the colours screen ended the session");
}

#[test]
fn the_colours_screen_returns_to_a_usable_prompt() {
    // The failure that matters: a screen that restores the terminal badly leaves
    // the user unable to type.
    let mut app = open_and_dismiss("/colors", "olor");

    app.type_line("still working?")
        .expect("the prompt stopped accepting input after the colours screen");
    app.wait_for("STILL-ALIVE")
        .expect("the session stopped responding after the colours screen");
}

#[test]
fn the_model_settings_screen_opens_and_returns() {
    let mut app = open_and_dismiss("/model_settings", "etting");

    app.type_line("still working?")
        .expect("the prompt stopped accepting input after model settings");
    app.wait_for("STILL-ALIVE")
        .expect("the session stopped responding after model settings");
}

#[test]
fn the_diff_screen_opens_and_returns() {
    let mut app = open_and_dismiss("/diff", "iff");

    app.type_line("still working?")
        .expect("the prompt stopped accepting input after the diff screen");
    app.wait_for("STILL-ALIVE")
        .expect("the session stopped responding after the diff screen");
}

#[test]
fn the_autosave_menu_opens_and_returns() {
    let mut app = open_and_dismiss("/autosave_load", "ession");

    app.type_line("still working?")
        .expect("the prompt stopped accepting input after the autosave menu");
    app.wait_for("STILL-ALIVE")
        .expect("the session stopped responding after the autosave menu");
}

#[test]
fn the_tutorial_opens_and_returns() {
    let mut app = open_and_dismiss("/tutorial", "utorial");

    app.type_line("still working?")
        .expect("the prompt stopped accepting input after the tutorial");
    app.wait_for("STILL-ALIVE")
        .expect("the session stopped responding after the tutorial");
}

#[test]
fn a_screen_can_be_opened_twice() {
    // State left behind by the first visit — raw mode, the alternate buffer, a
    // cursor position — would show up on the second.
    let mut app = at_prompt();

    for visit in 0..2 {
        app.type_line("/colors").expect("could not open the screen");

        // Judged from the screen, not the transcript. The transcript keeps every
        // redraw, so a count-based wait is satisfied by output from the previous
        // visit and Escape then dismisses a screen that has not drawn — leaving
        // the next command to be typed into a screen that is still open.
        app.wait_for_on_screen("olor")
            .unwrap_or_else(|e| panic!("the screen did not draw on visit {visit}: {e}"));

        app.send_key(Key::Esc)
            .expect("could not dismiss the screen");

        // Settling is the signal that it closed. "olor" cannot be waited on: the
        // typed "/colors" stays in the scrollback, so it never disappears.
        app.wait_until_idle(std::time::Duration::from_millis(300))
            .unwrap_or_else(|e| panic!("the screen did not close on visit {visit}: {e}"));
    }

    app.type_line("still working?")
        .expect("the prompt broke after opening a screen twice");
    app.wait_for("STILL-ALIVE")
        .expect("the session stopped responding");
}

#[test]
fn arrow_keys_in_a_screen_do_not_break_it() {
    let mut app = at_prompt();

    app.type_line("/colors").expect("could not open the screen");
    app.wait_for("olor").expect("the screen did not draw");

    for key in [Key::Down, Key::Down, Key::Up, Key::Right, Key::Left] {
        app.send_key(key).expect("could not send a key");
    }
    app.send_key(Key::Esc)
        .expect("could not dismiss the screen");

    app.type_line("still working?")
        .expect("navigating the screen broke the prompt");
    app.wait_for("STILL-ALIVE")
        .expect("the session stopped responding");
}

#[test]
fn the_first_run_tutorial_does_not_block_a_fresh_install() {
    // A brand new profile triggers onboarding. It must not leave the user stuck
    // at a screen they cannot get past.
    let mut app = TerminalApp::builder()
        .fresh_install()
        .script(says("FRESH-OK"))
        .spawn()
        .expect("failed to spawn");

    // Whatever onboarding shows, Escape must get out of it and reach a prompt.
    app.send_key(Key::Esc).ok();
    app.wait_for(">>>")
        .expect("a fresh install never reached a prompt");

    app.type_line("hello")
        .expect("a fresh install could not accept input");
    app.wait_for("FRESH-OK")
        .expect("a fresh install could not complete a turn");
}
