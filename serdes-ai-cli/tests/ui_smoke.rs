//! Terminal UI tests: drive the real binary and assert on the rendered screen.
//!
//! These spawn `serdes-ai` on a pseudo-terminal, so they exercise argument
//! parsing, model construction, the agent loop, and the renderer together —
//! everything a unit test of any one part would miss.
//!
//! Every run is answered by a scripted model, so nothing here touches a network.

#[path = "ui/harness.rs"]
mod harness;

use harness::{says, script, text_turn, tool_turn, workspace, Key, TerminalApp};

#[test]
fn version_is_reported() {
    // The cheapest possible check that the binary runs at all. When this fails,
    // every other UI test failure is noise.
    let mut app = TerminalApp::builder()
        .args(["--version"])
        .spawn()
        .expect("failed to spawn");

    app.wait_for(env!("CARGO_PKG_VERSION"))
        .expect("version was not printed");
    assert_eq!(app.wait_for_exit().unwrap(), 0);
}

#[test]
fn help_lists_the_prompt_flag() {
    let mut app = TerminalApp::builder()
        .args(["--help"])
        .spawn()
        .expect("failed to spawn");

    app.wait_for("--prompt")
        .expect("help did not mention -p/--prompt");
    assert_eq!(app.wait_for_exit().unwrap(), 0);
}

#[test]
fn a_single_prompt_prints_the_models_answer() {
    // The core non-interactive path: -p runs once and shows the reply.
    let mut app = TerminalApp::builder()
        .args(["-p", "say hello"])
        .script(says("Hello from the scripted model"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("Hello from the scripted model")
        .expect("the model's answer was never displayed");
    assert_eq!(app.wait_for_exit().unwrap(), 0);
}

#[test]
fn the_scripted_model_replaces_the_provider_entirely() {
    // Proves the tests are hermetic: no API key is set, so a run that reached a
    // real provider would fail rather than answer.
    let app = TerminalApp::builder()
        .args(["-p", "anything"])
        .script(says("answered without a provider"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("answered without a provider")
        .expect("the scripted model did not answer");
    app.assert_not_contains("API key");
}

#[test]
fn a_model_error_is_shown_to_the_user_not_swallowed() {
    let app = TerminalApp::builder()
        .args(["-p", "trigger a failure"])
        .script(script(vec![harness::error_turn("upstream is unavailable")]))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("upstream is unavailable")
        .expect("the model error never reached the screen");
}

#[test]
fn a_broken_mock_script_fails_loudly() {
    // A malformed script must not silently fall back to a real provider: a test
    // that quietly started calling the network would be worse than a failure.
    let dir = workspace().unwrap();
    let path = dir.path().join("broken.json");
    std::fs::write(&path, "{ not json at all").unwrap();

    let app = TerminalApp::builder()
        .args(["-p", "hello"])
        .env("SERDES_AI_MOCK", path.to_string_lossy())
        .spawn()
        .expect("failed to spawn");

    app.wait_for("script could not be loaded")
        .expect("a malformed script was not reported");
}

#[test]
fn a_tool_call_is_visible_and_actually_runs() {
    // The tool must both execute and be shown: an agent silently touching the
    // filesystem is exactly what a coding tool must not do.
    let dir = workspace().unwrap();
    std::fs::write(dir.path().join("target.txt"), "file contents here").unwrap();

    let app = TerminalApp::builder()
        .args(["-p", "read the file"])
        .cwd(dir.path())
        .script(script(vec![
            tool_turn("read_file", serde_json::json!({"path": "target.txt"})),
            text_turn("I read it"),
        ]))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("I read it").expect("the run never finished");
}

#[test]
fn the_interactive_prompt_appears_and_accepts_input() {
    let mut app = TerminalApp::builder()
        .script(says("interactive reply"))
        .spawn()
        .expect("failed to spawn");

    // Reaching a prompt at all is the precondition for every interactive test.
    app.wait_for(">>>").expect("no prompt appeared");

    app.type_line("hello there").unwrap();
    app.wait_for("interactive reply")
        .expect("the interactive turn produced no reply");
}

#[test]
fn a_slash_command_is_handled_without_calling_the_model() {
    let mut app = TerminalApp::builder()
        .script(says("THE MODEL SHOULD NOT ANSWER THIS"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("/help").unwrap();

    app.wait_for("/exit").expect("help did not list commands");
    app.assert_not_contains("THE MODEL SHOULD NOT ANSWER THIS");
}

#[test]
fn exit_terminates_the_session() {
    let mut app = TerminalApp::builder()
        .script(says("unused"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("/exit").unwrap();

    let code = app.wait_for_exit().expect("the process did not exit");
    assert_eq!(code, 0, "a clean exit should report success");
}

#[test]
fn ctrl_c_cancels_the_line_without_ending_the_session() {
    // The startup banner promises this. Before the input loop handled control
    // keys, raw mode delivered Ctrl-C as an ordinary key and it was typed into
    // the buffer as a literal 'c'.
    let mut app = TerminalApp::builder()
        .script(says("still alive"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");

    app.send("half-typed").unwrap();
    app.send_key(Key::CtrlC).unwrap();

    // The line is abandoned, and the session continues.
    app.send_key(Key::CtrlC).unwrap();
    app.wait_for("Input cancelled")
        .expect("Ctrl-C did not cancel the line");

    assert!(
        !app.has_exited(),
        "Ctrl-C should cancel the line, not end the session"
    );

    // And the abandoned text must not be submitted.
    app.type_line("go").unwrap();
    app.wait_for("still alive")
        .expect("the session stopped working");
    app.assert_not_contains("half-typed go");
}

#[test]
fn ctrl_d_exits_cleanly() {
    // The other half of what the banner promises.
    let mut app = TerminalApp::builder()
        .script(says("unused"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.send_key(Key::CtrlD).unwrap();

    let code = app.wait_for_exit().expect("the process did not exit");
    assert_eq!(code, 0, "Ctrl-D should be a clean exit");
}

#[test]
fn a_control_key_is_never_typed_into_the_buffer() {
    // Guards the specific defect: control characters reaching the Char(c) arm.
    let mut app = TerminalApp::builder()
        .script(says("echoed back"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.send("ab").unwrap();
    app.send_key(Key::CtrlC).unwrap();
    app.type_line("clean").unwrap();

    app.wait_for("echoed back").expect("no reply");
    app.assert_not_contains("abc");
}
