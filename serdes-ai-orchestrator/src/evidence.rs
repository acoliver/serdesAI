//! Evidence a verifier judges work by.
//!
//! The gate's whole value rests on this: verifiers must be shown what actually
//! changed and what the build actually did, never the working agents' account of
//! it. A summary claiming something was implemented is not evidence that it was.

use std::fmt::Write as _;
use std::path::Path;

use crate::tools::shell::{self, ShellOutcome};

/// How much of a diff to show a verifier before truncating.
const MAX_DIFF_BYTES: usize = 64 * 1024;

/// What actually happened in the working tree.
#[derive(Debug, Clone)]
pub struct Evidence {
    /// Unified diff against `HEAD`, or `None` when it could not be obtained.
    pub diff: Option<String>,
    /// Paths reported as changed.
    pub changed_files: Vec<String>,
    /// Result of the configured test command, if one was configured and ran.
    pub tests: Option<ShellOutcome>,
    /// Why evidence is incomplete, if it is.
    ///
    /// Surfaced to verifiers rather than swallowed: a verifier that does not
    /// know the diff is missing might read an empty diff as "nothing to check".
    pub gaps: Vec<String>,
}

impl Evidence {
    /// Whether the working tree shows any change at all.
    pub fn has_changes(&self) -> bool {
        !self.changed_files.is_empty() || self.diff.as_ref().is_some_and(|d| !d.trim().is_empty())
    }

    /// Render for inclusion in a verifier's prompt.
    pub fn render(&self) -> String {
        let mut out = String::new();

        if !self.gaps.is_empty() {
            out.push_str("## Evidence gaps\n\n");
            out.push_str(
                "The following could not be gathered. Do not treat their absence \
                 as evidence that nothing changed:\n",
            );
            for gap in &self.gaps {
                let _ = writeln!(out, "- {gap}");
            }
            out.push('\n');
        }

        out.push_str("## Changed files\n\n");
        if self.changed_files.is_empty() {
            out.push_str("(none reported)\n\n");
        } else {
            for file in &self.changed_files {
                let _ = writeln!(out, "- {file}");
            }
            out.push('\n');
        }

        if let Some(diff) = &self.diff {
            out.push_str("## Diff against HEAD\n\n```diff\n");
            out.push_str(diff);
            if !diff.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n\n");
        }

        match &self.tests {
            Some(outcome) => {
                let _ = writeln!(
                    out,
                    "## Test command\n\nExit status: {}\n",
                    match outcome.exit_code {
                        Some(0) => "0 (passed)".to_string(),
                        Some(code) => format!("{code} (failed)"),
                        None => "terminated by signal".to_string(),
                    }
                );
                if !outcome.stdout.trim().is_empty() {
                    let _ = writeln!(out, "\n```\n{}\n```", outcome.stdout.trim());
                }
                if !outcome.stderr.trim().is_empty() {
                    let _ = writeln!(out, "\nstderr:\n```\n{}\n```", outcome.stderr.trim());
                }
            }
            None => out.push_str("## Test command\n\nNo test command was configured.\n"),
        }

        out
    }
}

/// Keep the head of a diff: the first hunks are the ones worth reading.
fn clamp_diff(diff: String) -> (String, Option<String>) {
    if diff.len() <= MAX_DIFF_BYTES {
        return (diff, None);
    }

    let mut cut = MAX_DIFF_BYTES;
    while cut > 0 && !diff.is_char_boundary(cut) {
        cut -= 1;
    }

    (
        format!("{}\n[... diff truncated ...]", &diff[..cut]),
        Some("the diff was too large to include in full".to_string()),
    )
}

/// Gather evidence about the current state of `root`.
///
/// Never fails: a missing git repository or a failing command becomes a recorded
/// gap, because a gate that cannot run is worse than one running on partial
/// evidence it knows is partial.
pub async fn collect(root: &Path, test_command: Option<&str>) -> Evidence {
    let mut gaps = Vec::new();

    let inside_repo = shell::run(root, "git rev-parse --is-inside-work-tree", None)
        .await
        .map(|o| o.success())
        .unwrap_or(false);

    let (diff, changed_files) = if inside_repo {
        // Include untracked files: a new file is exactly the kind of work a plan
        // asks for, and it would otherwise be invisible.
        let diff = match shell::run(root, "git add -A -N . && git diff HEAD", None).await {
            Ok(o) if o.success() => Some(o.stdout),
            Ok(o) => {
                gaps.push(format!(
                    "git diff exited {}: {}",
                    o.exit_code.unwrap_or(-1),
                    o.stderr.trim()
                ));
                None
            }
            Err(e) => {
                gaps.push(format!("git diff could not be run: {e}"));
                None
            }
        };

        let files = match shell::run(root, "git diff HEAD --name-only", None).await {
            Ok(o) if o.success() => o
                .stdout
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect(),
            _ => {
                gaps.push("the changed-file list could not be obtained".to_string());
                Vec::new()
            }
        };

        (diff, files)
    } else {
        gaps.push(
            "the working directory is not a git repository, so no diff is available".to_string(),
        );
        (None, Vec::new())
    };

    let diff = diff.map(|d| {
        let (clamped, gap) = clamp_diff(d);
        if let Some(gap) = gap {
            gaps.push(gap);
        }
        clamped
    });

    let tests = match test_command {
        Some(cmd) => match shell::run(root, cmd, None).await {
            Ok(outcome) => Some(outcome),
            Err(e) => {
                gaps.push(format!("the test command could not be run: {e}"));
                None
            }
        },
        None => None,
    };

    Evidence {
        diff,
        changed_files,
        tests,
        gaps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_root() -> PathBuf {
        let base = std::env::temp_dir().join(format!("serdes-ev-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&base).unwrap();
        base.canonicalize().unwrap()
    }

    fn git(root: &Path, args: &[&str]) {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git failed");
    }

    fn init_repo(root: &Path) {
        git(root, &["init", "-q"]);
        git(root, &["config", "user.email", "t@example.com"]);
        git(root, &["config", "user.name", "T"]);
        fs::write(root.join("base.txt"), "original\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "base"]);
    }

    #[tokio::test]
    async fn a_modified_file_shows_up_in_the_diff() {
        let root = temp_root();
        init_repo(&root);
        fs::write(root.join("base.txt"), "changed\n").unwrap();

        let evidence = collect(&root, None).await;

        assert!(evidence.has_changes());
        assert!(evidence.changed_files.contains(&"base.txt".to_string()));
        assert!(evidence.diff.unwrap().contains("changed"));
    }

    #[tokio::test]
    async fn a_new_untracked_file_shows_up() {
        // The common case for plan work: an added file must not be invisible.
        let root = temp_root();
        init_repo(&root);
        fs::write(root.join("added.rs"), "fn new() {}\n").unwrap();

        let evidence = collect(&root, None).await;

        assert!(
            evidence.changed_files.contains(&"added.rs".to_string()),
            "untracked files must appear: {:?}",
            evidence.changed_files
        );
        assert!(evidence.diff.unwrap().contains("fn new()"));
    }

    #[tokio::test]
    async fn an_unchanged_tree_reports_no_changes() {
        let root = temp_root();
        init_repo(&root);

        let evidence = collect(&root, None).await;

        assert!(!evidence.has_changes());
        assert!(evidence.changed_files.is_empty());
    }

    #[tokio::test]
    async fn a_non_repository_is_recorded_as_a_gap() {
        // Silence here would let a verifier read "no diff" as "nothing to check".
        let root = temp_root();

        let evidence = collect(&root, None).await;

        assert!(!evidence.gaps.is_empty());
        assert!(
            evidence
                .gaps
                .iter()
                .any(|g| g.contains("not a git repository"))
        );
        assert!(evidence.render().contains("Evidence gaps"));
    }

    #[tokio::test]
    async fn test_output_is_captured_including_failure() {
        let root = temp_root();
        init_repo(&root);

        let evidence = collect(&root, Some("echo boom && exit 1")).await;

        let rendered = evidence.render();
        let tests = evidence.tests.expect("no test outcome");
        assert_eq!(tests.exit_code, Some(1));
        assert!(tests.stdout.contains("boom"));
        assert!(rendered.contains("1 (failed)"));
    }

    #[tokio::test]
    async fn a_passing_test_command_is_reported_as_passed() {
        let root = temp_root();
        init_repo(&root);

        let evidence = collect(&root, Some("true")).await;

        let rendered = evidence.render();
        assert!(evidence.tests.unwrap().success());
        assert!(rendered.contains("0 (passed)"));
    }

    #[tokio::test]
    async fn no_test_command_is_stated_explicitly() {
        let root = temp_root();
        init_repo(&root);

        let rendered = collect(&root, None).await.render();

        assert!(rendered.contains("No test command was configured"));
    }

    #[test]
    fn an_oversized_diff_is_truncated_and_flagged() {
        let (clamped, gap) = clamp_diff("x".repeat(MAX_DIFF_BYTES * 2));

        assert!(clamped.len() < MAX_DIFF_BYTES + 200);
        assert!(clamped.ends_with("[... diff truncated ...]"));
        assert!(gap.is_some());
    }
}
