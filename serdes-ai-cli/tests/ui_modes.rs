//! Terminal UI tests for the multi-agent modes.
//!
//! These drive the real binary through Fast and Workflow mode with a scripted
//! model, so they cover the whole path: argument parsing, orchestrator
//! construction, subagent delegation, tool execution, the approval prompt, the
//! quorum gate, and how all of it is rendered.

#[path = "ui/harness.rs"]
mod harness;

use harness::{Key, TerminalApp, workspace};

/// A script driving an orchestrator that delegates once to a code agent.
fn delegating_script(file: &str, contents: &str) -> String {
    serde_json::json!({
        "by_role": {
            "orchestrator": [
                {"tool": {"tool": "spawn_agent", "args": {
                    "role": "code",
                    "task": format!("create {file}")
                }}},
                {"text": {"text": "ORCHESTRATOR-DONE"}}
            ],
            "code": [
                {"tool": {"tool": "write_file", "args": {
                    "path": file,
                    "content": contents
                }}},
                {"text": {"text": "CODE-DONE"}}
            ]
        },
        "turns": [{"text": {"text": "fallback"}}]
    })
    .to_string()
}

#[test]
fn mode_is_listed_in_help() {
    let mut app = TerminalApp::builder()
        .args(["--help"])
        .spawn()
        .expect("failed to spawn");

    app.wait_for("--mode").expect("--mode is not documented");
    assert_eq!(app.wait_for_exit().unwrap(), 0);
}

#[test]
fn an_unknown_mode_is_rejected_with_the_valid_ones() {
    let app = TerminalApp::builder()
        .args(["--mode", "nonsense", "-p", "hello"])
        .spawn()
        .expect("failed to spawn");

    app.wait_for("nonsense")
        .expect("the rejected value was not named");
    // A bare rejection is not actionable; the alternatives must be shown.
    app.wait_for("workflow")
        .expect("the valid modes were not listed");
}

#[test]
fn fast_mode_delegates_and_the_subagent_changes_the_tree() {
    // The whole point of the mode: an orchestrator that delegates, and a
    // subagent whose work lands on disk.
    let dir = workspace().unwrap();

    let app = TerminalApp::builder()
        .args(["--mode", "fast", "-p", "create a widget"])
        .cwd(dir.path())
        .script(delegating_script("widget.rs", "fn widget() {}\n"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("ORCHESTRATOR-DONE")
        .expect("the orchestrator never finished");

    let written = std::fs::read_to_string(dir.path().join("widget.rs"))
        .expect("the subagent did not create the file");
    assert_eq!(written, "fn widget() {}\n");
}

#[test]
fn fast_mode_shows_which_subagent_is_working() {
    // Delegation the user cannot see is indistinguishable from a single agent.
    let dir = workspace().unwrap();

    let app = TerminalApp::builder()
        .args(["--mode", "fast", "-p", "create a widget"])
        .cwd(dir.path())
        .script(delegating_script("widget.rs", "x\n"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("ORCHESTRATOR-DONE")
        .expect("run never finished");
    app.assert_contains("code");
}

#[test]
fn fast_mode_does_not_ask_for_approval() {
    // Fast mode's defining property: no gate and no plan approval.
    let dir = workspace().unwrap();

    let app = TerminalApp::builder()
        .args(["--mode", "fast", "-p", "create a widget"])
        .cwd(dir.path())
        .script(delegating_script("widget.rs", "x\n"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("ORCHESTRATOR-DONE")
        .expect("run never finished");
    app.assert_not_contains("Proceed with this plan?");
}

#[test]
fn workflow_mode_shows_the_plan_and_waits_for_approval() {
    // No work may happen before the user has approved.
    let tmp = workspace().unwrap();
    let dir = tmp.path().to_path_buf();

    let mut app = TerminalApp::builder()
        .args(["--mode", "workflow", "-p", "create a widget"])
        .cwd(&dir)
        .script(
            serde_json::json!({
                "by_role": {
                    "planner": [{"tool": {"tool": "submit_plan", "args": {
                        "summary": "PLAN-SUMMARY-MARKER",
                        "steps": [{"description": "create widget.rs", "files": ["widget.rs"]}],
                        "verification": ["test -f widget.rs"]
                    }}}]
                },
                "turns": [{"text": {"text": "unused"}}]
            })
            .to_string(),
        )
        .spawn()
        .expect("failed to spawn");

    app.wait_for("PLAN-SUMMARY-MARKER")
        .expect("the plan was never shown to the user");
    app.wait_for("Proceed with this plan?")
        .expect("the user was never asked to approve");

    // Nothing should have been written while waiting for an answer.
    assert!(
        !dir.join("widget.rs").exists(),
        "work started before the plan was approved"
    );

    // Decline, so the test does not leave a process waiting.
    app.send_line("3").ok();
}

#[test]
fn declining_a_plan_abandons_the_run() {
    let tmp = workspace().unwrap();
    let dir = tmp.path().to_path_buf();

    let mut app = TerminalApp::builder()
        .args(["--mode", "workflow", "-p", "create a widget"])
        .cwd(&dir)
        .script(
            serde_json::json!({
                "by_role": {
                    "planner": [{"tool": {"tool": "submit_plan", "args": {
                        "summary": "a plan",
                        "steps": [{"description": "create widget.rs", "files": ["widget.rs"]}]
                    }}}],
                    "orchestrator": [{"text": {"text": "SHOULD-NOT-RUN"}}]
                },
                "turns": [{"text": {"text": "unused"}}]
            })
            .to_string(),
        )
        .spawn()
        .expect("failed to spawn");

    app.wait_for("Proceed with this plan?")
        .expect("no approval prompt");

    // Option 3 is "Cancel".
    app.send_line("3").unwrap();

    app.wait_for("cancelled")
        .expect("cancelling was not reported");
    app.assert_not_contains("SHOULD-NOT-RUN");
    assert!(!dir.join("widget.rs").exists());
}

#[test]
fn the_mode_command_reports_the_current_mode() {
    let mut app = TerminalApp::builder()
        .script(harness::says("unused"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("/mode").unwrap();

    app.wait_for("single")
        .expect("/mode did not report the mode");
}

#[test]
fn the_mode_command_switches_modes() {
    let mut app = TerminalApp::builder()
        .script(harness::says("unused"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("/mode fast").unwrap();

    app.wait_for("fast").expect("/mode fast was not accepted");
}

#[test]
fn the_mode_command_rejects_an_unknown_mode() {
    let mut app = TerminalApp::builder()
        .script(harness::says("unused"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("/mode sideways").unwrap();

    app.wait_for("Unknown mode")
        .expect("an unknown mode was accepted");
    // The alternatives must be offered, not just the rejection.
    app.wait_for("workflow")
        .expect("valid modes were not listed");

    app.send_key(Key::CtrlD).ok();
}
