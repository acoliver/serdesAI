//! Terminal UI tests for recalling previous input with the arrow keys.
//!
//! The lines entered are already written to a history file, and `/history`
//! prints them, but the input line never read it — so Up and Down did nothing
//! unless the completion list happened to be open.

#[path = "ui/harness.rs"]
mod harness;

use std::time::Duration;

use harness::{Key, TerminalApp, says};

/// The pause after which the application is taken to have finished drawing.
const SETTLED: Duration = Duration::from_millis(300);

/// The line currently being edited.
///
/// The last such row, not the first: earlier prompts stay on screen as
/// scrollback, and matching one of those would report a line that was entered
/// several turns ago.
fn input_line(app: &TerminalApp) -> String {
    app.screen()
        .into_iter()
        .rfind(|row| row.contains(">>>"))
        .unwrap_or_default()
}

/// An app that has already had `lines` entered.
fn after_entering(lines: &[&str]) -> TerminalApp {
    let mut app = TerminalApp::builder()
        .script(says("answered"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");

    for line in lines {
        app.type_line(line)
            .unwrap_or_else(|e| panic!("could not enter {line}: {e}"));
        app.wait_for_additional("answered", 0)
            .unwrap_or_else(|e| panic!("no reply to {line}: {e}"));
    }

    app.wait_until_idle(SETTLED).expect("never settled");
    app
}

#[test]
fn up_recalls_the_previous_line() {
    let mut app = after_entering(&["remember this"]);

    app.send_key(Key::Up).expect("could not press up");
    app.wait_until_idle(SETTLED).expect("never settled");

    assert!(
        input_line(&app).contains("remember this"),
        "Up did not recall the previous line: {}",
        input_line(&app)
    );
}

#[test]
fn up_twice_reaches_the_line_before_that() {
    let mut app = after_entering(&["oldest one", "newest one"]);

    app.send_key(Key::Up).expect("could not press up");
    app.wait_until_idle(SETTLED).expect("never settled");
    assert!(input_line(&app).contains("newest one"));

    app.send_key(Key::Up).expect("could not press up");
    app.wait_until_idle(SETTLED).expect("never settled");
    assert!(
        input_line(&app).contains("oldest one"),
        "the second Up did not go further back: {}",
        input_line(&app)
    );
}

#[test]
fn down_comes_back_towards_the_newest() {
    let mut app = after_entering(&["oldest one", "newest one"]);

    app.send_key(Key::Up).expect("could not press up");
    app.send_key(Key::Up).expect("could not press up");
    app.wait_until_idle(SETTLED).expect("never settled");
    assert!(input_line(&app).contains("oldest one"));

    app.send_key(Key::Down).expect("could not press down");
    app.wait_until_idle(SETTLED).expect("never settled");

    assert!(
        input_line(&app).contains("newest one"),
        "Down did not come back: {}",
        input_line(&app)
    );
}

#[test]
fn down_past_the_newest_restores_what_was_being_typed() {
    // Browsing history must not destroy a line that was half written.
    let mut app = after_entering(&["something earlier"]);

    app.send("a half written line").expect("could not type");
    app.wait_until_idle(SETTLED).expect("never settled");

    app.send_key(Key::Up).expect("could not press up");
    app.wait_until_idle(SETTLED).expect("never settled");
    assert!(input_line(&app).contains("something earlier"));

    app.send_key(Key::Down).expect("could not press down");
    app.wait_until_idle(SETTLED).expect("never settled");

    assert!(
        input_line(&app).contains("a half written line"),
        "the half written line was lost: {}",
        input_line(&app)
    );
}

#[test]
fn a_recalled_line_can_be_run_again() {
    let mut app = after_entering(&["run me twice"]);

    app.send_key(Key::Up).expect("could not press up");
    app.wait_until_idle(SETTLED).expect("never settled");
    app.send_key(Key::Enter).expect("could not submit");

    app.wait_for_additional("answered", 1)
        .expect("the recalled line did not run");
}

#[test]
fn the_arrows_still_move_the_completion_selection() {
    // With the list open the arrows belong to it, not to history.
    let mut app = after_entering(&["something earlier"]);

    app.send("/").expect("could not type");
    app.wait_for("Show or set active agent")
        .expect("no completion list");
    app.wait_until_idle(SETTLED).expect("never settled");

    app.send_key(Key::Down).expect("could not press down");
    app.wait_until_idle(SETTLED).expect("never settled");

    assert!(
        !input_line(&app).contains("something earlier"),
        "Down recalled history while the completion list was open: {}",
        input_line(&app)
    );
}

#[test]
fn up_on_an_empty_history_does_nothing() {
    let mut app = TerminalApp::builder()
        .script(says("answered"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.send_key(Key::Up).expect("could not press up");
    app.wait_until_idle(SETTLED).expect("never settled");

    // Still usable, and nothing invented on the line.
    app.type_line("still works").expect("the prompt broke");
    app.wait_for("answered")
        .expect("the session stopped responding");
}

#[test]
fn history_survives_into_the_next_session() {
    // The file is shared, so a line entered before is recallable later.
    let mut app = TerminalApp::builder()
        .config_line("onboarding_complete = true")
        .script(says("answered"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("from an earlier session").unwrap();
    app.wait_for("answered").expect("no reply");
    app.type_line("/exit").unwrap();
    app.wait_for_exit().expect("did not exit");

    // Found rather than named: the history file lives in the state directory,
    // whose location depends on the platform.
    let found = find_history(app.home());

    assert!(
        found
            .iter()
            .any(|text| text.contains("from an earlier session")),
        "the line was not written to any history file under {}",
        app.home().display()
    );
}

/// The contents of every command history file under `home`.
fn find_history(home: &std::path::Path) -> Vec<String> {
    fn walk(dir: &std::path::Path, found: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.file_name().and_then(|n| n.to_str()) == Some("command_history.txt") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    found.push(text);
                }
            }
        }
    }

    let mut found = Vec::new();
    walk(home, &mut found);
    found
}
