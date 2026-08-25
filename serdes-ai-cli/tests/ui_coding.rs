//! Terminal UI tests for the coding tools.
//!
//! Before these existed the CLI's own agent could read files but not change
//! them, and its results arrived as undifferentiated tool text while the
//! renderer's file, diff, grep and shell displays sat unused. These drive the
//! real binary and check both halves: the change actually happening, and the
//! user being shown what happened.

#[path = "ui/harness.rs"]
mod harness;

use harness::{script, text_turn, tool_turn, workspace, TerminalApp};

/// Run one prompt in `dir` with a scripted tool call, then a closing reply.
fn run_tool(
    dir: &std::path::Path,
    tool: &str,
    args: serde_json::Value,
    reply: &str,
) -> TerminalApp {
    TerminalApp::builder()
        .args(["-p", "do it"])
        .cwd(dir)
        .script(script(vec![tool_turn(tool, args), text_turn(reply)]))
        .spawn()
        .expect("failed to spawn")
}

#[test]
fn write_file_creates_the_file() {
    // The gap this closes: the single-agent path had no way to change the tree.
    let dir = workspace().unwrap();

    let app = run_tool(
        dir.path(),
        "write_file",
        serde_json::json!({"path": "created.rs", "content": "fn made() {}\n"}),
        "WROTE-IT",
    );

    app.wait_for("WROTE-IT").expect("the run never finished");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("created.rs")).expect("file was not created"),
        "fn made() {}\n"
    );
}

#[test]
fn write_file_announces_the_edit_with_a_line_count() {
    // The renderer summarises an edit rather than printing the whole file: the
    // path and how many lines were added and removed.
    let dir = workspace().unwrap();

    let app = run_tool(
        dir.path(),
        "write_file",
        serde_json::json!({"path": "shown.rs", "content": "one\ntwo\nthree\n"}),
        "done",
    );

    // The active renderer draws a titled box, not the banner form.
    app.wait_for("Diff")
        .expect("a file change was made with no announcement");
    app.wait_for("shown.rs")
        .expect("the edit did not name the file");
    app.wait_for("+3")
        .expect("the edit did not report its line count");
}

#[test]
fn an_edit_reports_both_additions_and_removals() {
    let dir = workspace().unwrap();
    std::fs::write(dir.path().join("counted.rs"), "keep\nOLD\nkeep2\n").unwrap();

    let app = run_tool(
        dir.path(),
        "edit_file",
        serde_json::json!({
            "path": "counted.rs",
            "old_string": "OLD",
            "new_string": "NEW"
        }),
        "done",
    );

    app.wait_for("+1").expect("the addition was not counted");
    app.wait_for("-1").expect("the removal was not counted");
}

#[test]
fn write_file_creates_parent_directories() {
    let dir = workspace().unwrap();

    let app = run_tool(
        dir.path(),
        "write_file",
        serde_json::json!({"path": "a/b/c/deep.txt", "content": "nested\n"}),
        "NESTED-OK",
    );

    app.wait_for("NESTED-OK").expect("the run never finished");
    assert!(dir.path().join("a/b/c/deep.txt").exists());
}

#[test]
fn edit_file_replaces_an_exact_string() {
    let dir = workspace().unwrap();
    std::fs::write(dir.path().join("edit.rs"), "fn a() {}\nfn b() {}\n").unwrap();

    let app = run_tool(
        dir.path(),
        "edit_file",
        serde_json::json!({
            "path": "edit.rs",
            "old_string": "fn b() {}",
            "new_string": "fn c() {}"
        }),
        "EDITED-OK",
    );

    app.wait_for("EDITED-OK").expect("the run never finished");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("edit.rs")).unwrap(),
        "fn a() {}\nfn c() {}\n"
    );
}

#[test]
fn an_ambiguous_edit_is_refused_and_the_file_is_untouched() {
    // Guessing which occurrence was meant would silently corrupt the file.
    let dir = workspace().unwrap();
    std::fs::write(dir.path().join("amb.rs"), "x\nx\n").unwrap();

    let app = run_tool(
        dir.path(),
        "edit_file",
        serde_json::json!({"path": "amb.rs", "old_string": "x", "new_string": "y"}),
        "FINISHED",
    );

    app.wait_for("FINISHED").expect("the run never finished");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("amb.rs")).unwrap(),
        "x\nx\n",
        "an ambiguous edit modified the file"
    );
}

#[test]
fn a_write_outside_the_working_directory_is_refused() {
    let dir = workspace().unwrap();
    let outside = workspace().unwrap();
    let target = outside.path().join("escaped.txt");

    let app = run_tool(
        dir.path(),
        "write_file",
        serde_json::json!({"path": target.to_string_lossy(), "content": "nope"}),
        "FINISHED",
    );

    app.wait_for("FINISHED").expect("the run never finished");
    assert!(
        !target.exists(),
        "a tool wrote outside the working directory"
    );
}

#[test]
fn bash_runs_a_command_and_shows_its_output() {
    let dir = workspace().unwrap();

    // The command itself is echoed by the shell panel, so asserting on a marker
    // that appears in the command would pass even if the output were dropped —
    // which it was. The marker here can only come from what the command printed.
    let app = run_tool(
        dir.path(),
        "bash",
        serde_json::json!({"command": "printf 'PRODUCED-%s\\n' OUTPUT"}),
        "RAN-IT",
    );

    app.wait_for("PRODUCED-OUTPUT")
        .expect("shell output was not displayed");
    app.wait_for("RAN-IT").expect("the run never finished");
}

#[test]
fn bash_actually_affects_the_working_directory() {
    let dir = workspace().unwrap();

    let app = run_tool(
        dir.path(),
        "bash",
        serde_json::json!({"command": "echo hello > from-shell.txt"}),
        "SHELL-DONE",
    );

    app.wait_for("SHELL-DONE").expect("the run never finished");
    assert!(
        dir.path().join("from-shell.txt").exists(),
        "the command ran somewhere other than the working directory"
    );
}

#[test]
fn a_failing_command_is_reported_rather_than_hidden() {
    let dir = workspace().unwrap();

    let app = run_tool(
        dir.path(),
        "bash",
        serde_json::json!({"command": "echo oops && exit 3"}),
        "SAW-FAILURE",
    );

    app.wait_for("oops").expect("the command's output was lost");
    app.wait_for("SAW-FAILURE").expect("the run never finished");
}

#[test]
fn read_file_displays_the_contents() {
    let dir = workspace().unwrap();
    std::fs::write(dir.path().join("shown.txt"), "READ-CONTENT-MARKER\n").unwrap();

    let app = run_tool(
        dir.path(),
        "read_file",
        serde_json::json!({"path": "shown.txt"}),
        "READ-IT",
    );

    app.wait_for("READ-CONTENT-MARKER")
        .expect("file contents were not displayed");
}

#[test]
fn list_files_displays_the_directory() {
    let dir = workspace().unwrap();
    std::fs::write(dir.path().join("visible.txt"), "x").unwrap();
    std::fs::create_dir(dir.path().join("adir")).unwrap();

    let app = run_tool(
        dir.path(),
        "list_files",
        serde_json::json!({"path": "."}),
        "LISTED",
    );

    // The renderer shows a summary rather than every filename.
    app.wait_for("Directory Listing")
        .expect("the listing was not announced");
    app.wait_for("dirs: 1")
        .expect("the listing did not summarise what it found");
    app.wait_for("LISTED").expect("the run never finished");
}

#[test]
fn grep_finds_and_displays_matches() {
    let dir = workspace().unwrap();
    std::fs::write(
        dir.path().join("haystack.rs"),
        "nothing\nfn NEEDLE_MARKER() {}\nnothing\n",
    )
    .unwrap();

    let app = run_tool(
        dir.path(),
        "grep",
        serde_json::json!({"pattern": "NEEDLE_MARKER", "path": "."}),
        "SEARCHED",
    );

    app.wait_for("NEEDLE_MARKER")
        .expect("the match was not displayed");
    app.wait_for("SEARCHED").expect("the run never finished");
}

#[test]
fn grep_reports_no_matches_rather_than_appearing_to_hang() {
    let dir = workspace().unwrap();
    std::fs::write(dir.path().join("empty.rs"), "nothing here\n").unwrap();

    let app = run_tool(
        dir.path(),
        "grep",
        serde_json::json!({"pattern": "ABSENT_PATTERN", "path": "."}),
        "SEARCH-DONE",
    );

    app.wait_for("SEARCH-DONE").expect("the run never finished");
}
