//! Wiggum Loop State - Auto re-prompt functionality

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Wiggum state
pub struct WiggumState {
    active: AtomicBool,
    prompt: Mutex<String>,
    iteration: Mutex<usize>,
}

impl WiggumState {
    pub fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            prompt: Mutex::new(String::new()),
            iteration: Mutex::new(0),
        }
    }

    /// Start wiggum loop with given prompt
    pub fn start(&self, prompt: &str) {
        *self.prompt.lock().expect("wiggum prompt mutex poisoned") = prompt.to_string();
        *self
            .iteration
            .lock()
            .expect("wiggum iteration mutex poisoned") = 0;
        self.active.store(true, Ordering::SeqCst);
    }

    /// Stop wiggum loop
    pub fn stop(&self) {
        self.active.store(false, Ordering::SeqCst);
        *self.prompt.lock().expect("wiggum prompt mutex poisoned") = String::new();
        *self
            .iteration
            .lock()
            .expect("wiggum iteration mutex poisoned") = 0;
    }

    /// Check if wiggum is active
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Get current prompt
    pub fn get_prompt(&self) -> String {
        self.prompt
            .lock()
            .expect("wiggum prompt mutex poisoned")
            .clone()
    }

    /// Increment iteration and get next prompt
    pub fn next_iteration(&self) -> String {
        let mut iter = self
            .iteration
            .lock()
            .expect("wiggum iteration mutex poisoned");
        *iter += 1;
        self.get_prompt()
    }

    /// Get current iteration count
    pub fn get_iteration(&self) -> usize {
        *self
            .iteration
            .lock()
            .expect("wiggum iteration mutex poisoned")
    }

    /// Check if response contains a question (trigger for re-prompt)
    pub fn response_has_question(&self, response: &str) -> bool {
        let lower = response.to_lowercase();

        response.contains('?')
            || lower.contains("what")
            || lower.contains("how")
            || lower.contains("why")
            || lower.contains("when")
            || lower.contains("where")
            || lower.contains("can you")
            || lower.contains("could you")
    }
}

impl Default for WiggumState {
    fn default() -> Self {
        Self::new()
    }
}

// Global wiggum state
static WIGGUM: once_cell::sync::Lazy<WiggumState> = once_cell::sync::Lazy::new(WiggumState::new);

/// Start wiggum loop
pub fn start_wiggum(prompt: &str) {
    WIGGUM.start(prompt);
}

/// Stop wiggum loop
pub fn stop_wiggum() {
    WIGGUM.stop();
}

/// Check if wiggum is active
pub fn is_wiggum_active() -> bool {
    WIGGUM.is_active()
}

/// Get wiggum prompt
pub fn get_wiggum_prompt() -> String {
    WIGGUM.get_prompt()
}

/// Get next wiggum prompt
pub fn next_wiggum_prompt() -> String {
    WIGGUM.next_iteration()
}

/// Get current wiggum iteration count
pub fn get_wiggum_iteration() -> usize {
    WIGGUM.get_iteration()
}

/// Check if response triggers re-prompt
pub fn wiggum_should_continue(response: &str) -> bool {
    WIGGUM.response_has_question(response)
}
