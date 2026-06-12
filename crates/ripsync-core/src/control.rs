//! Cooperative pause and cancellation shared by planning, apply, and verification.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::{Error, Result};

#[derive(Default)]
struct State {
    paused: AtomicBool,
    cancelled: AtomicBool,
    lock: Mutex<()>,
    wake: Condvar,
}

/// Cloneable cooperative run control.
#[derive(Clone, Default)]
pub struct RunControl {
    state: Arc<State>,
}

impl RunControl {
    /// Pause before the next cooperative checkpoint.
    pub fn pause(&self) {
        self.state.paused.store(true, Ordering::Release);
    }

    /// Resume a paused run.
    pub fn resume(&self) {
        self.state.paused.store(false, Ordering::Release);
        self.state.wake.notify_all();
    }

    /// Request graceful cancellation and wake paused workers.
    pub fn cancel(&self) {
        self.state.cancelled.store(true, Ordering::Release);
        self.state.wake.notify_all();
    }

    /// Whether pause is currently requested.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.state.paused.load(Ordering::Acquire)
    }

    /// Whether cancellation is currently requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    /// Block while paused and return [`Error::Cancelled`] when cancelled.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Cancelled`] after cancellation is requested.
    pub fn checkpoint(&self) -> Result<()> {
        if self.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if self.is_paused() {
            let mut guard = self
                .state
                .lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while self.is_paused() && !self.is_cancelled() {
                guard = self
                    .state
                    .wake
                    .wait(guard)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
        if self.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RunControl;

    #[test]
    fn pause_resume_and_cancel() {
        let control = RunControl::default();
        control.pause();
        assert!(control.is_paused());
        control.resume();
        assert!(control.checkpoint().is_ok());
        control.cancel();
        assert!(control.checkpoint().is_err());
    }
}
