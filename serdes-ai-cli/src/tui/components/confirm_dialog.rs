//! Yes/No confirmation dialog state

/// Lightweight confirmation dialog model.
#[derive(Debug, Clone)]
pub struct ConfirmDialog {
    prompt: String,
    confirmed: bool,
}

impl ConfirmDialog {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            confirmed: false,
        }
    }

    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    pub fn is_confirmed(&self) -> bool {
        self.confirmed
    }

    pub fn set_confirmed(&mut self, confirmed: bool) {
        self.confirmed = confirmed;
    }
}
