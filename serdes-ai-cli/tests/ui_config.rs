//! Terminal UI tests for configuration and per-agent model pinning.
//!
//! Configuration lives in `~/.newcode/config.cfg`. Every test here gets its
//! own HOME, so these exercise the real file without touching the developer's.

#[path = "ui/harness.rs"]
mod harness;

use harness::{TerminalApp, says};

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

    let config = app.home().join(".newcode/config.cfg");
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
        std::fs::read_to_string(app.home().join(".newcode/config.cfg")).unwrap_or_default();

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
        std::fs::read_to_string(app.home().join(".newcode/config.cfg")).unwrap_or_default();
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

#[test]
fn an_existing_code_puppy_install_is_carried_over() {
    // Renaming the settings directory must not look to an existing user like
    // their configuration was wiped.
    let mut app = TerminalApp::builder()
        .legacy_config("[puppy]\nonboarding_complete = true\nmodel = openai:carried-over\n")
        .script(says("unused"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("/exit").unwrap();
    app.wait_for_exit().expect("did not exit");

    let contents = std::fs::read_to_string(app.home().join(".newcode/config.cfg"))
        .expect("settings were not carried over to the new location");

    assert!(
        contents.contains("carried-over"),
        "the previous model setting was lost in the move:\n{contents}"
    );
}

#[test]
fn carrying_settings_over_is_announced() {
    // A silent move leaves a user unsure whether their settings survived.
    let app = TerminalApp::builder()
        .legacy_config("[puppy]\nonboarding_complete = true\n")
        .args(["-p", "hello"])
        .script(says("answered"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(".newcode")
        .expect("the move was never mentioned");
}

#[test]
fn the_previous_settings_are_left_in_place() {
    // Copying rather than moving keeps a downgrade non-destructive.
    let mut app = TerminalApp::builder()
        .legacy_config("[puppy]\nonboarding_complete = true\nmodel = openai:carried-over\n")
        .script(says("unused"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("/exit").unwrap();
    app.wait_for_exit().expect("did not exit");

    assert!(
        app.home().join(".code_puppy/puppy.cfg").exists(),
        "the previous settings were removed rather than copied"
    );
}

#[test]
fn settings_already_at_the_new_location_are_not_overwritten() {
    // Once moved, a stale copy left at the old path must not come back and
    // undo later changes.
    let mut app = TerminalApp::builder()
        .legacy_config("[puppy]\nonboarding_complete = true\nmodel = openai:stale-old\n")
        .config_line("model = openai:current")
        .script(says("unused"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("/exit").unwrap();
    app.wait_for_exit().expect("did not exit");

    let contents =
        std::fs::read_to_string(app.home().join(".newcode/config.cfg")).unwrap_or_default();

    assert!(
        !contents.contains("stale-old"),
        "settings at the old path overwrote the current ones:\n{contents}"
    );
}

#[test]
fn a_fresh_install_writes_only_to_the_new_location() {
    let mut app = TerminalApp::builder()
        .script(says("unused"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("/exit").unwrap();
    app.wait_for_exit().expect("did not exit");

    assert!(app.home().join(".newcode/config.cfg").exists());
    assert!(
        !app.home().join(".code_puppy").exists(),
        "a fresh install recreated the old settings directory"
    );
}

#[test]
fn a_model_with_its_own_endpoint_is_accepted_without_a_provider() {
    // Its selector is a name of the user's choosing, so provider validation
    // must not reject it as an unknown provider.
    let mut app = TerminalApp::builder()
        .models_file(
            r#"{"my-local-model": {"type": "custom_openai", "name": "served-as-this",
                 "custom_endpoint": {"url": "http://127.0.0.1:9/v1", "api_key": "k"}}}"#,
        )
        .script(says("unused"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("/model my-local-model").unwrap();

    app.wait_for("my-local-model").expect("no response");
    app.assert_not_contains("unknown provider");
}

#[test]
fn the_gate_verifiers_can_be_named_in_the_configuration() {
    // The gate otherwise defaults to three models from three providers, which
    // needs credentials for all three — unusable against a single endpoint.
    let mut app = TerminalApp::builder()
        .config_line(r#"gate_verifier_models = ["a-model", "b-model", "c-model"]"#)
        .script(says("unused"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("/exit").unwrap();
    app.wait_for_exit().expect("did not exit");

    let contents =
        std::fs::read_to_string(app.home().join(".newcode/config.cfg")).unwrap_or_default();

    assert!(
        contents.contains("a-model") && contents.contains("c-model"),
        "the configured verifiers were not kept:\n{contents}"
    );
}

#[test]
fn gate_verifiers_may_be_written_as_a_plain_list() {
    // This is usually typed by hand, where JSON brackets and quotes are easy to
    // get wrong.
    let mut app = TerminalApp::builder()
        .config_line("gate_verifier_models = a-model, b-model, c-model")
        .script(says("unused"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.type_line("/exit").unwrap();
    app.wait_for_exit().expect("did not exit");

    let contents =
        std::fs::read_to_string(app.home().join(".newcode/config.cfg")).unwrap_or_default();

    assert!(
        contents.contains("a-model") && contents.contains("c-model"),
        "a comma-separated list was not understood:\n{contents}"
    );
}
