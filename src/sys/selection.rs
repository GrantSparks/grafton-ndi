//! A process-wide value that is loaded at most once successfully.

use std::{
    sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError},
    thread::{self, ThreadId},
};

use crate::{Error, Result};

/// A value selected once for the process: a failed load can be retried, a
/// successful one is permanent.
///
/// Loading runs outside the internal lock, so foreign code executed while
/// loading (a native library's constructors) cannot deadlock against it, and a
/// load that panics leaves the selection retryable.
pub(crate) struct Selection<T> {
    value: OnceLock<T>,
    loading: Mutex<Option<ThreadId>>,
    changed: Condvar,
}

impl<T> Selection<T> {
    pub(crate) const fn new() -> Self {
        Self {
            value: OnceLock::new(),
            loading: Mutex::new(None),
            changed: Condvar::new(),
        }
    }

    /// Return the selected value, running `load` when nothing is selected yet.
    ///
    /// Concurrent callers wait for the load in progress and then observe its
    /// outcome: the value on success, their own attempt on failure.
    ///
    /// # Errors
    ///
    /// Returns the error of this caller's `load`, or [`Error::RuntimeConflict`]
    /// when `load` re-enters the selection on the thread already loading.
    pub(crate) fn get_or_try_load(&self, load: impl FnOnce() -> Result<T>) -> Result<&T> {
        let current = thread::current().id();
        let mut loading = self.lock();
        loop {
            if let Some(value) = self.value.get() {
                return Ok(value);
            }
            match *loading {
                None => break,
                Some(owner) if owner == current => {
                    return Err(Error::RuntimeConflict(
                        "native module loading re-entered NDI runtime selection".into(),
                    ));
                }
                Some(_) => {
                    loading = self
                        .changed
                        .wait(loading)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
        }
        *loading = Some(current);
        drop(loading);
        // Released on every exit, including unwinding out of `load`.
        let _release = Release(self);
        let value = load()?;
        Ok(self.value.get_or_init(|| value))
    }

    fn lock(&self) -> MutexGuard<'_, Option<ThreadId>> {
        self.loading.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Ends a load: clears its owner and wakes every waiter.
struct Release<'a, T>(&'a Selection<T>);

impl<T> Drop for Release<'_, T> {
    fn drop(&mut self) {
        *self.0.lock() = None;
        self.0.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        panic::{catch_unwind, AssertUnwindSafe},
        sync::{
            atomic::{AtomicUsize, Ordering},
            Barrier,
        },
    };

    #[test]
    fn a_failed_load_is_retryable_and_a_successful_one_is_permanent() {
        let selection = Selection::new();
        assert!(matches!(
            selection.get_or_try_load(|| Err::<u32, _>(Error::RuntimeLoadFailed("missing".into()))),
            Err(Error::RuntimeLoadFailed(_))
        ));
        assert_eq!(*selection.get_or_try_load(|| Ok(1)).unwrap(), 1);
        assert_eq!(*selection.get_or_try_load(|| Ok(2)).unwrap(), 1);
    }

    #[test]
    fn a_panicking_load_does_not_block_later_selection() {
        let selection = Selection::new();
        let unwound = catch_unwind(AssertUnwindSafe(|| {
            let _ = selection.get_or_try_load(|| -> Result<u32> { panic!("loader panicked") });
        }));
        assert!(unwound.is_err());

        // A waiter that arrives on another thread must not find a stale owner.
        thread::scope(|scope| {
            let loaded = scope.spawn(|| selection.get_or_try_load(|| Ok(7)).copied());
            assert_eq!(loaded.join().unwrap().unwrap(), 7);
        });
        assert_eq!(*selection.get_or_try_load(|| Ok(9)).unwrap(), 7);
    }

    #[test]
    fn re_entrant_loading_is_reported_instead_of_deadlocking() {
        let selection = Selection::new();
        let outer = selection.get_or_try_load(|| {
            assert!(matches!(
                selection.get_or_try_load(|| Ok(1)),
                Err(Error::RuntimeConflict(_))
            ));
            Ok(2)
        });
        assert_eq!(*outer.unwrap(), 2);
    }

    #[test]
    fn concurrent_callers_share_one_successful_load() {
        const CALLERS: usize = 16;
        let selection = Selection::new();
        let loads = AtomicUsize::new(0);
        let start = Barrier::new(CALLERS);
        thread::scope(|scope| {
            for _ in 0..CALLERS {
                scope.spawn(|| {
                    start.wait();
                    let value = selection.get_or_try_load(|| {
                        loads.fetch_add(1, Ordering::SeqCst);
                        Ok(5)
                    });
                    assert_eq!(*value.unwrap(), 5);
                });
            }
        });
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }
}
