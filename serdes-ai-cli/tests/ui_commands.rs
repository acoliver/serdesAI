//! Terminal UI tests for the slash commands.
//!
//! The CLI registers 36 commands. Most had no coverage at all, so the first
//! priority is breadth: every command must respond without panicking, hanging,
//! or leaving the session unusable. Individual behaviour is then checked for the
//! ones whose output a user relies on.

#[path = "ui/harness.rs"]
mod harness;

use harness::{TerminalApp, says, workspace};

/// An app sitting at the interactive prompt.
fn at_prompt(reply: &str) -> TerminalApp {
    let app = TerminalApp::builder()
        .script(says(reply))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app
}

/// Commands that print and return, taking no further input.
///
/// Commands that open a picker or wizard are covered separately: they hold the
/// terminal, so sweeping them the same way would just hang.
const NON_INTERACTIVE: &[&str] = &[
    "/help",
    "/commands",
    "/show",
    "/tools",
    "/session",
    "/history",
    "/motd",
    "/cd",
    "/clear",
    "/compact",
    "/truncate",
    "/verbosity",
    "/reasoning",
    "/api",
    "/mcp",
    "/uc",
    "/autosave",
    "/dump_context",
    "/wiggum_stop",
    "/unpin",
    "/diff",
];

/// Commands that take over the terminal and wait for a choice.
///
/// They were in the sweep above until the test harness began answering
/// cursor-position queries, at which point they started opening properly and
/// swallowing the sweep's follow-up typing as filter text.
const PICKERS: &[(&str, &str)] = &[("/model", "Select a model"), ("/agent", "Select an agent")];

#[test]
fn a_picker_opens_and_can_be_dismissed() {
    for (command, heading) in PICKERS {
        let mut app = at_prompt("PICKER-ALIVE");

        app.type_line(command)
            .unwrap_or_else(|e| panic!("{command} was never echoed: {e}"));
        app.wait_for(heading)
            .unwrap_or_else(|e| panic!("{command} did not open its picker: {e}"));

        app.send_key(harness::Key::Esc)
            .unwrap_or_else(|e| panic!("could not dismiss {command}: {e}"));

        app.type_line("still there?")
            .unwrap_or_else(|e| panic!("{command} left the prompt unusable: {e}"));
        app.wait_for("PICKER-ALIVE")
            .unwrap_or_else(|e| panic!("{command} left the session unresponsive: {e}"));
    }
}

#[test]
fn every_non_interactive_command_leaves_the_session_usable() {
    // A fresh process per command. Sharing one session would be faster, but a
    // single command that wedges the input loop would then be reported as
    // "everything after it failed" rather than naming the one at fault.
    let mut broken = Vec::new();

    for command in NON_INTERACTIVE {
        let mut app = at_prompt("SWEEP-ALIVE");

        if app.type_line(command).is_err() {
            broken.push(format!("{command}: input was never echoed"));
            continue;
        }

        // No Esc here. A lone escape byte is ambiguous — a terminal has to wait
        // to see whether it begins a sequence — so sending one can swallow the
        // characters typed straight after it, which made this sweep report a
        // different set of "broken" commands on every run. Each command now runs
        // in its own process, so nothing needs dismissing anyway: a command that
        // really does open a picker will fail here and be named.
        if app.type_line("still there?").is_err() {
            broken.push(format!("{command}: the prompt stopped accepting input"));
            continue;
        }

        if app.wait_for("SWEEP-ALIVE").is_err() {
            broken.push(format!(
                "{command}: the session stopped responding\n    screen: {}",
                app.screen_text().replace('\n', " | ")
            ));
        }
    }

    assert!(
        broken.is_empty(),
        "{} of {} commands left the session unusable:\n  {}",
        broken.len(),
        NON_INTERACTIVE.len(),
        broken.join("\n  ")
    );
}

#[test]
fn an_unknown_command_is_reported_and_not_sent_to_the_model() {
    let mut app = at_prompt("MODEL-SHOULD-NOT-SEE-THIS");

    app.type_line("/definitely-not-a-command").unwrap();

    app.wait_for("Unknown command")
        .expect("an unknown command was not reported");
    app.assert_not_contains("MODEL-SHOULD-NOT-SEE-THIS");
}

#[test]
fn help_lists_the_core_commands() {
    let mut app = at_prompt("unused");

    app.type_line("/help").unwrap();

    for expected in ["/exit", "/help"] {
        app.wait_for(expected)
            .unwrap_or_else(|e| panic!("help did not mention {expected}: {e}"));
    }
}

#[test]
fn cd_reports_the_current_directory() {
    let dir = workspace().unwrap();
    std::fs::write(dir.path().join("marker-file.txt"), "x").unwrap();

    let mut app = TerminalApp::builder()
        .script(says("unused"))
        .cwd(dir.path())
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("/cd").unwrap();

    app.wait_for("marker-file.txt")
        .expect("/cd did not list the current directory");
}

#[test]
fn cd_changes_directory() {
    let dir = workspace().unwrap();
    std::fs::create_dir(dir.path().join("subdir")).unwrap();
    std::fs::write(dir.path().join("subdir/inner.txt"), "x").unwrap();

    let mut app = TerminalApp::builder()
        .script(says("unused"))
        .cwd(dir.path())
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("/cd subdir").unwrap();
    app.type_line("/cd").unwrap();

    app.wait_for("inner.txt")
        .expect("/cd did not change directory");
}

#[test]
fn cd_into_a_missing_directory_is_reported() {
    let mut app = at_prompt("unused");

    app.type_line("/cd /no/such/directory/anywhere").unwrap();

    // The failure must be visible; silently staying put would be worse.
    app.wait_for("no/such/directory")
        .expect("a failed /cd was not reported");
    assert!(!app.has_exited(), "a bad path ended the session");
}

#[test]
fn model_reports_the_active_model() {
    let mut app = at_prompt("unused");

    let before = app.transcript().matches("odel").count();
    app.type_line("/model").unwrap();

    // "odel" also appears in the echo of "/model" itself, so requiring a new
    // occurrence beyond that is what actually proves the command printed
    // something.
    app.wait_for_additional("odel", before + 1)
        .expect("/model printed nothing beyond echoing the command");
}

#[test]
fn tools_lists_the_available_tools() {
    let mut app = at_prompt("unused");

    app.type_line("/tools").unwrap();

    app.wait_for("read_file")
        .expect("/tools did not list the built-in tools");
}

#[test]
fn clear_resets_the_conversation_and_says_so() {
    let mut app = at_prompt("unused");

    app.type_line("/clear").unwrap();

    app.wait_for("cleared")
        .expect("/clear did not confirm what it did");
}

#[test]
fn a_command_with_unexpected_arguments_does_not_crash() {
    // Handlers parse their own arguments; a stray one must not panic.
    let mut app = at_prompt("ARGS-OK");

    for command in [
        "/help extra",
        "/show nonsense",
        "/tools --weird",
        "/clear 1 2 3",
    ] {
        app.type_line(command).unwrap();
        assert!(
            !app.has_exited(),
            "{command} ended the session.\n---- screen ----\n{}",
            app.screen_text()
        );
    }

    app.type_line("after").unwrap();
    app.wait_for("ARGS-OK")
        .expect("the session broke on unexpected arguments");
}

#[test]
fn commands_are_case_insensitive_where_documented() {
    // The runner lowercases exit/quit forms, so /EXIT must work.
    let mut app = at_prompt("unused");

    app.type_line("/EXIT").unwrap();

    let code = app.wait_for_exit().expect("/EXIT did not exit");
    assert_eq!(code, 0);
}

#[test]
fn a_turn_after_clear_still_reaches_the_model() {
    // /clear resets history and rotates the autosave session. The next turn must
    // still work; a command that leaves the session unable to answer is worse
    // than one that fails outright, because nothing says so.
    let mut app = at_prompt("AFTER-CLEAR-OK");

    app.type_line("/clear").unwrap();
    app.type_line("are you still there?").unwrap();

    app.wait_for("AFTER-CLEAR-OK").unwrap_or_else(|e| {
        panic!(
            "a turn after /clear never reached the model: {e}\n\
             ---- full transcript ----\n{}\n-------------------------",
            app.transcript()
        )
    });
}

#[test]
fn a_turn_after_session_still_reaches_the_model() {
    let mut app = at_prompt("AFTER-SESSION-OK");

    app.type_line("/session").unwrap();
    app.type_line("are you still there?").unwrap();

    app.wait_for("AFTER-SESSION-OK").unwrap_or_else(|e| {
        panic!(
            "a turn after /session never reached the model: {e}\n\
             ---- full transcript ----\n{}\n-------------------------",
            app.transcript()
        )
    });
}
