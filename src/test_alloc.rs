// Thread-local accounting excludes allocations from concurrent test threads.
struct CountingAllocator;
std::thread_local! {
    static TEST_ALLOCATIONS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static FAIL_ALLOCATIONS_AT_LEAST: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static TEST_BYTES: std::cell::Cell<Option<(usize, usize)>> = const { std::cell::Cell::new(None) };
    static DEALLOCATION_OBSERVER: std::cell::Cell<Option<fn(std::alloc::Layout)>> = const { std::cell::Cell::new(None) };
}
fn record_bytes(added: usize, removed: usize) {
    let _ = TEST_BYTES.try_with(|bytes| {
        if let Some((live, peak)) = bytes.get() {
            let live = live.saturating_sub(removed) + added;
            bytes.set(Some((live, peak.max(live))));
        }
    });
}
fn record_allocation() {
    let _ = TEST_ALLOCATIONS.try_with(|count| {
        if let Some(n) = count.get() {
            count.set(Some(n + 1));
        }
    });
}
fn refuse_allocation(size: usize) -> bool {
    FAIL_ALLOCATIONS_AT_LEAST
        .try_with(|minimum| minimum.get().is_some_and(|minimum| size >= minimum))
        .unwrap_or(false)
}

unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        if refuse_allocation(layout.size()) {
            return std::ptr::null_mut();
        }
        record_allocation();
        record_bytes(layout.size(), 0);
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        if refuse_allocation(layout.size()) {
            return std::ptr::null_mut();
        }
        record_allocation();
        record_bytes(layout.size(), 0);
        unsafe { std::alloc::System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        if refuse_allocation(size) {
            return std::ptr::null_mut();
        }
        record_allocation();
        record_bytes(size, layout.size());
        unsafe { std::alloc::System.realloc(p, layout, size) }
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: std::alloc::Layout) {
        record_bytes(0, layout.size());
        let _ = DEALLOCATION_OBSERVER.try_with(|observer| {
            if let Some(observe) = observer.get() {
                observe(layout);
            }
        });
        unsafe { std::alloc::System.dealloc(p, layout) }
    }
}

/// Peak live bytes requested from the allocator on this thread during `f`.
/// Includes codec/output buffers, excludes previously allocated source storage
/// and allocator overhead. `f` must not drop preexisting allocations or move
/// measured allocations across threads.
pub(crate) fn peak_bytes<T>(f: impl FnOnce() -> T) -> (T, usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_BYTES.with(|bytes| bytes.set(None));
        }
    }
    TEST_BYTES.with(|bytes| bytes.set(Some((0, 0))));
    let reset = Reset;
    let value = f();
    let peak = TEST_BYTES.with(|bytes| bytes.get().unwrap().1);
    drop(reset);
    (value, peak)
}
#[global_allocator]
static TEST_ALLOCATOR: CountingAllocator = CountingAllocator;

pub(crate) fn allocation_count<T>(f: impl FnOnce() -> T) -> (T, usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_ALLOCATIONS.with(|count| count.set(None));
        }
    }
    TEST_ALLOCATIONS.with(|count| count.set(Some(0)));
    let reset = Reset;
    let value = f();
    let count = TEST_ALLOCATIONS.with(|count| count.get().unwrap());
    drop(reset);
    (value, count)
}

/// Observe this thread's deallocations during `f`, restoring an outer observer
/// even on unwind. The callback must neither allocate nor deallocate or panic.
pub(crate) fn observe_deallocations<T>(
    observe: fn(std::alloc::Layout),
    f: impl FnOnce() -> T,
) -> T {
    struct Reset(Option<fn(std::alloc::Layout)>);
    impl Drop for Reset {
        fn drop(&mut self) {
            DEALLOCATION_OBSERVER.with(|observer| observer.set(self.0));
        }
    }
    let reset = Reset(DEALLOCATION_OBSERVER.with(|observer| observer.replace(Some(observe))));
    let result = f();
    drop(reset);
    result
}

/// Refuse large allocations only on this test thread, restoring the previous
/// threshold even if the closure unwinds. Source frames must be built first.
pub(crate) fn fail_allocations_at_least<T>(minimum: usize, f: impl FnOnce() -> T) -> T {
    struct Reset(Option<usize>);
    impl Drop for Reset {
        fn drop(&mut self) {
            FAIL_ALLOCATIONS_AT_LEAST.with(|minimum| minimum.set(self.0));
        }
    }
    let reset = Reset(FAIL_ALLOCATIONS_AT_LEAST.with(|value| value.replace(Some(minimum))));
    let result = f();
    drop(reset);
    result
}
