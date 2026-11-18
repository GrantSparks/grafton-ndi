/// Registry/policy overhead only: all aligned backing blocks and measurement
/// vectors are allocated before timing. The preboxed reference also prepares
/// each handle before its cycle's clock starts, excluding pool-claim/boxing cost.
#[test]
#[ignore = "manual custom allocator median/p99 latency comparison"]
fn custom_allocation_cycle_cost() {
    use std::{sync::mpsc::sync_channel, time::Instant};
    #[derive(Clone, Copy, Debug)]
    enum Path { External, Pool, Preboxed }
    const CYCLES: usize = 100_000;
    let mut descriptor = NDIlib_video_frame_v2_t {
        xres: 1920, yres: 1080,
        FourCC: NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVY,
        ..Default::default()
    };
    descriptor.__bindgen_anon_1.line_stride_in_bytes = 3840;
    let request = descriptor.request().unwrap();
    for path in [Path::External, Path::Pool, Path::Preboxed] {
        for producers in [0, 1, 4] {
            let pool = TestPool::new(16, request.bytes);
            let prepared = Arc::new(Mutex::new(Vec::with_capacity(4)));
            let registry = Arc::new(Registry::new(AllocatorLimits {
                max_generations: 1, max_allocations: 16,
                max_bytes: 16 * request.bytes,
                max_concurrent_calls: 4, max_allocation_bytes: request.bytes,
            }.validate().unwrap()).unwrap());
            let policy_pool = pool.clone();
            let policy_prepared = prepared.clone();
            let token = registry.register(move |request| match path {
                Path::External => {
                    let block = policy_pool.claim();
                    Ok(AllocationBuffer::from_external(request, PoolHandle { pool: policy_pool.clone(), block })?)
                }
                Path::Pool => policy_pool.buffer(request),
                Path::Preboxed => Ok(lock(&policy_prepared).pop().unwrap()),
            }).unwrap();
            let prepare = || {
                if matches!(path, Path::Preboxed) {
                    let block = pool.claim();
                    let buffer = AllocationBuffer::from_external(request, PoolHandle { pool: pool.clone(), block }).unwrap();
                    lock(&prepared).push(buffer);
                }
            };
            let mut times = if producers == 0 {
                let mut times = Vec::with_capacity(CYCLES);
                for _ in 0..CYCLES {
                    prepare();
                    let start = Instant::now();
                    let (pointer, _) = registry.allocate(token, request).unwrap();
                    assert!(registry.free(token, pointer.as_ptr()));
                    times.push(start.elapsed().as_nanos() as u64);
                }
                times
            } else {
                // At most eight queued blocks + four producers + one freeing
                // block are live, within the sixteen preallocated pool slots.
                let (tx, rx) = sync_channel::<(usize, Instant)>(8);
                let times = Vec::with_capacity(CYCLES);
                std::thread::scope(|scope| {
                    let free_registry = &registry;
                    let free = scope.spawn(move || {
                        let mut times = times;
                        while let Ok((pointer, start)) = rx.recv() {
                            assert!(free_registry.free(token, pointer as *mut u8));
                            times.push(start.elapsed().as_nanos() as u64);
                        }
                        times
                    });
                    let mut allocating = Vec::with_capacity(producers);
                    for _ in 0..producers {
                        let tx = tx.clone();
                        let registry = &registry;
                        let prepare = &prepare;
                        allocating.push(scope.spawn(move || {
                            for _ in 0..CYCLES / producers {
                                prepare();
                                let start = Instant::now();
                                let (pointer, _) = registry.allocate(token, request).unwrap();
                                tx.send((pointer.as_ptr() as usize, start)).unwrap();
                            }
                        }));
                    }
                    drop(tx);
                    for thread in allocating { thread.join().unwrap(); }
                    free.join().unwrap()
                })
            };
            assert_eq!(times.len(), CYCLES);
            assert_eq!(pool.releases.load(Ordering::Relaxed), CYCLES);
            assert_eq!(registry.usage().retained_allocations(), 0);
            assert!(lock(&prepared).is_empty());
            registry.closing(token);
            times.sort_unstable();
            let pattern = match producers { 0 => "same-thread", 1 => "one-to-one", 4 => "four-to-one", _ => unreachable!() };
            eprintln!("custom-allocation,{path:?},{pattern},{CYCLES},{},{}", times[CYCLES / 2], times[(CYCLES - 1) * 99 / 100]);
        }
    }
}
