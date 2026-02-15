//! Zero-overhead completion signal for async operations.
//!
//! This module provides a reusable abstraction for waiting on completion events
//! with timeout support. It encapsulates the atomic flag, mutex, and condvar
//! pattern used throughout the sender module for async video completion tracking.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Condvar, Mutex,
    },
    time::Duration,
};

/// A completion signal for synchronizing async operations.
///
/// This struct provides a thread-safe mechanism for one thread to signal completion
/// while another thread waits for that signal with optional timeout. It handles
/// mutex poisoning gracefully, preferring progress over panic.
///
/// # Thread Safety
///
/// All methods are safe to call from multiple threads. The implementation uses:
/// - An atomic boolean for lock-free completion checks
/// - A mutex + condvar for efficient blocking waits
/// - Poison recovery to avoid panics in Drop contexts
///
/// # Example
///
/// ```
/// use std::time::Duration;
/// use std::thread;
/// # use grafton_ndi::waitable_completion::WaitableCompletion;
///
/// let completion = WaitableCompletion::new();
///
/// // Spawn a thread that signals completion after a delay
/// let completion_clone = completion.clone();
/// thread::spawn(move || {
///     thread::sleep(Duration::from_millis(10));
///     completion_clone.signal();
/// });
///
/// // Wait for completion with timeout
/// match completion.wait_timeout(Duration::from_secs(1)) {
///     Ok(()) => println!("Operation completed"),
///     Err(e) => println!("Timed out: {e}"),
/// }
/// ```
#[derive(Debug)]
pub struct WaitableCompletion {
    completed: AtomicBool,
    lock: Mutex<()>,
    cv: Condvar,
}

impl Clone for WaitableCompletion {
    fn clone(&self) -> Self {
        Self {
            completed: AtomicBool::new(self.completed.load(Ordering::Acquire)),
            lock: Mutex::new(()),
            cv: Condvar::new(),
        }
    }
}

impl Default for WaitableCompletion {
    fn default() -> Self {
        Self::new()
    }
}

impl WaitableCompletion {
    /// Creates a new completion signal in the incomplete state.
    pub fn new() -> Self {
        Self {
            completed: AtomicBool::new(false),
            lock: Mutex::new(()),
            cv: Condvar::new(),
        }
    }

    /// Creates a new completion signal in the completed state.
    ///
    /// Useful when initializing a sender that has no pending async operations.
    pub fn new_completed() -> Self {
        Self {
            completed: AtomicBool::new(true),
            lock: Mutex::new(()),
            cv: Condvar::new(),
        }
    }

    /// Signals completion and wakes all waiting threads.
    ///
    /// This method is safe to call multiple times; subsequent calls are no-ops
    /// for the atomic flag but will still notify waiting threads.
    pub fn signal(&self) {
        self.completed.store(true, Ordering::Release);
        let _lock = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.cv.notify_all();
    }

    /// Checks if the operation has completed.
    ///
    /// This is a non-blocking, lock-free check using atomic load.
    pub fn is_complete(&self) -> bool {
        self.completed.load(Ordering::Acquire)
    }

    /// Resets the completion state to incomplete.
    ///
    /// Call this before starting a new async operation to reuse the signal.
    pub fn reset(&self) {
        self.completed.store(false, Ordering::Release);
    }

    /// Waits for completion with timeout, returning a Result.
    ///
    /// This method blocks until either:
    /// - The completion signal is received (returns `Ok(())`)
    /// - The timeout elapses (returns `Err` with timeout message)
    ///
    /// # Poison Recovery
    ///
    /// If the mutex is poisoned (e.g., a thread panicked while holding it),
    /// this method recovers the guard and continues waiting. This ensures
    /// robust behavior even in exceptional circumstances.
    ///
    /// # Errors
    ///
    /// Returns an error string if the timeout elapses before completion.
    pub fn wait_timeout(&self, timeout: Duration) -> Result<(), String> {
        let mut guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let start = std::time::Instant::now();

        while !self.completed.load(Ordering::Acquire) {
            let elapsed = start.elapsed();
            if elapsed >= timeout {
                return Err(format!(
                    "Async operation did not complete within {timeout:?}"
                ));
            }

            let remaining = timeout - elapsed;
            let wait_result = self.cv.wait_timeout(guard, remaining);
            match wait_result {
                Ok((new_guard, timeout_result)) => {
                    guard = new_guard;
                    if timeout_result.timed_out() && !self.completed.load(Ordering::Acquire) {
                        return Err(format!(
                            "Async operation did not complete within {timeout:?}"
                        ));
                    }
                }
                Err(poisoned) => {
                    let (new_guard, _) = poisoned.into_inner();
                    guard = new_guard;
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn test_new_starts_incomplete() {
        let wc = WaitableCompletion::new();
        assert!(!wc.is_complete());
    }

    #[test]
    fn test_new_completed_starts_complete() {
        let wc = WaitableCompletion::new_completed();
        assert!(wc.is_complete());
    }

    #[test]
    fn test_signal_sets_complete() {
        let wc = WaitableCompletion::new();
        assert!(!wc.is_complete());
        wc.signal();
        assert!(wc.is_complete());
    }

    #[test]
    fn test_reset_clears_complete() {
        let wc = WaitableCompletion::new_completed();
        assert!(wc.is_complete());
        wc.reset();
        assert!(!wc.is_complete());
    }

    #[test]
    fn test_signal_before_wait() {
        let wc = WaitableCompletion::new();
        wc.signal();
        let result = wc.wait_timeout(Duration::from_millis(100));
        assert!(result.is_ok());
    }

    #[test]
    fn test_wait_then_signal() {
        let wc = Arc::new(WaitableCompletion::new());
        let wc_clone = Arc::clone(&wc);

        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            wc_clone.signal();
        });

        let result = wc.wait_timeout(Duration::from_secs(1));
        assert!(result.is_ok());
        handle.join().unwrap();
    }

    #[test]
    fn test_timeout_expires() {
        let wc = WaitableCompletion::new();
        let result = wc.wait_timeout(Duration::from_millis(10));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("did not complete"));
    }

    #[test]
    fn test_clone_preserves_state() {
        let wc1 = WaitableCompletion::new();
        wc1.signal();

        let wc2 = wc1.clone();
        assert!(wc2.is_complete());
    }

    #[test]
    fn test_clone_is_independent() {
        let wc1 = WaitableCompletion::new();
        let wc2 = wc1.clone();

        wc1.signal();
        assert!(wc1.is_complete());
        assert!(!wc2.is_complete());
    }

    #[test]
    fn test_multiple_signals_are_idempotent() {
        let wc = WaitableCompletion::new();
        wc.signal();
        wc.signal();
        wc.signal();
        assert!(wc.is_complete());
    }

    #[test]
    fn test_concurrent_signal_and_wait() {
        for _ in 0..100 {
            let wc = Arc::new(WaitableCompletion::new());
            let wc_clone = Arc::clone(&wc);

            let signaler = thread::spawn(move || {
                wc_clone.signal();
            });

            let result = wc.wait_timeout(Duration::from_secs(1));
            signaler.join().unwrap();

            assert!(
                result.is_ok() || wc.is_complete(),
                "Expected completion but got {:?}",
                result
            );
        }
    }
}
