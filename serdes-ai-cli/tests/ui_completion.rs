//! Terminal UI tests for the completion dropdown.
//!
//! The dropdown draws itself in raw mode, where the terminal does almost
//! nothing on its own: a newline moves down a row but does not return to the
//! first column, and nothing is erased unless it is erased explicitly. Both of
//! those have to be handled here or the list arrives as a diagonal staircase
//! with the previous list still underneath it.

#[path = "ui/harness.rs"]
mod harness;

use std::time::Duration;

use harness::{TerminalApp, says};

/// The pause after which the application is taken to have finished drawing.
const SETTLED: Duration = Duration::from_millis(250);

/// The description of the first entry, which is always visible when the list
/// opens. The list comes from the command registry and is alphabetical, so
/// naming a command further down would depend on how many commands exist.
const FIRST_ENTRY: &str = "Show or set active agent";

/// An app sitting at the prompt, ready to be typed into.
fn at_prompt() -> TerminalApp {
    let app = TerminalApp::builder()
        .script(says("unused"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app
}

/// The visible rows that look like completion entries.
fn entry_rows(app: &TerminalApp) -> Vec<String> {
    app.screen()
        .into_iter()
        .filter(|row| row.contains(" - /") || row.trim_start().starts_with("/"))
        .filter(|row| row.contains(" - "))
        .collect()
}

#[test]
fn the_dropdown_appears_when_a_command_is_started() {
    let mut app = at_prompt();

    app.send("/").expect("could not type");

    app.wait_for(FIRST_ENTRY)
        .expect("the completion dropdown never appeared");
}

#[test]
fn dropdown_entries_all_start_at_the_same_column() {
    // In raw mode a bare newline does not return to column 0, so each entry
    // starts where the previous one ended and the list walks off to the right.
    let mut app = at_prompt();

    app.send("/").expect("could not type");
    app.wait_for(FIRST_ENTRY).expect("no dropdown");

    // The region redraws per keystroke; reading it mid-draw sees a partial list.
    app.wait_until_idle(SETTLED).expect("never settled");

    let rows = entry_rows(&app);
    assert!(rows.len() > 1, "expected several entries, got {rows:?}");

    let indents: Vec<usize> = rows
        .iter()
        .map(|row| row.len() - row.trim_start().len())
        .collect();

    let first = indents[0];
    assert!(
        indents.iter().all(|indent| *indent == first),
        "the entries are staircased rather than aligned: {indents:?}\n\
         ---- screen ----\n{}",
        app.screen_text()
    );
}

#[test]
fn narrowing_the_search_removes_the_entries_that_no_longer_match() {
    // Only the current line is cleared before redrawing, so a shorter list
    // leaves the tail of the previous one on screen.
    let mut app = at_prompt();

    app.send("/").expect("could not type");
    app.wait_for(FIRST_ENTRY).expect("no dropdown");

    // "/co" cannot match /agent, so that entry must leave the screen. Waiting
    // for something to appear would not do: the narrowed entries are already
    // on screen as part of the wider list.
    app.send("co").expect("could not type");

    app.wait_until_gone(FIRST_ENTRY)
        .expect("an entry that no longer matches was left on screen");
}

#[test]
fn dismissing_the_dropdown_clears_it_from_the_screen() {
    let mut app = at_prompt();

    app.send("/").expect("could not type");
    app.wait_for(FIRST_ENTRY).expect("no dropdown");

    // Removing the slash leaves nothing to complete.
    app.send_key(harness::Key::Backspace)
        .expect("could not send backspace");

    app.wait_until_gone(FIRST_ENTRY)
        .expect("the dropdown was left behind after it stopped applying");
}

#[test]
fn the_prompt_keeps_showing_the_active_model() {
    // The prompt names the model so it is clear what a message will be sent to.
    // The dropdown redraws the whole line and must not replace it with one of
    // its own.
    let mut app = TerminalApp::builder()
        .args(["-m", "openai:the-active-model"])
        .script(says("unused"))
        .spawn()
        .expect("failed to spawn");

    app.wait_for(">>>").expect("no prompt appeared");
    app.send("/").expect("could not type");
    app.wait_for(FIRST_ENTRY).expect("no dropdown");
    app.wait_until_idle(SETTLED).expect("never settled");

    let screen = app.screen_text();
    assert!(
        screen.contains("the-active-model"),
        "the dropdown replaced the prompt with one of its own:\n{screen}"
    );
}

#[test]
fn typing_continues_to_work_after_the_dropdown_has_shown() {
    // Whatever the dropdown does to the cursor, the line being edited has to
    // remain correct.
    let mut app = at_prompt();

    app.send("/").expect("could not type");
    app.wait_for(FIRST_ENTRY).expect("no dropdown");

    app.type_line("help").expect("could not finish the command");

    app.wait_for("/exit")
        .expect("the command did not run, so the typed line was wrong");
}

#[test]
fn a_command_typed_straight_through_still_runs() {
    // The dropdown is open the whole time this is typed, so it is the case most
    // likely to corrupt the buffer.
    let mut app = at_prompt();

    app.type_line("/help").expect("could not type the command");

    app.wait_for("/exit").expect("/help did not run");
}

#[test]
fn the_dropdown_hangs_below_the_line_being_typed() {
    // The list belongs under the prompt, and the prompt has to stay visible: a
    // list drawn over it would leave the user typing somewhere they cannot see.
    let mut app = at_prompt();

    app.send("/").expect("could not type");
    app.wait_for(FIRST_ENTRY).expect("no dropdown");
    app.wait_until_idle(SETTLED).expect("never settled");

    let rows: Vec<String> = app
        .screen()
        .into_iter()
        .filter(|row| !row.trim().is_empty())
        .collect();

    let prompt_row = rows
        .iter()
        .rposition(|row| row.contains(">>>"))
        .expect("the prompt was overwritten by the dropdown");
    let first_entry = rows
        .iter()
        .position(|row| row.contains(FIRST_ENTRY))
        .expect("the dropdown is not on screen");

    assert!(
        prompt_row < first_entry,
        "the dropdown was drawn above the prompt:\n{}",
        app.screen_text()
    );
}

#[test]
fn enter_chooses_the_highlighted_entry() {
    // Enter used to submit whatever had been typed — the bare "/" — throwing the
    // selection away and making the arrow keys pointless.
    let mut app = at_prompt();

    app.send("/").expect("could not type");
    app.wait_for(FIRST_ENTRY).expect("no dropdown");
    app.wait_until_idle(SETTLED).expect("never settled");

    app.send_key(harness::Key::Enter).expect("could not choose");
    app.wait_until_idle(SETTLED).expect("never settled");

    let screen = app.screen_text();
    assert!(
        screen.contains(">>> /agent"),
        "Enter did not put the highlighted entry on the line:\n{screen}"
    );
}

#[test]
fn arrow_keys_change_what_enter_chooses() {
    let mut app = at_prompt();

    app.send("/").expect("could not type");
    app.wait_for(FIRST_ENTRY).expect("no dropdown");
    app.wait_until_idle(SETTLED).expect("never settled");

    app.send_key(harness::Key::Down)
        .expect("could not move down");
    app.send_key(harness::Key::Enter).expect("could not choose");
    app.wait_until_idle(SETTLED).expect("never settled");

    let screen = app.screen_text();
    assert!(
        !screen.contains(">>> /agent "),
        "Enter chose the first entry rather than the highlighted one:\n{screen}"
    );
}

#[test]
fn enter_submits_once_the_list_is_closed() {
    // Choosing must not also run the command: these mostly take arguments, so
    // the line has to stay editable.
    let mut app = at_prompt();

    app.send("/sho").expect("could not type");
    app.wait_for("Show current configuration")
        .expect("no dropdown");
    app.wait_until_idle(SETTLED).expect("never settled");

    app.send_key(harness::Key::Enter).expect("could not choose");
    app.wait_until_idle(SETTLED).expect("never settled");

    // The list is closed and the command has not run: the line is still being
    // edited, which is what lets an argument be typed.
    assert!(app.screen_text().contains(">>> /show"));
    app.assert_not_contains("current_agent");

    app.send_key(harness::Key::Enter).expect("could not submit");
    app.wait_for("current_agent")
        .expect("the chosen command never ran");
}

#[test]
fn tab_and_enter_choose_the_same_way() {
    let mut with_tab = at_prompt();
    with_tab.send("/mod").expect("could not type");
    with_tab
        .wait_for("Show or set active model")
        .expect("no dropdown");
    with_tab.send_key(harness::Key::Tab).expect("could not tab");
    with_tab.wait_until_idle(SETTLED).expect("never settled");

    let mut with_enter = at_prompt();
    with_enter.send("/mod").expect("could not type");
    with_enter
        .wait_for("Show or set active model")
        .expect("no dropdown");
    with_enter
        .send_key(harness::Key::Enter)
        .expect("could not enter");
    with_enter.wait_until_idle(SETTLED).expect("never settled");

    let line = |app: &TerminalApp| {
        app.screen()
            .into_iter()
            .find(|row| row.contains(">>>"))
            .unwrap_or_default()
    };

    assert_eq!(line(&with_tab), line(&with_enter));
}

#[test]
fn a_chosen_command_can_take_an_argument() {
    // The point of not running it immediately.
    let mut app = at_prompt();

    app.send("/pin").expect("could not type");
    app.wait_for("Pin a specific model").expect("no dropdown");
    app.send_key(harness::Key::Enter).expect("could not choose");
    app.wait_until_idle(SETTLED).expect("never settled");

    app.send("default openai:chosen-model")
        .expect("could not type an argument");
    app.wait_until_idle(SETTLED).expect("never settled");

    let screen = app.screen_text();
    assert!(
        screen.contains("/pin_model default openai:chosen-model"),
        "an argument could not be typed after choosing:\n{screen}"
    );
}

#[test]
fn a_command_that_is_not_a_prefix_of_another_is_not_replaced() {
    // /mode is a command in its own right and /model starts with it. Enter must
    // run what was typed rather than completing it to the longer name.
    let mut app = at_prompt();

    app.type_line("/mode").expect("could not type the command");

    app.wait_for("Mode:")
        .expect("/mode was replaced by a longer command");
}

#[test]
fn every_command_can_be_completed() {
    // The list used to be kept by hand and held fourteen of them, so most
    // commands could never be completed.
    let mut app = at_prompt();

    app.send("/").expect("could not type");
    app.wait_for(FIRST_ENTRY).expect("no dropdown");

    // A command that was never in the hand-written list.
    app.send("pin").expect("could not type");
    app.wait_for("Pin a specific model")
        .expect("a registered command is missing from the completion list");
}
