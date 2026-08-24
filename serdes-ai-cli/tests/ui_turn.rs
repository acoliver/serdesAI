//! Terminal UI tests for how a turn presents itself.
//!
//! A turn opens with a rule, shows a waiting indicator, may show what the model
//! was thinking, answers, and closes with what it cost. Each of those was either
//! missing or improvised before, despite the renderer having displays for all of
//! them.

#[path = "ui/harness.rs"]
mod harness;

use harness::{says, script, text_turn, tool_turn, workspace, TerminalApp};

fn at_prompt(reply: &str) -> TerminalApp {
    let app = TerminalApp::builder()
        .script(says(reply))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app
}

#[test]
fn a_turn_reports_what_it_cost() {
    // The closing panel is how a user knows what a request spent without going
    // to a provider dashboard.
    let app = TerminalApp::builder()
        .args(["-p", "hello"])
        .script(says("answered"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("answered").expect("no answer");
    app.wait_for("Turn").expect("the turn reported no summary");
    app.wait_for("request(s) in")
        .expect("the summary did not report how long the turn took");
}

#[test]
fn an_unreported_usage_says_so_rather_than_claiming_zero() {
    // The scripted model reports no usage. Showing "0 tokens" would read as a
    // request that cost nothing, which is a different claim from not knowing.
    let app = TerminalApp::builder()
        .args(["-p", "hello"])
        .script(says("answered"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("not reported")
        .expect("missing usage was presented as zero");
}

#[test]
fn turns_are_separated_by_a_rule() {
    // Without a boundary a long session runs together and it becomes hard to see
    // where one exchange ended.
    let mut app = at_prompt("REPLY-ONE");

    let before = app.transcript().matches('─').count();
    app.type_line("first").unwrap();
    app.wait_for("REPLY-ONE").expect("no reply");

    let after = app.transcript().matches('─').count();
    assert!(
        after > before,
        "no rule was drawn between turns.\n---- screen ----\n{}",
        app.screen_text()
    );
}

#[test]
fn a_failing_turn_reports_the_failure_rather_than_spinning_on() {
    // A turn that errored while the indicator kept going would look like it was
    // still working.
    let app = TerminalApp::builder()
        .args(["-p", "fail please"])
        .script(script(vec![harness::error_turn("the provider refused")]))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("Turn failed")
        .expect("a failed turn produced no failure panel");
    app.wait_for("the provider refused")
        .expect("the failure did not say what went wrong");
}

#[test]
fn a_failed_turn_still_reports_how_long_it_took() {
    let app = TerminalApp::builder()
        .args(["-p", "fail please"])
        .script(script(vec![harness::error_turn("nope")]))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("After")
        .expect("a failed turn reported no duration");
}

#[test]
fn the_summary_counts_the_requests_a_turn_made() {
    // A turn that called a tool makes more than one request, and the count is
    // how a user sees that a single question cost several round trips.
    let dir = workspace().unwrap();
    std::fs::write(dir.path().join("f.txt"), "x").unwrap();

    let app = TerminalApp::builder()
        .args(["-p", "read it"])
        .cwd(dir.path())
        .script(script(vec![
            tool_turn("read_file", serde_json::json!({"path": "f.txt"})),
            text_turn("read"),
        ]))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("read").expect("no answer");
    app.wait_for("2 request(s)")
        .expect("a multi-request turn did not report its request count");
}

#[test]
fn each_turn_gets_its_own_summary() {
    let mut app = at_prompt("ANSWERED");

    // Wait for the summary itself, not for the answer. The answer is emitted
    // first by design, so its arrival says nothing about whether the summary
    // that follows it has been written yet.
    app.type_line("first").unwrap();
    app.wait_for_additional("request(s) in", 0)
        .expect("the first turn produced no summary");

    app.type_line("second").unwrap();
    app.wait_for_additional("request(s) in", 1)
        .expect("the second turn produced no summary of its own");
}

#[test]
fn a_command_does_not_produce_a_turn_summary() {
    // Commands do not call the model, so reporting a cost for one would be
    // misleading.
    let mut app = at_prompt("unused");

    app.type_line("/help").unwrap();
    app.wait_for("/exit").expect("help did not run");

    app.assert_not_contains("request(s) in");
}
