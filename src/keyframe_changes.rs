//! Route a sender-wide native update into latched stream invalidations.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, PoisonError, Weak,
    },
};

use crate::{
    advanced::VideoPacketFormat,
    change_feed::ChangeFeed,
    timeout::{Deadline, NeverStop},
};

type SelectorKey = Option<u32>;

#[derive(Default)]
pub(crate) struct KeyframeChanges {
    changes: ChangeFeed,
    selectors: Mutex<HashMap<SelectorKey, Weak<Selector>>>,
}

struct Selector {
    format: Option<VideoPacketFormat>,
    generation: AtomicU64,
}

impl KeyframeChanges {
    fn lock(&self) -> MutexGuard<'_, HashMap<SelectorKey, Weak<Selector>>> {
        self.selectors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn observe(&self, format: Option<VideoPacketFormat>) -> Observer<'_> {
        // Processing.NDI.Advanced.h specifies that only FourCC selects the
        // stream/bandwidth; geometry and timing must not split one selector.
        let key = format.map(|format| u32::from_ne_bytes(format.raw().FourCC.to_ne_bytes()));
        let mut selectors = self.lock();
        let selector = selectors
            .get(&key)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let selector = Arc::new(Selector {
                    format,
                    generation: AtomicU64::new(0),
                });
                selectors.insert(key, Arc::downgrade(&selector));
                selector
            });
        let observed = selector.generation.load(Ordering::Relaxed);
        Observer {
            changes: self,
            selector,
            observed,
        }
    }

    /// Sample every active selector before the global feed wakes any caller.
    /// Equivalent selectors share one query and one generation. A consumer
    /// can therefore send its keyframe without erasing another caller's report.
    fn publish(&self, required: &mut impl FnMut(VideoPacketFormat) -> bool) {
        let mut selectors = self.lock();
        let mut sampled = Vec::new();
        selectors.retain(|_, weak| {
            let Some(selector) = weak.upgrade() else {
                return false;
            };
            let demanded = match selector.format {
                None => true,
                Some(format) => required(format),
            };
            sampled.push((selector, demanded));
            true
        });
        // Complete all queries before publishing any counter, including when
        // an injected test query unwinds. Native queries call no application code.
        for (selector, demanded) in sampled {
            if demanded {
                selector.generation.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub(crate) fn wait(
        &self,
        format: Option<VideoPacketFormat>,
        deadline: Deadline,
        mut native_wait: impl FnMut(u32) -> bool,
        mut required: impl FnMut(VideoPacketFormat) -> bool,
    ) -> bool {
        let observer = self.observe(format);
        loop {
            // Read the global generation before the selector counter. A report
            // between this check and the wait is then visible in either one.
            let generation = self.changes.generation();
            if observer.changed() {
                return true;
            }
            let changed = self
                .changes
                .wait_since(generation, deadline, NeverStop, |timeout| {
                    if !native_wait(timeout) {
                        return false;
                    }
                    self.publish(&mut required);
                    true
                });
            if observer.changed() {
                return true;
            }
            if !changed || deadline.is_expired() {
                return false;
            }
        }
    }
}

struct Observer<'a> {
    changes: &'a KeyframeChanges,
    selector: Arc<Selector>,
    observed: u64,
}

impl Observer<'_> {
    fn changed(&self) -> bool {
        let _selectors = self.changes.lock();
        self.selector.generation.load(Ordering::Relaxed) != self.observed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{advanced::VideoCodec, StreamTier};
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::{Duration, Instant},
    };

    fn budget() -> Deadline {
        Deadline::after(Duration::from_secs(3)).unwrap()
    }

    #[test]
    fn an_update_is_latched_before_the_first_demand_query_clears_it() {
        let changes = KeyframeChanges::default();
        let format = VideoPacketFormat::new(VideoCodec::H264, 64, 32).unwrap();
        let equivalent = VideoPacketFormat::new(VideoCodec::H264, 128, 64).unwrap();
        let demanded = AtomicBool::new(true);
        let queries = AtomicUsize::new(0);
        let (entered, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        thread::scope(|scope| {
            let changes = &changes;
            let demanded = &demanded;
            let queries = &queries;
            let leader = scope.spawn(move || {
                changes.wait(
                    Some(format),
                    budget(),
                    |_| {
                        entered.send(()).unwrap();
                        released.recv_timeout(Duration::from_secs(3)).unwrap();
                        true
                    },
                    |_| {
                        queries.fetch_add(1, Ordering::Relaxed);
                        demanded.swap(false, Ordering::AcqRel)
                    },
                )
            });
            started.recv_timeout(Duration::from_secs(3)).unwrap();
            let followers: Vec<_> = [None, Some(equivalent)]
                .into_iter()
                .map(|format| {
                    scope.spawn(move || {
                        changes.wait(
                            format,
                            budget(),
                            |_| panic!("a follower needed another native update"),
                            |_| panic!("a follower re-queried cleared demand"),
                        )
                    })
                })
                .collect();
            let end = Instant::now() + Duration::from_secs(3);
            while changes
                .lock()
                .values()
                .map(Weak::strong_count)
                .sum::<usize>()
                != 3
            {
                assert!(Instant::now() < end, "followers did not register");
                thread::yield_now();
            }
            release.send(()).unwrap();
            assert!(leader.join().unwrap());
            for follower in followers {
                assert!(follower.join().unwrap());
            }
        });
        assert!(!demanded.load(Ordering::Acquire));
        assert_eq!(queries.load(Ordering::Relaxed), 1);
        assert!(changes
            .lock()
            .values()
            .all(|selector| selector.strong_count() == 0));
    }

    #[test]
    fn sampling_routes_updates_only_to_demanded_active_selectors() {
        let changes = KeyframeChanges::default();
        let format = VideoPacketFormat::new(VideoCodec::H264, 64, 32).unwrap();
        let all = changes.observe(None);
        let program = changes.observe(Some(format));
        let preview = changes.observe(Some(format.with_tier(StreamTier::Preview)));
        changes.publish(&mut |format| format.tier() == StreamTier::Program);
        assert!(all.changed());
        assert!(program.changed());
        assert!(!preview.changed());
        // A later observer starts after that invalidation, even while older
        // observers still retain their own changed generations.
        let later = changes.observe(Some(format));
        assert!(!later.changed());
        drop((all, program, preview, later));
        assert!(changes
            .lock()
            .values()
            .all(|selector| selector.strong_count() == 0));
    }

    #[test]
    fn an_unwinding_native_wait_releases_its_selector_and_global_leadership() {
        let changes = KeyframeChanges::default();
        let format = VideoPacketFormat::new(VideoCodec::H264, 64, 32).unwrap();
        let panic = std::panic::catch_unwind(|| {
            changes.wait(Some(format), budget(), |_| panic!("native wait"), |_| true)
        });
        assert!(panic.is_err());
        assert!(changes
            .lock()
            .values()
            .all(|selector| selector.strong_count() == 0));
        assert!(changes.wait(Some(format), budget(), |_| true, |_| true));
        assert!(changes
            .lock()
            .values()
            .all(|selector| selector.strong_count() == 0));
    }
    #[test]
    fn an_unwinding_demand_query_publishes_no_partial_generation_and_recovers() {
        let changes = KeyframeChanges::default();
        let format = VideoPacketFormat::new(VideoCodec::H264, 64, 32).unwrap();
        let all = changes.observe(None);
        let program = changes.observe(Some(format));
        let preview = changes.observe(Some(format.with_tier(StreamTier::Preview)));
        let queries = AtomicUsize::new(0);
        let panic = std::panic::catch_unwind(|| {
            changes.publish(&mut |_| {
                if queries.fetch_add(1, Ordering::Relaxed) == 1 {
                    panic!("second demand query");
                }
                true
            });
        });
        assert!(panic.is_err());
        assert_eq!(queries.load(Ordering::Relaxed), 2);
        assert!(changes.selectors.is_poisoned());
        for observer in [&all, &program, &preview] {
            assert!(
                !observer.changed(),
                "no selector publishes before all queries finish"
            );
            assert_eq!(
                Arc::strong_count(&observer.selector),
                1,
                "unwinding releases sampled owners"
            );
        }
        // Recovering the poisoned registry must retain selector sharing and
        // permit a later successful update without replaying a partial one.
        let later = changes.observe(Some(format));
        assert!(Arc::ptr_eq(&program.selector, &later.selector));
        changes.publish(&mut |_| true);
        for observer in [&all, &program, &preview, &later] {
            assert!(observer.changed());
            assert_eq!(observer.selector.generation.load(Ordering::Relaxed), 1);
        }
        drop((all, program, preview, later));
        changes.publish(&mut |_| panic!("unwinding retained a dead selector"));
        assert!(changes.lock().is_empty());
    }

    #[test]
    fn dropping_one_observer_preserves_the_shared_selector_and_generation() {
        let changes = KeyframeChanges::default();
        let format = VideoPacketFormat::new(VideoCodec::H264, 64, 32).unwrap();
        let first = changes.observe(Some(format));
        let second = changes.observe(Some(format));
        assert!(Arc::ptr_eq(&first.selector, &second.selector));
        changes.publish(&mut |_| true);
        drop(first);
        assert!(second.changed());
        let later = changes.observe(Some(format));
        assert!(Arc::ptr_eq(&second.selector, &later.selector));
        assert!(!later.changed());
        drop((second, later));
        assert!(changes
            .lock()
            .values()
            .all(|selector| selector.strong_count() == 0));
        changes.publish(&mut |_| panic!("dead selectors must not be sampled"));
        assert!(changes.lock().is_empty());
    }
}
