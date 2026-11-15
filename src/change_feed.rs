//! Broadcast a consumable native change report to concurrent Rust waiters.
//!
//! Finder's native model is pinned by `a_report_waits_for_the_next_native_wait`,
//! `another_consumers_snapshot_leaves_the_report_for_the_next_wait` and
//! `a_native_wait_consumes_the_report_it_returns` in
//! `tests/change_notifications.rs`: reports survive idle intervals and snapshots,
//! and a native wait consumes the report it returns.

use std::sync::{Condvar, Mutex, MutexGuard};

use crate::timeout::{Deadline, Stop, POLL_INTERVAL};

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
    pub(crate) fn wait_since<S: Stop>(
        &self,
        observed: u64,
        deadline: Deadline,
        stop: S,
        mut native_wait: impl FnMut(u32) -> bool,
    ) -> bool {
        let mut state = self.lock();
        loop {
            if stop.is_stopped() {
                return false;
            }
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
                // Retain the leader role across bounded attempts. No feed lock
                // or follower wake is needed between cancellable native waits.
                let changed = loop {
                    if stop.is_stopped() {
                        break false;
                    }
                    let attempt = if S::CAN_STOP {
                        deadline.within_at(std::time::Instant::now(), POLL_INTERVAL)
                    } else {
                        deadline
                    };
                    let changed = native_wait(attempt.remaining_ms());
                    if changed || !S::CAN_STOP || deadline.is_expired() {
                        break changed;
                    }
                };
                state = self.lock();
                if changed {
                    state.generation = state.generation.wrapping_add(1);
                }
                state.native_waiter = false;
                waiter.armed = false;
                self.changed.notify_all();
                return state.generation != observed;
            }
            let remaining = if S::CAN_STOP {
                deadline.remaining().min(POLL_INTERVAL)
            } else {
                deadline.remaining()
            };
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
    use crate::timeout::NeverStop;
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
    fn synchronous_wait_spends_one_whole_native_budget() {
        let feed = ChangeFeed::default();
        let mut calls = 0;
        assert!(!feed.wait_since(0, budget(), NeverStop, |timeout| {
            calls += 1;
            assert_eq!(timeout, 3000);
            false
        }));
        assert_eq!(calls, 1);
    }

    #[test]
    fn stoppable_leader_keeps_its_role_across_slices_and_hands_off() {
        use std::sync::atomic::AtomicBool;
        let feed = ChangeFeed::default();
        let stop = AtomicBool::new(false);
        let mut calls = 0;
        assert!(!feed.wait_since(0, budget(), &stop, |timeout| {
            assert!(timeout <= 100);
            let state = feed
                .state
                .try_lock()
                .expect("native waits hold no feed lock");
            assert!(state.native_waiter, "slicing must retain the leader role");
            drop(state);
            calls += 1;
            if calls == 3 {
                stop.store(true, Ordering::Release);
            }
            false
        }));
        assert_eq!(calls, 3);
        assert!(feed.wait_since(0, budget(), NeverStop, |_| true));
    }

    #[test]
    fn stoppable_follower_can_leave_a_synchronous_native_wait() {
        use std::sync::atomic::AtomicBool;
        let feed = ChangeFeed::default();
        let stop = AtomicBool::new(false);
        let (entered, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        thread::scope(|scope| {
            let feed = &feed;
            let leader = scope.spawn(move || {
                feed.wait_since(0, budget(), NeverStop, |_| {
                    entered.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(3)).unwrap();
                    false
                })
            });
            started.recv_timeout(Duration::from_secs(3)).unwrap();
            let follower = scope.spawn(|| {
                feed.wait_since(0, budget(), &stop, |_| panic!("overlapping native wait"))
            });
            thread::sleep(Duration::from_millis(20));
            let cancelled = std::time::Instant::now();
            stop.store(true, Ordering::Release);
            assert!(!follower.join().unwrap());
            assert!(cancelled.elapsed() < Duration::from_millis(150));
            release.send(()).unwrap();
            assert!(!leader.join().unwrap());
        });
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
                feed.wait_since(observed, budget(), NeverStop, |_| {
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
                        feed.wait_since(observed, budget(), NeverStop, |_| {
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
        assert!(feed.wait_since(observed, budget(), NeverStop, |_| panic!(
            "report already published"
        )));
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
                feed.wait_since(observed, budget(), NeverStop, |_| {
                    entered.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(3)).unwrap();
                    false
                })
            });
            started.recv_timeout(Duration::from_secs(3)).unwrap();
            let follower = scope.spawn(|| {
                feed.wait_since(observed, budget(), NeverStop, |timeout| {
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
                feed.wait_since(0, budget(), NeverStop, |_| {
                    entered.send(()).unwrap();
                    released.recv_timeout(Duration::from_secs(3)).unwrap();
                    false
                })
            });
            started.recv_timeout(Duration::from_secs(3)).unwrap();
            for timeout in [Duration::ZERO, Duration::from_millis(10)] {
                assert!(!feed.wait_since(
                    0,
                    Deadline::after(timeout).unwrap(),
                    NeverStop,
                    |_| panic!("overlapping native wait")
                ));
            }
            release.send(()).unwrap();
            assert!(!leader.join().unwrap());
        });
        assert!(!feed.wait_since(
            0,
            Deadline::after(Duration::ZERO).unwrap(),
            NeverStop,
            |timeout| {
                assert_eq!(timeout, 0);
                false
            }
        ));
    }

    #[test]
    fn unwinding_releases_the_native_waiter_and_snapshots_can_observe_a_report() {
        let feed = ChangeFeed::default();
        let observed = feed.generation();
        let panic = std::panic::catch_unwind(|| {
            feed.wait_since(observed, budget(), NeverStop, |_| panic!("native wait"))
        });
        assert!(panic.is_err());
        let calls = AtomicUsize::new(0);
        assert!(feed.wait_since(observed, budget(), NeverStop, |_| {
            calls.fetch_add(1, Ordering::Relaxed);
            true
        }));
        // This models a report published while a snapshot/predicate ran.
        assert!(feed.wait_since(observed, budget(), NeverStop, |_| panic!(
            "lost snapshot-to-wait report"
        )));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
}
