//! Text input dialog state

use tui_input::Input;

/// Text input dialog wrapper around tui-input.
#[derive(Debug, Clone)]
pub struct InputDialog {
    prompt: String,
    input: Input,
}

impl InputDialog {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            input: Input::default(),
        }
    }

    pub fn prompt(&self) -> &str {
        &self.prompt
    }

    pub fn input(&self) -> &Input {
        &self.input
    }

    pub fn input_mut(&mut self) -> &mut Input {
        &mut self.input
    }

    pub fn value(&self) -> &str {
        self.input.value()
    }
}
