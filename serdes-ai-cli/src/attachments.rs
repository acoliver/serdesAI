//! Attachment parsing module (placeholder)
//!
//! Full implementation will include:
//! - File path extraction (@ prefix)
//! - Clipboard image handling
//! - URL link detection
//! - Image encoding for LLM

/// Placeholder attachment type
#[derive(Debug, Clone)]
pub struct Attachment {
    pub path: String,
    pub content: Vec<u8>,
}

/// Parse attachments from prompt text
pub fn parse_attachments(text: &str) -> (String, Vec<Attachment>) {
    // TODO: Implement full attachment parsing
    (text.to_string(), Vec::new())
}
