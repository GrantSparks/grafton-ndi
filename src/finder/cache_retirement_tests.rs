use super::*;
use std::{
    alloc::Layout,
    cell::RefCell,
    sync::{mpsc, MutexGuard, TryLockError},
};

thread_local! {
    static RETIRING_CACHE: RefCell<Option<Arc<CachedSources>>> = const { RefCell::new(None) };
    // Deallocations, deallocations while locked, and retired bytes.
    static RETIREMENTS: Cell<(usize, usize, usize)> = const { Cell::new((0, 0, 0)) };
}

fn state(cache: &CachedSources) -> MutexGuard<'_, CacheState> {
    cache
        .cache
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

fn observe_retirement(layout: Layout) {
    let locked = RETIRING_CACHE.with(|cache| {
        let cache = cache.borrow();
        let locked = matches!(
            cache.as_ref().unwrap().cache.try_lock(),
            Err(TryLockError::WouldBlock)
        );
        locked
    });
    RETIREMENTS.with(|counts| {
        let (total, held, bytes) = counts.get();
        counts.set((total + 1, held + usize::from(locked), bytes + layout.size()));
    });
}

fn observe(cache: &Arc<CachedSources>, retire: impl FnOnce()) -> (usize, usize, usize, usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            RETIRING_CACHE.with(|cache| drop(cache.borrow_mut().take()));
        }
    }
    RETIRING_CACHE.with(|current| *current.borrow_mut() = Some(Arc::clone(cache)));
    RETIREMENTS.with(|counts| counts.set((0, 0, 0)));
    let reset = Reset;
    let ((), allocations) = crate::test_alloc::allocation_count(|| {
        crate::test_alloc::observe_deallocations(observe_retirement, retire);
    });
    let (total, held, bytes) = RETIREMENTS.with(Cell::get);
    drop(reset);
    (allocations, total, held, bytes)
}

fn seed(cache: &CachedSources, host: &str, count: usize) {
    let sources = (0..count)
        .map(|index| {
            Source::new(format!("{host} (Camera {index})"))
                .unwrap()
                .with_address(SourceAddress::new(format!("192.0.2.1:{}", 5960 + index)).unwrap())
        })
        .collect();
    let key = HostKey::try_from(HostQuery::checked(host).unwrap()).unwrap();
    let generation = state(cache).generation;
    cache.publish(generation, key, Arc::new(sources));
}

#[test]
fn retirement_reclaims_keys_snapshots_strings_and_map_after_unlocking() {
    let sources = if cfg!(miri) { 2 } else { 32 };
    for clear in [false, true] {
        for poisoned in [false, true] {
            let cache = Arc::new(CachedSources::default());
            seed(&cache, "camera.local", sources);
            seed(&cache, "other.local", sources);
            let capacity = state(&cache).entries.capacity();
            if poisoned {
                let result = std::panic::catch_unwind(|| {
                    let _guard = cache.cache.lock().unwrap();
                    panic!("poison a populated cache before retirement");
                });
                assert!(result.is_err());
            }
            let (allocations, reclaimed, held, bytes) = observe(&cache, || {
                if clear {
                    cache.clear();
                } else {
                    cache.invalidate("CAMERA.LOCAL.");
                }
            });
            assert_eq!(allocations, 0, "retirement must not allocate");
            assert_eq!(held, 0, "all retired ownership must leave the mutex");
            // Each unique name/address has a CString and an Arc allocation;
            // each entry additionally retires its key, vector and Arc header.
            let entries = if clear { 2 } else { 1 };
            assert_eq!(reclaimed, entries * (sources * 4 + 3) + usize::from(clear));
            assert!(bytes > 0);
            assert_eq!(cache.len(), usize::from(!clear));
            assert_eq!(state(&cache).generation, Some(1));
            assert_eq!(
                state(&cache).entries.capacity(),
                if clear { 0 } else { capacity }
            );

            // Clear deliberately returns its bucket storage. A later fill can
            // still publish, allocating a replacement map only when needed.
            let sources = vec![Source::new("camera.local (Refill)").unwrap()];
            let (result, allocations) = crate::test_alloc::allocation_count(|| {
                cache.find_sources_by_host_with::<NeverStop>(
                    HostQuery::checked("camera.local").unwrap(),
                    Deadline::after(Duration::ZERO).unwrap(),
                    None,
                    |_, _| Ok(Ok(sources)),
                )
            });
            assert_eq!(result.unwrap().unwrap().len(), 1);
            // Snapshot vector, its Arc and key; clear additionally needs new
            // map buckets, while invalidate preserves the existing capacity.
            assert_eq!(allocations, if clear { 4 } else { 3 });
            assert_eq!(cache.len(), if clear { 1 } else { 2 });
            assert!(state(&cache).entries.capacity() > 0);
        }
    }
}

#[test]
fn retirement_of_absent_or_invalid_hosts_still_advances_generation_without_allocating() {
    let cache = Arc::new(CachedSources::default());
    seed(&cache, "camera.local", 1);
    for host in ["absent.local", "invalid:host:port"] {
        let previous = state(&cache).generation.unwrap();
        let (allocations, reclaimed, held, _) = observe(&cache, || cache.invalidate(host));
        assert_eq!((allocations, reclaimed, held), (0, 0, 0));
        assert_eq!(state(&cache).generation, Some(previous + 1));
        assert_eq!(cache.len(), 1);
    }
    cache.clear();
    let (allocations, reclaimed, held, _) = observe(&cache, || cache.clear());
    assert_eq!((allocations, reclaimed, held), (0, 0, 0));
    state(&cache).generation = Some(u64::MAX);
    cache.invalidate("invalid:host:port");
    assert_eq!(state(&cache).generation, None);
    seed(&cache, "camera.local", 1);
    assert!(
        cache.is_empty(),
        "exhaustion must keep publication disabled"
    );
}

#[test]
fn snapshot_remains_live_across_retirement_and_post_transition_probes_miss() {
    for clear in [false, true] {
        let cache = Arc::new(CachedSources::default());
        seed(&cache, "camera.local", 1);
        let snapshot = Arc::downgrade(state(&cache).entries.values().next().unwrap());
        let (started_tx, started_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let reader_cache = Arc::clone(&cache);
        let reader = std::thread::spawn(move || {
            let probe = reader_cache
                .probe(HostQuery::checked("camera.local").unwrap(), |sources| {
                    started_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                    assert_eq!(sources[0].name(), "camera.local (Camera 0)");
                    copy_sources(sources)
                })
                .unwrap();
            let CacheProbe::Hit(sources) = probe else {
                panic!("the reader must observe the entry before retirement");
            };
            sources
        });
        started_rx.recv().unwrap();
        let (allocations, reclaimed, held, _) = observe(&cache, || {
            if clear {
                cache.clear();
            } else {
                cache.invalidate("camera.local");
            }
        });
        assert_eq!((allocations, held), (0, 0));
        // A paused reader owns the snapshot; only the key and, for clear, the
        // map allocation retire now. Descriptor storage survives until release.
        assert_eq!(reclaimed, 1 + usize::from(clear));
        assert!(snapshot.upgrade().is_some());
        let probe = cache
            .probe(HostQuery::checked("camera.local").unwrap(), |_| {
                panic!("a post-transition lookup must not see the retired entry")
            })
            .unwrap();
        assert!(matches!(probe, CacheProbe::Miss(_)));
        resume_tx.send(()).unwrap();
        let result = reader.join().unwrap();
        assert_eq!(result[0].name(), "camera.local (Camera 0)");
        assert!(snapshot.upgrade().is_none());
        assert_eq!(result[0].address().unwrap().as_str(), "192.0.2.1:5960");
    }
}

#[test]
#[ignore = "manual cache retirement/contention measurement; prints observations, not thresholds"]
fn measure_cache_retirement_contention() {
    use std::{
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
        time::Instant,
    };

    const ROUNDS: usize = 24;
    const HOSTS: usize = 4;
    const SOURCES: usize = 1024;
    const READERS: usize = 2;
    for clear in [false, true] {
        for detached in [false, true] {
            let cache = Arc::new(CachedSources::default());
            let sampling = AtomicBool::new(false);
            let stop = AtomicBool::new(false);
            let active = AtomicUsize::new(0);
            let mut retirement_ns = 0;
            let mut retirement_allocations = 0;
            let mut refill_ns = 0;
            let mut refill_allocations = 0;
            std::thread::scope(|scope| {
                let mut readers = Vec::new();
                for _ in 0..READERS {
                    readers.push(scope.spawn(|| {
                        let mut samples = Vec::with_capacity(ROUNDS * 4096);
                        let mut misses = 0;
                        let source = Source::new("steady.local (Program)").unwrap();
                        while !stop.load(Ordering::Acquire) {
                            if !sampling.load(Ordering::Acquire) {
                                std::thread::yield_now();
                                continue;
                            }
                            active.fetch_add(1, Ordering::AcqRel);
                            if !sampling.load(Ordering::Acquire) {
                                active.fetch_sub(1, Ordering::AcqRel);
                                continue;
                            }
                            let before = Instant::now();
                            let result = cache
                                .find_sources_by_host_with::<NeverStop>(
                                    HostQuery::checked("steady.local").unwrap(),
                                    Deadline::after(Duration::ZERO).unwrap(),
                                    None,
                                    |_, _| {
                                        misses += 1;
                                        Ok(Ok(vec![source.clone()]))
                                    },
                                )
                                .unwrap()
                                .unwrap();
                            std::hint::black_box(result);
                            let elapsed = before.elapsed().as_nanos();
                            active.fetch_sub(1, Ordering::AcqRel);
                            samples.push(elapsed);
                        }
                        (samples, misses)
                    }));
                }
                for _ in 0..ROUNDS {
                    cache.clear();
                    seed(&cache, "steady.local", 1);
                    for host in 0..HOSTS {
                        seed(&cache, &format!("bulk{host}.local"), SOURCES);
                    }
                    sampling.store(true, Ordering::Release);
                    while active.load(Ordering::Acquire) == 0 {
                        std::thread::yield_now();
                    }
                    let before = Instant::now();
                    let ((), allocations) = crate::test_alloc::allocation_count(|| {
                        if detached {
                            if clear {
                                cache.clear();
                            } else {
                                for host in
                                    ["bulk0.local", "bulk1.local", "bulk2.local", "bulk3.local"]
                                {
                                    cache.invalidate(host);
                                }
                            }
                        } else {
                            // Reproduce the previous lock scope solely for the
                            // comparison. Production always transfers ownership.
                            if clear {
                                let mut locked = state(&cache);
                                locked.invalidate_pending();
                                locked.entries.clear();
                            } else {
                                for host in
                                    ["bulk0.local", "bulk1.local", "bulk2.local", "bulk3.local"]
                                {
                                    let mut locked = state(&cache);
                                    locked.invalidate_pending();
                                    locked.entries.remove(
                                        &HostQuery::checked(host).unwrap() as &dyn HostIdentity
                                    );
                                }
                            }
                        }
                    });
                    retirement_ns += before.elapsed().as_nanos();
                    retirement_allocations += allocations;
                    sampling.store(false, Ordering::Release);
                    while active.load(Ordering::Acquire) > 0 {
                        std::thread::yield_now();
                    }
                    let source = Source::new("refill.local (Program)").unwrap();
                    let sources = vec![source];
                    let before = Instant::now();
                    let (result, allocations) = crate::test_alloc::allocation_count(|| {
                        cache.find_sources_by_host_with::<NeverStop>(
                            HostQuery::checked("refill.local").unwrap(),
                            Deadline::after(Duration::ZERO).unwrap(),
                            None,
                            |_, _| Ok(Ok(sources)),
                        )
                    });
                    refill_ns += before.elapsed().as_nanos();
                    refill_allocations += allocations;
                    drop(result.unwrap().unwrap());
                }
                stop.store(true, Ordering::Release);
                let mut samples = Vec::new();
                let mut misses = 0;
                for reader in readers {
                    let (latencies, reader_misses) = reader.join().unwrap();
                    samples.extend(latencies);
                    misses += reader_misses;
                }
                samples.sort_unstable();
                let percentile = |percent: usize| samples[(samples.len() - 1) * percent / 100];
                eprintln!(
                    "cache retirement: clear={clear} detached={detached} rounds={ROUNDS} \
                     hosts={HOSTS} sources_per_host={SOURCES} readers={READERS} \
                     samples={} misses={misses} lookup_p50_ns={} lookup_p99_ns={} lookup_max_ns={} \
                     retirement_mean_ns={} retirement_allocations={retirement_allocations} \
                     refill_mean_ns={} refill_allocations={refill_allocations}",
                    samples.len(),
                    percentile(50),
                    percentile(99),
                    samples.last().unwrap(),
                    retirement_ns / ROUNDS as u128,
                    refill_ns / ROUNDS as u128,
                );
            });
        }
    }
}
