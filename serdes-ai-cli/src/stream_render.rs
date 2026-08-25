//! Rendering the model's answer as it arrives.
//!
//! The answer used to appear all at once: the turn ran to completion and then
//! emitted the finished text. For anything that takes more than a moment that
//! reads as a hang, and the terminal sits empty while the model is plainly
//! producing words.
//!
//! Markdown is awkward to render incrementally — `**bold` is not yet bold, and
//! a table is not a table until its rows have arrived — so this delegates to
//! streamdown, which is built for exactly that. Its parser is line-oriented,
//! while the model emits arbitrary chunks, so the job here is to hold a partial
//! line back until it is complete and to make sure nothing else writes into the
//! middle of one.

use std::io::Write;

use streamdown_parser::Parser as MarkdownParser;
use streamdown_render::Renderer;

/// The width to render at when the terminal will not say.
const FALLBACK_WIDTH: usize = 80;

/// Renders answer text to the terminal as it arrives.
///
/// Feed it whatever chunks the model produces; it emits complete lines as they
/// become complete and holds the rest. [`finish`](Self::finish) flushes what is
/// left, including block elements the parser was still accumulating.
pub struct StreamRenderer {
    parser: MarkdownParser,
    renderer: Renderer<Vec<u8>>,
    width: usize,
    /// The tail of the current line, not yet terminated by a newline.
    pending: String,
    /// Whether anything has been written, so a blank answer prints nothing.
    wrote_anything: bool,
}

impl StreamRenderer {
    /// A renderer sized to the terminal.
    pub fn new() -> Self {
        Self::with_width(terminal_width())
    }

    /// A renderer at an explicit width, for tests.
    pub fn with_width(width: usize) -> Self {
        Self {
            parser: MarkdownParser::new(),
            renderer: Renderer::new(Vec::new(), width),
            width,
            pending: String::new(),
            wrote_anything: false,
        }
    }

    /// Take the next chunk of the answer.
    ///
    /// Returns the text to display, which is empty while a line is still
    /// incomplete. Chunk boundaries are arbitrary — a single newline can arrive
    /// in its own delta — so a chunk carrying several line breaks and a chunk
    /// carrying none both have to work.
    pub fn push(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);

        let mut out = String::new();
        while let Some(index) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=index).collect();
            let line = line.trim_end_matches('\n').trim_end_matches('\r');
            out.push_str(&self.render_line(line));
        }

        if !out.is_empty() {
            self.wrote_anything = true;
        }

        out
    }

    /// Flush the last partial line and anything the parser was still holding.
    ///
    /// Tables and fenced code blocks are buffered until they are complete, so
    /// without this the end of an answer can simply be missing.
    pub fn finish(&mut self) -> String {
        let mut out = String::new();

        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            out.push_str(&self.render_line(&line));
        }

        for event in self.parser.finalize() {
            let _ = self.renderer.render_event(&event);
        }
        out.push_str(&self.drain());

        if !out.is_empty() {
            self.wrote_anything = true;
        }

        out
    }

    /// Whether anything has been rendered.
    pub fn wrote_anything(&self) -> bool {
        self.wrote_anything
    }

    /// Whether a line is part-written, so something else must not cut into it.
    ///
    /// Tool output and status messages go through a different path and would
    /// otherwise land mid-sentence.
    pub fn has_partial_line(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Flush everything and start again from a clean parser.
    ///
    /// Used when something else must write to the terminal — tool output, a
    /// status line — so that content appears in the order it happened rather
    /// than after whatever the parser was still holding.
    pub fn flush_for_interruption(&mut self) -> String {
        let out = self.finish();
        self.parser = MarkdownParser::new();
        self.renderer = Renderer::new(Vec::new(), self.width);
        out
    }

    fn render_line(&mut self, line: &str) -> String {
        for event in self.parser.parse_line(line) {
            let _ = self.renderer.render_event(&event);
        }
        self.drain()
    }

    /// Take what the renderer has written since last time.
    fn drain(&mut self) -> String {
        let _ = self.renderer.writer_mut().flush();
        let bytes = std::mem::take(self.renderer.writer_mut());
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl Default for StreamRenderer {
    fn default() -> Self {
        Self::new()
    }
}

/// The terminal's width, or a readable default.
fn terminal_width() -> usize {
    crossterm::terminal::size()
        .map(|(cols, _)| cols as usize)
        .ok()
        .filter(|width| *width > 0)
        .unwrap_or(FALLBACK_WIDTH)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The visible text, with ANSI styling removed.
    fn plain(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();

        while let Some(c) = chars.next() {
            if c != '\x1b' {
                out.push(c);
                continue;
            }
            // Skip the escape sequence up to its terminating letter.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }

        out
    }

    #[test]
    fn a_complete_line_is_rendered_as_soon_as_it_arrives() {
        let mut renderer = StreamRenderer::with_width(80);

        let out = renderer.push("hello world\n");

        assert!(
            plain(&out).contains("hello world"),
            "a finished line was not rendered: {out:?}"
        );
    }

    #[test]
    fn a_partial_line_is_held_back() {
        // Rendering half a line would commit to formatting that the rest of the
        // line can still change — "**bold" is not bold yet.
        let mut renderer = StreamRenderer::with_width(80);

        let out = renderer.push("hello wor");

        assert_eq!(out, "", "an unfinished line was rendered early");
        assert!(renderer.has_partial_line());
    }

    #[test]
    fn a_line_split_across_chunks_renders_once_whole() {
        let mut renderer = StreamRenderer::with_width(80);

        renderer.push("hello ");
        renderer.push("wor");
        let out = renderer.push("ld\n");

        assert!(plain(&out).contains("hello world"));
    }

    #[test]
    fn several_lines_in_one_chunk_all_render() {
        let mut renderer = StreamRenderer::with_width(80);

        let out = plain(&renderer.push("one\ntwo\nthree\n"));

        for line in ["one", "two", "three"] {
            assert!(out.contains(line), "{line} was dropped from {out:?}");
        }
    }

    #[test]
    fn a_lone_newline_completes_the_held_line() {
        // Deltas split anywhere, so the newline that finishes a line often
        // arrives on its own.
        let mut renderer = StreamRenderer::with_width(80);

        renderer.push("hello");
        let out = renderer.push("\n");

        assert!(plain(&out).contains("hello"));
    }

    #[test]
    fn finishing_flushes_a_line_with_no_trailing_newline() {
        // Models routinely end without one, and that last line is usually the
        // answer.
        let mut renderer = StreamRenderer::with_width(80);

        renderer.push("the final word");
        let out = renderer.finish();

        assert!(
            plain(&out).contains("the final word"),
            "the last line was lost: {out:?}"
        );
    }

    #[test]
    fn markdown_is_styled_rather_than_shown_raw() {
        let mut renderer = StreamRenderer::with_width(80);

        let out = renderer.push("# A heading\n");

        assert!(!out.is_empty(), "the heading produced no output");
        assert!(
            plain(&out).contains("A heading"),
            "the heading text was lost: {out:?}"
        );
    }

    #[test]
    fn a_table_survives_being_streamed_a_line_at_a_time() {
        // Tables are held by the parser until complete, which is the case most
        // likely to lose content if finalize is not called.
        let mut renderer = StreamRenderer::with_width(80);

        let mut out = String::new();
        for line in ["| a | b |", "|---|---|", "| 1 | 2 |"] {
            out.push_str(&renderer.push(&format!("{line}\n")));
        }
        out.push_str(&renderer.finish());

        let out = plain(&out);
        for cell in ["a", "b", "1", "2"] {
            assert!(out.contains(cell), "cell {cell} was lost from {out:?}");
        }
    }

    #[test]
    fn an_empty_answer_renders_nothing() {
        let mut renderer = StreamRenderer::with_width(80);

        let out = renderer.finish();

        assert_eq!(out, "");
        assert!(!renderer.wrote_anything());
    }

    #[test]
    fn a_code_block_keeps_its_contents() {
        let mut renderer = StreamRenderer::with_width(80);

        let mut out = String::new();
        for line in ["```rust", "let x = 1;", "```"] {
            out.push_str(&renderer.push(&format!("{line}\n")));
        }
        out.push_str(&renderer.finish());

        assert!(
            plain(&out).contains("let x = 1;"),
            "the code was lost: {out:?}"
        );
    }
}
