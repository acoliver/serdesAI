//! Steering input for running agent streams.
//!
//! A [`SteeringQueue`] lets a caller inject user text into a run that is
//! already in flight, instead of waiting for the current turn to finish. The
//! queued text is delivered to the model at the next tool-call boundary: after
//! the step's tool returns are appended to the history and before the loop
//! issues the next model request.
//!
//! # Delivery rules
//!
//! - Messages are drained FIFO at tool-call boundaries only.
//! - Messages enqueued before the first model request are NOT delivered
//!   immediately; they wait for the first tool-call boundary.
//! - A run that ends without ever crossing a tool-call boundary (text-only
//!   completion, error, cancellation) delivers nothing. Undelivered texts stay
//!   queued for the caller and survive the run: a later run created from the
//!   same queue delivers them at its first tool-call boundary.
//! - A run claims the single channel receiver for its lifetime; concurrent
//!   runs on the same queue are not supported (the second run gets no
//!   receiver and never drains).
//!
//! # Example
//!
//! ```rust
//! use serdes_ai_agent::{RunOptions, SteeringQueue};
//!
//! let queue = SteeringQueue::new();
//! let options = RunOptions::new().steering(queue.clone());
//!
//! // From any task, e.g. a UI input handler, while the agent is working:
//! queue.steer("focus on the error case".to_string());
//!
//! // Not yet delivered; delivery happens at the next tool-call boundary.
//! assert_eq!(queue.pending_len(), 1);
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::mpsc;

/// A cheaply cloneable queue of steering texts for a running agent stream.
///
/// Clone it freely: every clone feeds the same channel. Pass one clone to the
/// run via [`crate::RunOptions::steering`] and keep another wherever user
/// input arrives.
///
/// Texts are drained by the agent stream at tool-call boundaries only, never
/// before the first model request and never at a text-only end of turn.
/// Undelivered texts stay queued for the caller across runs.
#[derive(Debug, Clone)]
pub struct SteeringQueue {
    tx: mpsc::UnboundedSender<String>,
    /// Number of texts enqueued but not yet drained by a run. Authoritative
    /// for `pending_len` because the channel receiver cannot report a length.
    pending: Arc<AtomicUsize>,
    /// The single channel receiver, parked here until a run claims it via
    /// [`SteeringQueue::take_receiver`] and parked back when that run's
    /// receiver is dropped.
    rx_slot: Arc<Mutex<Option<mpsc::UnboundedReceiver<String>>>>,
}

impl SteeringQueue {
    /// Create a new empty steering queue.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            tx,
            pending: Arc::new(AtomicUsize::new(0)),
            rx_slot: Arc::new(Mutex::new(Some(rx))),
        }
    }

    /// Enqueue `text` for delivery at the next tool-call boundary.
    ///
    /// Returns `true` if the text was queued, `false` if the queue is closed
    /// (every receiver has been dropped and none can be claimed again), in
    /// which case the text is dropped. With the usual run lifecycle the
    /// receiver is parked back on the queue when the run ends, so steering
    /// stays possible between runs and returns `true`.
    pub fn steer(&self, text: String) -> bool {
        self.pending.fetch_add(1, Ordering::SeqCst);
        if self.tx.send(text).is_err() {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            return false;
        }
        true
    }

    /// Number of enqueued texts not yet drained by a run.
    ///
    /// A positive count after a run finished means the texts were never
    /// delivered (the run crossed no tool-call boundary); they remain queued.
    pub fn pending_len(&self) -> usize {
        self.pending.load(Ordering::SeqCst)
    }

    /// Claim the single receiver for a run. Returns `None` if another run
    /// already holds it (or holds it via a not-yet-dropped receiver).
    pub(crate) fn take_receiver(&self) -> Option<SteeringReceiver> {
        let rx = self.rx_slot.lock().take()?;
        Some(SteeringReceiver {
            rx: Some(rx),
            pending: Arc::clone(&self.pending),
            slot: Arc::clone(&self.rx_slot),
        })
    }
}

impl Default for SteeringQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// The receiving half of a [`SteeringQueue`], held by a running agent stream.
///
/// Dropping it parks the receiver (and any undrained texts inside it) back on
/// the originating queue, so leftovers survive the run that claimed them.
pub(crate) struct SteeringReceiver {
    /// `Some` for the lifetime of this receiver; taken only by [`Drop`].
    rx: Option<mpsc::UnboundedReceiver<String>>,
    pending: Arc<AtomicUsize>,
    slot: Arc<Mutex<Option<mpsc::UnboundedReceiver<String>>>>,
}

impl SteeringReceiver {
    /// Drain every currently queued text, FIFO, without waiting.
    pub(crate) fn drain(&mut self) -> Vec<String> {
        let mut drained = Vec::new();
        if let Some(rx) = self.rx.as_mut() {
            while let Ok(text) = rx.try_recv() {
                drained.push(text);
            }
        }
        if !drained.is_empty() {
            self.pending.fetch_sub(drained.len(), Ordering::SeqCst);
        }
        drained
    }
}

impl Drop for SteeringReceiver {
    fn drop(&mut self) {
        // Park the receiver (with any undrained texts) back on the queue so
        // leftovers survive this run and a later run can deliver them.
        *self.slot.lock() = self.rx.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steer_before_any_run_stays_pending() {
        let queue = SteeringQueue::new();
        assert_eq!(queue.pending_len(), 0);
        assert!(queue.steer("one".to_string()));
        assert_eq!(queue.pending_len(), 1);
    }

    #[test]
    fn drain_delivers_fifo_and_empties_pending() {
        let queue = SteeringQueue::new();
        queue.steer("one".to_string());
        queue.steer("two".to_string());
        queue.steer("three".to_string());

        let mut rx = queue.take_receiver().expect("receiver not yet claimed");
        assert_eq!(rx.drain(), vec!["one", "two", "three"]);
        assert_eq!(queue.pending_len(), 0);
    }

    #[test]
    fn drain_only_takes_what_is_queued_and_never_blocks() {
        let queue = SteeringQueue::new();
        let mut rx = queue.take_receiver().expect("receiver not yet claimed");
        assert!(rx.drain().is_empty());
        assert!(queue.steer("late".to_string()));
        assert_eq!(rx.drain(), vec!["late"]);
        assert!(rx.drain().is_empty());
    }

    #[test]
    fn receiver_returned_on_drop_keeps_undrained_texts() {
        let queue = SteeringQueue::new();
        queue.steer("survivor".to_string());
        {
            let _rx = queue.take_receiver().expect("receiver not yet claimed");
            // Dropped without draining anything.
        }

        assert_eq!(queue.pending_len(), 1);

        let mut rx = queue.take_receiver().expect("receiver returned on drop");
        assert_eq!(rx.drain(), vec!["survivor"]);
        assert_eq!(queue.pending_len(), 0);
    }

    #[test]
    fn take_receiver_is_single_shot_until_returned() {
        let queue = SteeringQueue::new();
        let held = queue.take_receiver();
        assert!(held.is_some());
        assert!(queue.take_receiver().is_none());
        // Only once the holder is dropped does the receiver come back.
        drop(held);
        assert!(queue.take_receiver().is_some());
    }

    #[test]
    fn clones_feed_the_same_queue() {
        let queue = SteeringQueue::new();
        let clone = queue.clone();
        assert!(clone.steer("via clone".to_string()));
        assert_eq!(queue.pending_len(), 1);

        let mut rx = queue.take_receiver().expect("receiver not yet claimed");
        assert_eq!(rx.drain(), vec!["via clone"]);
    }
}
