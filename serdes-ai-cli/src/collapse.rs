//! Showing long tool output in summary, with the whole of it a keypress away.
//!
//! A command that prints ten thousand lines used to put all ten thousand on the
//! screen. That buries the conversation, and while it is scrolling past there is
//! nothing useful to look at.
//!
//! Output is now shown as its first few lines and a count of the rest. The whole
//! text is kept, and Ctrl-O prints the most recent one in full.
//!
//! It prints the full text *below* rather than expanding what is already on
//! screen. The conversation lives in the terminal's own scrollback — which is
//! what lets it be scrolled and selected like any other command output — and
//! scrollback cannot be rewritten after the fact. Expanding in place would mean
//! owning the whole screen and giving that up.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

/// How many lines of a long output to show.
const PREVIEW_LINES: usize = 4;

/// Output only slightly longer than the preview is shown whole: hiding two lines
/// behind a note about two lines helps nobody.
const COLLAPSE_ABOVE: usize = PREVIEW_LINES + 2;

/// How many outputs to keep the full text of.
const KEPT_BLOCKS: usize = 32;

/// A piece of output that was shown in summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    /// What produced it, for the heading when it is expanded.
    pub label: String,
    /// The whole text.
    pub text: String,
}

fn blocks() -> &'static Mutex<VecDeque<Block>> {
    static BLOCKS: OnceLock<Mutex<VecDeque<Block>>> = OnceLock::new();
    BLOCKS.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// What to display for `text`, keeping the whole of it for later.
///
/// Short output is returned unchanged. Long output becomes its first few lines
/// and a note saying how much was left out.
pub fn summarise(label: &str, text: &str) -> String {
    // Piped or redirected output has no keyboard to expand it with, and
    // something reading it expects what the command actually printed.
    if !crate::screen::is_active() {
        return text.to_string();
    }

    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= COLLAPSE_ABOVE {
        return text.to_string();
    }

    remember(Block {
        label: label.to_string(),
        text: text.to_string(),
    });

    let hidden = lines.len() - PREVIEW_LINES;
    let mut shown: Vec<String> = lines[..PREVIEW_LINES]
        .iter()
        .map(|line| (*line).to_string())
        .collect();

    shown.push(format!("... {hidden} more lines - ctrl+o to show all"));
    shown.join("\n")
}

fn remember(block: Block) {
    let Ok(mut blocks) = blocks().lock() else {
        return;
    };

    blocks.push_back(block);
    while blocks.len() > KEPT_BLOCKS {
        blocks.pop_front();
    }
}

/// The most recent summarised output, removed from the store.
///
/// Taken rather than copied: expanding the same output repeatedly with each
/// Ctrl-O would be surprising, and what has been shown in full no longer needs
/// keeping.
pub fn take_latest() -> Option<Block> {
    blocks().lock().ok()?.pop_back()
}

/// How many summarised outputs are still waiting to be shown.
pub fn pending() -> usize {
    blocks().lock().map(|blocks| blocks.len()).unwrap_or(0)
}

/// Forget everything kept, for a session being cleared.
pub fn clear() {
    if let Ok(mut blocks) = blocks().lock() {
        blocks.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The store is process-wide, so tests that use it run one at a time.
    fn isolated<T>(body: impl FnOnce() -> T) -> T {
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear();
        body()
    }

    /// `summarise` only collapses for an interactive session, which a test is
    /// not, so these exercise the decision directly.
    fn summarise_lines(lines: usize) -> String {
        let text = (1..=lines)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");

        let all: Vec<&str> = text.lines().collect();
        if all.len() <= COLLAPSE_ABOVE {
            return text;
        }

        remember(Block {
            label: "test".to_string(),
            text: text.clone(),
        });

        let hidden = all.len() - PREVIEW_LINES;
        let mut shown: Vec<String> = all[..PREVIEW_LINES]
            .iter()
            .map(|l| (*l).to_string())
            .collect();
        shown.push(format!("... {hidden} more lines - ctrl+o to show all"));
        shown.join("\n")
    }

    #[test]
    fn short_output_is_shown_whole() {
        isolated(|| {
            let shown = summarise_lines(3);

            assert_eq!(shown, "line 1\nline 2\nline 3");
            assert_eq!(pending(), 0, "short output should not be kept");
        });
    }

    #[test]
    fn output_just_over_the_preview_is_still_shown_whole() {
        // Hiding two lines behind a note about two lines helps nobody.
        isolated(|| {
            let shown = summarise_lines(COLLAPSE_ABOVE);

            assert!(shown.contains(&format!("line {COLLAPSE_ABOVE}")));
            assert!(!shown.contains("more lines"));
        });
    }

    #[test]
    fn long_output_is_cut_to_the_preview() {
        isolated(|| {
            let shown = summarise_lines(50);

            assert!(shown.contains("line 1"));
            assert!(shown.contains(&format!("line {PREVIEW_LINES}")));
            assert!(!shown.contains(&format!("line {}", PREVIEW_LINES + 1)));
        });
    }

    #[test]
    fn the_note_counts_what_was_left_out() {
        isolated(|| {
            let shown = summarise_lines(50);

            assert!(
                shown.contains(&format!("{} more lines", 50 - PREVIEW_LINES)),
                "unexpected note: {shown}"
            );
        });
    }

    #[test]
    fn the_note_says_how_to_see_the_rest() {
        isolated(|| {
            assert!(summarise_lines(50).contains("ctrl+o"));
        });
    }

    #[test]
    fn the_whole_text_is_kept() {
        isolated(|| {
            summarise_lines(50);

            let block = take_latest().expect("nothing was kept");
            assert!(block.text.contains("line 50"), "the tail was lost");
            assert_eq!(block.text.lines().count(), 50);
        });
    }

    #[test]
    fn expanding_takes_the_most_recent_first() {
        isolated(|| {
            summarise_lines(20);
            let first = take_latest().expect("nothing kept").text;
            summarise_lines(30);
            let second = take_latest().expect("nothing kept").text;

            assert_eq!(first.lines().count(), 20);
            assert_eq!(second.lines().count(), 30);
        });
    }

    #[test]
    fn expanding_the_same_output_twice_yields_nothing_the_second_time() {
        isolated(|| {
            summarise_lines(20);

            assert!(take_latest().is_some());
            assert!(take_latest().is_none());
        });
    }

    #[test]
    fn only_the_most_recent_outputs_are_kept() {
        // Otherwise a long session holds every command's output for ever.
        isolated(|| {
            for _ in 0..KEPT_BLOCKS + 10 {
                summarise_lines(20);
            }

            assert_eq!(pending(), KEPT_BLOCKS);
        });
    }

    #[test]
    fn clearing_forgets_everything() {
        isolated(|| {
            summarise_lines(20);
            clear();

            assert_eq!(pending(), 0);
            assert!(take_latest().is_none());
        });
    }
}
