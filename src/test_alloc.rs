// Thread-local accounting excludes allocations from concurrent test threads.
struct CountingAllocator;
std::thread_local! {
    static TEST_ALLOCATIONS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
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
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        record_allocation();
        unsafe { std::alloc::System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        record_allocation();
        unsafe { std::alloc::System.realloc(p, layout, size) }
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(p, layout) }
    }
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
