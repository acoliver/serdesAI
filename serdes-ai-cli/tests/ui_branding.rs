//! Terminal UI tests for what the application calls itself.
//!
//! The interface carried the name of the project this was ported from, and
//! decorated its output with emoji. Both are gone; these keep them gone, and
//! check that a settings file written under the old name still works.

#[path = "ui/harness.rs"]
mod harness;

use harness::{says, TerminalApp};

/// Pictographic characters that should never reach the screen.
const ICONS: &[&str] = &[
    "🐶", "🍩", "📋", "💡", "👋", "✅", "❌", "⚠", "📦", "🔌", "🎯", "🤖", "🎉", "💥", "🛑", "✏",
];

fn at_prompt() -> TerminalApp {
    let app = TerminalApp::builder()
        .script(says("unused"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app
}

#[test]
fn the_status_report_names_nothing_from_the_old_project() {
    let mut app = at_prompt();

    app.type_line("/show").unwrap();
    app.wait_for("current_agent").expect("/show did not report");

    let transcript = app.transcript().to_lowercase();
    assert!(
        !transcript.contains("puppy"),
        "the status report still names the old project:\n{}",
        app.screen_text()
    );
}

#[test]
fn the_status_report_carries_no_icons() {
    let mut app = at_prompt();

    app.type_line("/show").unwrap();
    app.wait_for("current_agent").expect("/show did not report");

    let transcript = app.transcript();
    for icon in ICONS {
        assert!(
            !transcript.contains(icon),
            "the status report still shows {icon}"
        );
    }
}

#[test]
fn the_status_report_drops_the_vestigial_name_fields() {
    // puppy_name and owner_name described nothing the application does.
    let mut app = at_prompt();

    app.type_line("/show").unwrap();
    app.wait_for("current_agent").expect("/show did not report");

    let transcript = app.transcript();
    assert!(
        !transcript.contains("owner_name"),
        "owner_name is still shown"
    );
    assert!(
        !transcript.contains("puppy_name"),
        "puppy_name is still shown"
    );
}

#[test]
fn a_session_shows_no_icons_anywhere() {
    // Covers the banner, the startup notices and a completed turn.
    let app = TerminalApp::builder()
        .args(["-m", "openai:gpt-4o", "-p", "hello"])
        .script(says("answered"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("request(s) in")
        .expect("the turn never finished");

    let transcript = app.transcript();
    for icon in ICONS {
        assert!(!transcript.contains(icon), "a session still shows {icon}");
    }
}

#[test]
fn the_default_agent_is_named_after_this_application() {
    let mut app = at_prompt();

    app.type_line("/show").unwrap();
    app.wait_for("newcode")
        .expect("the default agent is not named newcode");
}

#[test]
fn the_previous_agent_name_is_still_accepted() {
    // A saved setting or a script naming the old agent must keep working
    // rather than failing validation.
    let app = TerminalApp::builder()
        .args(["-a", "code-puppy", "-p", "hello"])
        .script(says("answered"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("answered")
        .expect("the previous agent name was rejected");
    app.assert_not_contains("not found");
}

#[test]
fn a_settings_file_written_under_the_old_name_is_still_read() {
    // The section was renamed along with everything else. Reading only the new
    // name would silently drop an existing user's settings.
    let app = TerminalApp::builder()
        .legacy_config("[puppy]\nonboarding_complete = true\nmodel = openai:carried-model\n")
        .args(["-p", "hello"])
        .env("RUST_LOG", "info")
        .script(says("answered"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for("carried-model")
        .expect("settings under the old section name were ignored");
}

#[test]
fn settings_are_written_under_the_new_section_name() {
    let mut app = at_prompt();

    app.type_line("/exit").unwrap();
    app.wait_for_exit().expect("did not exit");

    let contents =
        std::fs::read_to_string(app.home().join(".newcode/config.cfg")).unwrap_or_default();

    assert!(
        contents.contains("[newcode]"),
        "the settings file still uses the old section name:\n{contents}"
    );
}
