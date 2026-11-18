//! Stop a worker even when the test that joins it unwinds.

use std::{
    borrow::Borrow,
    sync::atomic::{AtomicBool, Ordering},
};

/// Publish a stop flag on drop. A scoped worker can share a borrowed flag,
/// and a result returned by a worker can own a shared flag.
pub struct StopOnDrop<T: Borrow<AtomicBool>>(pub T);

impl<T: Borrow<AtomicBool>> Drop for StopOnDrop<T> {
    fn drop(&mut self) {
        self.0.borrow().store(true, Ordering::Release);
    }
}
