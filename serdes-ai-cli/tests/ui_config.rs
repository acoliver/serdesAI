//! Terminal UI tests for configuration and per-agent model pinning.
//!
//! Configuration lives in `~/.code_puppy/puppy.cfg`. Every test here gets its
//! own HOME, so these exercise the real file without touching the developer's.

#[path = "ui/harness.rs"]
mod harness;

use harness::{says, TerminalApp};

fn at_prompt(reply: &str) -> TerminalApp {
    let app = TerminalApp::builder()
        .script(says(reply))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app
}

#[test]
fn show_reports_the_current_configuration() {
    let mut app = at_prompt("unused");

    app.type_line("/show").unwrap();

    app.wait_for("model")
        .expect("/show did not report the model");
}

#[test]
fn pinning_a_model_to_an_agent_is_confirmed() {
    let mut app = at_prompt("unused");

    app.type_line("/pin_model default openai:pinned-one")
        .unwrap();

    app.wait_for("pinned-one")
        .expect("pinning was not confirmed");
}

#[test]
fn a_pin_survives_into_the_configuration_file() {
    // A pin that is only held in memory would be lost between sessions, which
    // is the opposite of what pinning is for.
    let mut app = at_prompt("unused");

    app.type_line("/pin_model default openai:persisted-model")
        .unwrap();
    app.wait_for("persisted-model").expect("pinning failed");

    app.type_line("/exit").unwrap();
    app.wait_for_exit().expect("did not exit");

    let config = app.home().join(".code_puppy/puppy.cfg");
    let contents = std::fs::read_to_string(&config)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", config.display()));

    assert!(
        contents.contains("persisted-model"),
        "the pin was not written to the configuration:\n{contents}"
    );
}

#[test]
fn a_pinned_agent_uses_its_own_model() {
    // The point of pinning: this agent runs on its model rather than the
    // session-wide one.
    let app = TerminalApp::builder()
        .args(["-a", "default", "-p", "hello"])
        .env("RUST_LOG", "info")
        .config_line(r#"pinned_models = {"default": "openai:AGENT-SPECIFIC-MODEL"}"#)
        .script(says("answered"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("AGENT-SPECIFIC-MODEL")
        .expect("the pinned model was not used for that agent");
}

#[test]
fn an_unpinned_agent_falls_back_to_the_session_model() {
    let app = TerminalApp::builder()
        .args([
            "-a",
            "code-puppy",
            "-m",
            "openai:session-model",
            "-p",
            "hello",
        ])
        .env("RUST_LOG", "info")
        .config_line(r#"pinned_models = {"default": "openai:AGENT-SPECIFIC-MODEL"}"#)
        .script(says("answered"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("session-model")
        .expect("an unpinned agent did not use the session model");
    app.assert_not_contains("AGENT-SPECIFIC-MODEL");
}

#[test]
fn unpinning_restores_the_session_model() {
    let mut app = at_prompt("unused");

    app.type_line("/pin_model default openai:temporary")
        .unwrap();
    app.wait_for("temporary").expect("pinning failed");

    app.type_line("/unpin default").unwrap();
    app.type_line("/exit").unwrap();
    app.wait_for_exit().expect("did not exit");

    let contents =
        std::fs::read_to_string(app.home().join(".code_puppy/puppy.cfg")).unwrap_or_default();

    assert!(
        !contents.contains("openai:temporary"),
        "the pin survived being removed:\n{contents}"
    );
}

#[test]
fn pinning_one_agent_leaves_the_others_alone() {
    // Pinning used to also overwrite the session-wide model, so pinning a model
    // to one agent silently moved every other agent onto it too.
    let mut app = TerminalApp::builder()
        .args(["-m", "openai:session-model"])
        .config_line("model = openai:session-model")
        .script(says("unused"))
        .spawn()
        .expect("failed to spawn");
    app.wait_for(">>>").expect("no prompt appeared");

    app.type_line("/pin_model default openai:only-for-default")
        .unwrap();
    app.wait_for("only-for-default").expect("pinning failed");

    app.type_line("/exit").unwrap();
    app.wait_for_exit().expect("did not exit");

    let contents =
        std::fs::read_to_string(app.home().join(".code_puppy/puppy.cfg")).unwrap_or_default();
    let session_model = contents
        .lines()
        .find(|l| l.trim_start().starts_with("model ="))
        .expect("no session model in the configuration");

    assert!(
        session_model.contains("session-model"),
        "pinning an agent changed the session-wide model: {session_model}"
    );
}

#[test]
fn pinning_to_an_unknown_agent_is_refused() {
    let mut app = at_prompt("unused");

    app.type_line("/pin_model nosuchagent openai:gpt-4o")
        .unwrap();

    app.wait_for("nosuchagent")
        .expect("an unknown agent was not named in the refusal");
}

#[test]
fn pinning_an_unknown_provider_is_refused() {
    // Otherwise the failure would only surface later, when the agent is run.
    let mut app = at_prompt("unused");

    app.type_line("/pin_model default mycompany:internal")
        .unwrap();

    app.wait_for("unknown provider")
        .expect("an unusable model was accepted as a pin");
}

#[test]
fn a_saved_endpoint_is_used_without_a_flag() {
    // base_urls in the configuration should work the same as the flag, so a
    // self-hosted setup does not need repeating on every invocation.
    let app = TerminalApp::builder()
        .args(["-p", "hello"])
        .env("RUST_LOG", "info")
        .env("OPENAI_API_KEY", "not-needed")
        .config_line(r#"base_urls = {"openai": "http://127.0.0.1:9/v1"}"#)
        .spawn()
        .expect("failed to spawn");

    // Nothing is listening on port 9, so the request must fail against that
    // address rather than silently reaching the real provider.
    app.wait_for("127.0.0.1:9")
        .expect("the saved endpoint was ignored");
}
