// Thread-local accounting excludes allocations from concurrent test threads.
struct CountingAllocator;
std::thread_local! {
    static TEST_ALLOCATIONS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static TEST_BYTES: std::cell::Cell<Option<(usize, usize)>> = const { std::cell::Cell::new(None) };
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
unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        record_allocation();
        record_bytes(layout.size(), 0);
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        record_allocation();
        record_bytes(layout.size(), 0);
        unsafe { std::alloc::System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        record_allocation();
        record_bytes(size, layout.size());
        unsafe { std::alloc::System.realloc(p, layout, size) }
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: std::alloc::Layout) {
        record_bytes(0, layout.size());
        unsafe { std::alloc::System.dealloc(p, layout) }
    }
}

/// Peak live bytes requested from the allocator on this thread during `f`.
/// Includes codec/output buffers, excludes previously allocated source storage
/// and allocator overhead. `f` must not drop preexisting allocations or move
/// measured allocations across threads.
#[cfg(feature = "image-encoding")]
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
