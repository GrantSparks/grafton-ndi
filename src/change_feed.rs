//! Broadcast a consumable native change report to concurrent Rust waiters.

use std::sync::{Condvar, Mutex, MutexGuard};

use crate::timeout::Deadline;

/// One native wait at a time, with a generation for every observer to compare.
/// No lock is held across the native call or application code.
#[derive(Default)]
pub(crate) struct ChangeFeed {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Default)]
struct State {
    generation: u64,
    native_waiter: bool,
}

impl ChangeFeed {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Read before taking a snapshot, so a report during that snapshot cannot
    /// be lost in the interval before waiting.
    pub(crate) fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// Wait for a report newer than `observed`, spending one absolute budget
    /// on both native and condvar waits. An uncontended zero budget still
    /// makes one nonblocking native call; a contended one only reads the feed.
    pub(crate) fn wait_since(
        &self,
        observed: u64,
        deadline: Deadline,
        native_wait: impl FnOnce(u32) -> bool,
    ) -> bool {
        let mut state = self.lock();
        loop {
            if state.generation != observed {
                return true;
            }
            if !state.native_waiter {
                state.native_waiter = true;
                let mut waiter = NativeWaiter {
                    feed: self,
                    armed: true,
                };
                drop(state);
                let changed = native_wait(deadline.remaining_ms());
                state = self.lock();
                if changed {
                    state.generation = state.generation.wrapping_add(1);
                }
                state.native_waiter = false;
                waiter.armed = false;
                self.changed.notify_all();
                return state.generation != observed;
            }
            let remaining = deadline.remaining();
            if remaining.is_zero() {
                return false;
            }
            // Recheck both the generation and native-waiter flag after every
            // wake, including timeouts, handoff and spurious notifications.
            state = self
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

/// A native wait that unwinds must hand off to the remaining observers too.
struct NativeWaiter<'a> {
    feed: &'a ChangeFeed,
    armed: bool,
}

impl Drop for NativeWaiter<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.feed.lock().native_waiter = false;
            self.feed.changed.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc, Barrier,
        },
        thread,
        time::Duration,
    };

    fn budget() -> Deadline {
        Deadline::after(Duration::from_secs(3)).unwrap()
    }

    #[test]
    fn one_report_wakes_every_observer_without_another_native_call() {
        let feed = ChangeFeed::default();
        let observed = feed.generation();
        let (entered, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let ready = Barrier::new(5);
        thread::scope(|scope| {
            let feed = &feed;
            let leader = scope.spawn(move || {
                feed.wait_since(observed, budget(), |_| {
                    entered.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(3)).unwrap();
                    true
                })
            });
            started.recv_timeout(Duration::from_secs(3)).unwrap();
            let followers: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        ready.wait();
                        feed.wait_since(observed, budget(), |_| {
                            panic!("a follower consumed another report")
                        })
                    })
                })
                .collect();
            ready.wait();
            release.send(()).unwrap();
            assert!(leader.join().unwrap());
            for follower in followers {
                assert!(follower.join().unwrap());
            }
        });
        assert!(feed.wait_since(observed, budget(), |_| panic!("report already published")));
    }

    #[test]
    fn an_expired_native_wait_hands_off_without_spending_the_followers_budget() {
        let feed = ChangeFeed::default();
        let observed = feed.generation();
        let (entered, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        thread::scope(|scope| {
            let feed = &feed;
            let leader = scope.spawn(move || {
                feed.wait_since(observed, budget(), |_| {
                    entered.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(3)).unwrap();
                    false
                })
            });
            started.recv_timeout(Duration::from_secs(3)).unwrap();
            let follower = scope.spawn(|| {
                feed.wait_since(observed, budget(), |timeout| {
                    assert!(timeout > 0);
                    true
                })
            });
            release.send(()).unwrap();
            assert!(!leader.join().unwrap());
            assert!(follower.join().unwrap());
        });
    }

    #[test]
    fn contended_timeouts_neither_enter_the_sdk_nor_take_over_the_wait() {
        let feed = ChangeFeed::default();
        let (entered, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        thread::scope(|scope| {
            let feed = &feed;
            let leader = scope.spawn(move || {
                feed.wait_since(0, budget(), |_| {
                    entered.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(3)).unwrap();
                    false
                })
            });
            started.recv_timeout(Duration::from_secs(3)).unwrap();
            for timeout in [Duration::ZERO, Duration::from_millis(10)] {
                assert!(
                    !feed.wait_since(0, Deadline::after(timeout).unwrap(), |_| panic!(
                        "overlapping native wait"
                    ))
                );
            }
            release.send(()).unwrap();
            assert!(!leader.join().unwrap());
        });
        assert!(
            !feed.wait_since(0, Deadline::after(Duration::ZERO).unwrap(), |timeout| {
                assert_eq!(timeout, 0);
                false
            })
        );
    }

    #[test]
    fn unwinding_releases_the_native_waiter_and_snapshots_can_observe_a_report() {
        let feed = ChangeFeed::default();
        let observed = feed.generation();
        let panic = std::panic::catch_unwind(|| {
            feed.wait_since(observed, budget(), |_| panic!("native wait"))
        });
        assert!(panic.is_err());
        let calls = AtomicUsize::new(0);
        assert!(feed.wait_since(observed, budget(), |_| {
            calls.fetch_add(1, Ordering::Relaxed);
            true
        }));
        // This models a report published while a snapshot/predicate ran.
        assert!(feed.wait_since(observed, budget(), |_| panic!(
            "lost snapshot-to-wait report"
        )));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}
