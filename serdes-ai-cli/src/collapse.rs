//! Showing long tool output in summary, with the whole of it a keypress away.
//!
//! A command that prints ten thousand lines used to put all ten thousand on the
//! screen. That buries the conversation, and while it is scrolling past there is
//! nothing useful to look at.
//!
//! Output is now shown as its first few lines and a count of the rest, and
//! Ctrl-O expands it in place: what follows moves down, and pressing it again
//! brings that back up.
//!
//! Expanding in place means the block has to stay redrawable, so it is handed to
//! [`crate::screen`] to hold in the region above the prompt rather than written
//! to scrollback. Only the most recent block is redrawable — everything earlier
//! has been committed, and scrollback cannot be rewritten after the fact.

/// How many lines of a long output to show.
const PREVIEW_LINES: usize = 4;

/// Output only slightly longer than the preview is shown whole: hiding two lines
/// behind a note about two lines helps nobody.
const COLLAPSE_ABOVE: usize = PREVIEW_LINES + 2;

/// Decide how `text` should appear.
///
/// Returns the lines to show now, and — when the output was long enough to be
/// worth hiding — the whole text to keep for expansion.
pub fn summarise_parts(text: &str) -> (String, Option<String>) {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= COLLAPSE_ABOVE {
        return (text.to_string(), None);
    }

    let hidden = lines.len() - PREVIEW_LINES;
    let mut shown: Vec<String> = lines[..PREVIEW_LINES]
        .iter()
        .map(|line| (*line).to_string())
        .collect();
    shown.push(format!("... {hidden} more lines - ctrl+o to expand"));

    (shown.join("\n"), Some(text.to_string()))
}

/// What to display for `text`, handing the whole of it to the screen.
///
/// Short output is returned unchanged. Long output becomes its first few lines
/// and a note, with the rest held where Ctrl-O can reach it.
pub fn summarise(_label: &str, text: &str) -> String {
    // Piped or redirected output has no keyboard to expand it with, and
    // something reading it expects what the command actually printed.
    if !crate::screen::is_active() {
        return text.to_string();
    }

    let (shown, full) = summarise_parts(text);
    match full {
        Some(full) => {
            crate::screen::show_block(&shown, &full);
            // Already drawn by the screen, which owns the region it sits in.
            String::new()
        }
        None => shown,
    }
}

/// Expand or collapse the block currently on screen.
pub fn toggle() -> bool {
    crate::screen::toggle_block()
}

/// Whether there is a block to expand.
pub fn pending() -> bool {
    crate::screen::has_block()
}

/// Settle the current block into place, for a session moving on.
pub fn commit() {
    crate::screen::commit_block();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(lines: usize) -> String {
        (1..=lines)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn short_output_is_shown_whole() {
        let (shown, full) = summarise_parts(&text_of(3));

        assert_eq!(shown, "line 1\nline 2\nline 3");
        assert!(
            full.is_none(),
            "short output should not be kept for expansion"
        );
    }

    #[test]
    fn output_just_over_the_preview_is_still_shown_whole() {
        // Hiding two lines behind a note about two lines helps nobody.
        let (shown, full) = summarise_parts(&text_of(COLLAPSE_ABOVE));

        assert!(shown.contains(&format!("line {COLLAPSE_ABOVE}")));
        assert!(full.is_none());
    }

    #[test]
    fn long_output_is_cut_to_the_preview() {
        let (shown, _) = summarise_parts(&text_of(50));

        assert!(shown.contains("line 1"));
        assert!(shown.contains(&format!("line {PREVIEW_LINES}")));
        assert!(!shown.contains(&format!("line {}", PREVIEW_LINES + 1)));
    }

    #[test]
    fn the_note_counts_what_was_left_out() {
        let (shown, _) = summarise_parts(&text_of(50));

        assert!(
            shown.contains(&format!("{} more lines", 50 - PREVIEW_LINES)),
            "unexpected note: {shown}"
        );
    }

    #[test]
    fn the_note_says_how_to_see_the_rest() {
        let (shown, _) = summarise_parts(&text_of(50));

        assert!(shown.contains("ctrl+o"));
    }

    #[test]
    fn the_whole_text_is_kept_for_expansion() {
        let (_, full) = summarise_parts(&text_of(50));

        let full = full.expect("nothing was kept");
        assert!(full.contains("line 50"), "the tail was lost");
        assert_eq!(full.lines().count(), 50);
    }
}
