//! Filesystem operations for coding agents.
//!
//! These are the raw operations, deliberately free of any agent or tool types so
//! they can be tested directly. [`super::register`] wraps them as agent tools.
//!
//! Every path is resolved against a workspace root and rejected if it escapes
//! it, so an agent cannot be talked into editing `~/.ssh/config`.

use std::fs;
use std::path::{Component, Path, PathBuf};

use super::ToolFailure;

/// Result of writing a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOutcome {
    /// Path relative to the workspace root.
    pub path: String,
    /// Whether the file already existed.
    pub existed: bool,
    /// Bytes written.
    pub bytes: usize,
}

/// Result of editing a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditOutcome {
    /// Path relative to the workspace root.
    pub path: String,
    /// Line number where the replacement happened (1-based).
    pub line: usize,
}

/// Resolve `candidate` against `root`, refusing anything that escapes it.
///
/// The path is normalised lexically rather than through `canonicalize`, because
/// the target of a write may not exist yet. `..` components are resolved and any
/// attempt to climb above the root is rejected.
pub fn resolve_in_root(root: &Path, candidate: &str) -> Result<PathBuf, ToolFailure> {
    if candidate.trim().is_empty() {
        return Err(ToolFailure::InvalidPath {
            path: candidate.to_string(),
            reason: "path is empty".to_string(),
        });
    }

    let joined = if Path::new(candidate).is_absolute() {
        PathBuf::from(candidate)
    } else {
        root.join(candidate)
    };

    let mut normalised = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                if !normalised.pop() {
                    return Err(ToolFailure::InvalidPath {
                        path: candidate.to_string(),
                        reason: "path escapes the workspace root".to_string(),
                    });
                }
            }
            Component::CurDir => {}
            other => normalised.push(other.as_os_str()),
        }
    }

    if !normalised.starts_with(root) {
        return Err(ToolFailure::InvalidPath {
            path: candidate.to_string(),
            reason: "path escapes the workspace root".to_string(),
        });
    }

    Ok(normalised)
}

/// Render `path` relative to `root` for display.
fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Read a file's contents.
pub fn read_file(root: &Path, path: &str) -> Result<String, ToolFailure> {
    let resolved = resolve_in_root(root, path)?;

    fs::read_to_string(&resolved).map_err(|e| ToolFailure::Io {
        path: display_path(root, &resolved),
        message: e.to_string(),
    })
}

/// Write `content` to `path`, creating parent directories as needed.
///
/// Creating parents is deliberate: an agent adding a new module should not have
/// to issue a separate `mkdir` through the shell.
pub fn write_file(root: &Path, path: &str, content: &str) -> Result<WriteOutcome, ToolFailure> {
    let resolved = resolve_in_root(root, path)?;
    let existed = resolved.exists();

    if let Some(parent) = resolved.parent() {
        fs::create_dir_all(parent).map_err(|e| ToolFailure::Io {
            path: display_path(root, parent),
            message: e.to_string(),
        })?;
    }

    fs::write(&resolved, content).map_err(|e| ToolFailure::Io {
        path: display_path(root, &resolved),
        message: e.to_string(),
    })?;

    Ok(WriteOutcome {
        path: display_path(root, &resolved),
        existed,
        bytes: content.len(),
    })
}

/// Replace exactly one occurrence of `old` with `new` in `path`.
///
/// Refuses when `old` is absent, or when it appears more than once — an
/// ambiguous edit is a silent corruption waiting to happen, so the agent is made
/// to supply more surrounding context instead.
pub fn edit_file(
    root: &Path,
    path: &str,
    old: &str,
    new: &str,
) -> Result<EditOutcome, ToolFailure> {
    if old.is_empty() {
        return Err(ToolFailure::InvalidEdit {
            path: path.to_string(),
            reason: "old_string is empty; use write_file to create a file".to_string(),
        });
    }

    if old == new {
        return Err(ToolFailure::InvalidEdit {
            path: path.to_string(),
            reason: "old_string and new_string are identical".to_string(),
        });
    }

    let resolved = resolve_in_root(root, path)?;
    let content = fs::read_to_string(&resolved).map_err(|e| ToolFailure::Io {
        path: display_path(root, &resolved),
        message: e.to_string(),
    })?;

    let occurrences = content.matches(old).count();
    match occurrences {
        0 => {
            return Err(ToolFailure::InvalidEdit {
                path: display_path(root, &resolved),
                reason: "old_string was not found in the file".to_string(),
            })
        }
        1 => {}
        n => {
            return Err(ToolFailure::InvalidEdit {
                path: display_path(root, &resolved),
                reason: format!(
                    "old_string appears {n} times; include more surrounding context so it matches exactly once"
                ),
            })
        }
    }

    let offset = content.find(old).expect("occurrence counted above");
    let line = content[..offset].matches('\n').count() + 1;
    let updated = content.replacen(old, new, 1);

    fs::write(&resolved, &updated).map_err(|e| ToolFailure::Io {
        path: display_path(root, &resolved),
        message: e.to_string(),
    })?;

    Ok(EditOutcome {
        path: display_path(root, &resolved),
        line,
    })
}

/// List entries directly under `path`.
pub fn list_files(root: &Path, path: &str) -> Result<Vec<String>, ToolFailure> {
    let resolved = resolve_in_root(root, path)?;

    let entries = fs::read_dir(&resolved).map_err(|e| ToolFailure::Io {
        path: display_path(root, &resolved),
        message: e.to_string(),
    })?;

    let mut names = Vec::new();
    for entry in entries.flatten() {
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let name = entry.file_name().to_string_lossy().into_owned();
        names.push(if is_dir { format!("{name}/") } else { name });
    }
    names.sort();

    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> PathBuf {
        let base = std::env::temp_dir().join(format!("serdes-orch-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&base).unwrap();
        // Resolve symlinks (macOS /var -> /private/var) so root-containment
        // checks compare like with like.
        base.canonicalize().unwrap()
    }

    #[test]
    fn writes_and_reads_back() {
        let root = temp_root();
        let out = write_file(&root, "a/b.txt", "hello").unwrap();

        assert_eq!(out.path, "a/b.txt");
        assert!(!out.existed);
        assert_eq!(out.bytes, 5);
        assert_eq!(read_file(&root, "a/b.txt").unwrap(), "hello");
    }

    #[test]
    fn write_reports_when_overwriting() {
        let root = temp_root();
        write_file(&root, "f.txt", "one").unwrap();
        let second = write_file(&root, "f.txt", "two").unwrap();

        assert!(second.existed);
        assert_eq!(read_file(&root, "f.txt").unwrap(), "two");
    }

    #[test]
    fn edit_replaces_a_unique_occurrence() {
        let root = temp_root();
        write_file(&root, "s.rs", "fn a() {}\nfn b() {}\n").unwrap();

        let out = edit_file(&root, "s.rs", "fn b() {}", "fn c() {}").unwrap();

        assert_eq!(out.line, 2);
        assert_eq!(read_file(&root, "s.rs").unwrap(), "fn a() {}\nfn c() {}\n");
    }

    #[test]
    fn edit_refuses_an_ambiguous_match() {
        let root = temp_root();
        write_file(&root, "s.rs", "x\nx\n").unwrap();

        let err = edit_file(&root, "s.rs", "x", "y").unwrap_err();

        assert!(
            matches!(&err, ToolFailure::InvalidEdit { reason, .. } if reason.contains("appears 2 times")),
            "unexpected error: {err:?}"
        );
        // The file must be left untouched when the edit is refused.
        assert_eq!(read_file(&root, "s.rs").unwrap(), "x\nx\n");
    }

    #[test]
    fn edit_refuses_a_missing_match() {
        let root = temp_root();
        write_file(&root, "s.rs", "hello").unwrap();

        let err = edit_file(&root, "s.rs", "goodbye", "hi").unwrap_err();

        assert!(
            matches!(&err, ToolFailure::InvalidEdit { reason, .. } if reason.contains("not found")),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn edit_refuses_a_no_op() {
        let root = temp_root();
        write_file(&root, "s.rs", "same").unwrap();

        let err = edit_file(&root, "s.rs", "same", "same").unwrap_err();

        assert!(matches!(err, ToolFailure::InvalidEdit { .. }));
    }

    #[test]
    fn edit_of_a_missing_file_is_an_io_error() {
        let root = temp_root();

        let err = edit_file(&root, "nope.rs", "a", "b").unwrap_err();

        assert!(matches!(err, ToolFailure::Io { .. }), "got {err:?}");
    }

    #[test]
    fn paths_may_not_escape_the_root() {
        let root = temp_root();

        for escape in ["../outside.txt", "a/../../outside.txt", "/etc/passwd"] {
            let err = write_file(&root, escape, "nope").unwrap_err();
            assert!(
                matches!(err, ToolFailure::InvalidPath { .. }),
                "{escape} should have been rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn interior_parent_components_are_allowed() {
        let root = temp_root();
        write_file(&root, "a/b/../c.txt", "ok").unwrap();

        assert_eq!(read_file(&root, "a/c.txt").unwrap(), "ok");
    }

    #[test]
    fn empty_path_is_rejected() {
        let root = temp_root();

        assert!(matches!(
            write_file(&root, "   ", "x").unwrap_err(),
            ToolFailure::InvalidPath { .. }
        ));
    }

    #[test]
    fn lists_directory_entries_with_dirs_marked() {
        let root = temp_root();
        write_file(&root, "z.txt", "").unwrap();
        write_file(&root, "sub/inner.txt", "").unwrap();

        let listed = list_files(&root, ".").unwrap();

        assert_eq!(listed, vec!["sub/".to_string(), "z.txt".to_string()]);
    }
}
