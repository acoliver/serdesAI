//! Progress bar state component

/// Simple progress model for TUI rendering.
#[derive(Debug, Clone, Copy)]
pub struct ProgressBar {
    current: u64,
    total: u64,
}

impl ProgressBar {
    pub fn new(total: u64) -> Self {
        Self { current: 0, total }
    }

    pub fn with_current(total: u64, current: u64) -> Self {
        let mut progress = Self::new(total);
        progress.set_current(current);
        progress
    }

    pub fn current(&self) -> u64 {
        self.current
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn set_current(&mut self, current: u64) {
        self.current = current.min(self.total);
    }

    pub fn set_total(&mut self, total: u64) {
        self.total = total;
        if self.current > self.total {
            self.current = self.total;
        }
    }

    pub fn percentage(&self) -> u16 {
        if self.total == 0 {
            return 0;
        }
        ((self.current as f64 / self.total as f64) * 100.0).round() as u16
    }
}
