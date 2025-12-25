//! NDI runtime management and initialization.

use once_cell::sync::Lazy;

use std::sync::{Condvar, Mutex, MutexGuard};

use crate::{ndi_lib::*, Error, Result};

/// Runtime lifecycle phase.
///
/// Transitions:
/// - `Uninitialized` → `Initializing` → `Running` (on success)
/// - `Uninitialized` → `Initializing` → `Failed` (on failure)
/// - `Running` → `Destroying` → `Uninitialized` (on last release)
/// - `Failed` → `Initializing` (retry allowed)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Runtime not initialized. Ready for initialization.
    Uninitialized,
    /// Initialization in progress. Other threads should wait.
    Initializing,
    /// Runtime is active with one or more live handles.
    Running,
    /// Destruction in progress. Other threads should wait.
    Destroying,
    /// Last initialization attempt failed. Retry is allowed.
    Failed,
}

/// Internal state protected by the mutex.
struct RuntimeState {
    phase: Phase,
    refcount: usize,
}

impl RuntimeState {
    const fn new() -> Self {
        Self {
            phase: Phase::Uninitialized,
            refcount: 0,
        }
    }
}

/// Process-global runtime manager for NDI.
///
/// This implementation uses a `Mutex` + `Condvar` state machine that:
/// - Allows re-initialization after teardown
/// - Allows retry after initialization failure
/// - Avoids spin loops by using `Condvar` waits
/// - Maintains the invariant: `NDI::new()` returns `Ok` only when runtime is initialized
struct RuntimeManager {
    state: Mutex<RuntimeState>,
    cv: Condvar,
}

impl RuntimeManager {
    const fn new() -> Self {
        Self {
            state: Mutex::new(RuntimeState::new()),
            cv: Condvar::new(),
        }
    }

    /// Recover from mutex poisoning, preferring progress over panic.
    fn recover_guard<'a>(
        result: std::sync::LockResult<MutexGuard<'a, RuntimeState>>,
    ) -> MutexGuard<'a, RuntimeState> {
        result.unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn acquire(&self) -> Result<()> {
        let mut guard = Self::recover_guard(self.state.lock());

        loop {
            match guard.phase {
                Phase::Uninitialized | Phase::Failed => {
                    // We're responsible for initialization
                    guard.phase = Phase::Initializing;
                    drop(guard);

                    // Call NDIlib_initialize outside the lock
                    #[cfg(all(target_os = "windows", debug_assertions))]
                    {
                        if std::env::var("CI").is_ok() {
                            eprintln!("[NDI] Initializing NDI runtime in CI environment...");
                            if let Ok(sdk_dir) = std::env::var("NDI_SDK_DIR") {
                                eprintln!("[NDI] NDI_SDK_DIR: {}", sdk_dir);
                            }
                        }
                    }

                    let succeeded = unsafe { NDIlib_initialize() };

                    // Re-acquire lock to update state
                    guard = Self::recover_guard(self.state.lock());

                    if succeeded {
                        guard.phase = Phase::Running;
                        guard.refcount = 1;
                        self.cv.notify_all();
                        return Ok(());
                    } else {
                        guard.phase = Phase::Failed;
                        self.cv.notify_all();
                        return Err(Error::InitializationFailed(
                            "NDIlib_initialize failed".into(),
                        ));
                    }
                }

                Phase::Initializing | Phase::Destroying => {
                    // Wait for state transition to complete
                    guard = Self::recover_guard(self.cv.wait(guard));
                    // Loop again to check the new state
                }

                Phase::Running => {
                    // Runtime is already initialized, just increment refcount
                    guard.refcount += 1;
                    return Ok(());
                }
            }
        }
    }

    fn release(&self) {
        let mut guard = Self::recover_guard(self.state.lock());

        debug_assert!(
            guard.refcount > 0,
            "release() called when refcount was already 0 (double-free or unbalanced release)"
        );
        debug_assert!(
            guard.phase == Phase::Running,
            "release() called when phase was {:?}, expected Running",
            guard.phase
        );

        guard.refcount -= 1;

        if guard.refcount == 0 {
            // Last reference - destroy the runtime
            guard.phase = Phase::Destroying;
            drop(guard);

            // Call NDIlib_destroy outside the lock
            unsafe { NDIlib_destroy() };

            // Re-acquire lock to reset state
            let mut guard = Self::recover_guard(self.state.lock());
            guard.phase = Phase::Uninitialized;
            self.cv.notify_all();
        }
    }

    fn is_running(&self) -> bool {
        let guard = Self::recover_guard(self.state.lock());
        guard.phase == Phase::Running && guard.refcount > 0
    }
}

static RUNTIME: Lazy<RuntimeManager> = Lazy::new(RuntimeManager::new);

/// Manages the NDI runtime lifecycle.
///
/// The `NDI` struct is the entry point for all NDI operations. It ensures the NDI
/// runtime is properly initialized and cleaned up. Multiple instances can exist
/// simultaneously - they share the same underlying runtime through reference counting.
///
/// # Examples
///
/// ```no_run
/// use grafton_ndi::NDI;
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// // Create an NDI instance
/// let ndi = NDI::new()?;
///
/// // The runtime stays alive as long as any NDI instance exists
/// let ndi2 = ndi.clone(); // Cheap reference-counted clone
///
/// // Runtime is automatically cleaned up when all instances are dropped
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
pub struct NDI;

impl NDI {
    /// Creates a new NDI instance.
    ///
    /// This method is the single entry point for creating NDI instances. It is thread-safe
    /// and can be called from multiple threads. The first call initializes the NDI runtime,
    /// subsequent calls increment a reference count. When the last instance is dropped, the
    /// runtime is automatically destroyed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InitializationFailed`] if the NDI SDK fails to initialize.
    ///
    /// # Examples
    ///
    /// ```
    /// # use grafton_ndi::NDI;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// // Use NDI operations...
    /// # Ok(())
    /// # }
    /// ```
    pub fn new() -> Result<Self> {
        RUNTIME.acquire()?;
        Ok(Self)
    }

    /// Checks if the current CPU is supported by the NDI SDK.
    ///
    /// The NDI SDK requires certain CPU features (e.g., SSE4.2 on x86_64).
    ///
    /// # Examples
    ///
    /// ```
    /// if grafton_ndi::NDI::is_supported_cpu() {
    ///     println!("CPU is supported by NDI");
    /// } else {
    ///     eprintln!("CPU lacks required features for NDI");
    /// }
    /// ```
    pub fn is_supported_cpu() -> bool {
        unsafe { NDIlib_is_supported_CPU() }
    }

    /// Returns the version string of the NDI runtime.
    ///
    /// # Errors
    ///
    /// Returns an error if the version string cannot be retrieved or contains
    /// invalid UTF-8.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::NDI;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// match NDI::version() {
    ///     Ok(version) => println!("NDI version: {}", version),
    ///     Err(e) => eprintln!("Failed to get version: {}", e),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn version() -> Result<String> {
        unsafe {
            let version_ptr = NDIlib_version();
            if version_ptr.is_null() {
                return Err(Error::NullPointer("NDIlib_version".into()));
            }
            let c_str = std::ffi::CStr::from_ptr(version_ptr);
            c_str
                .to_str()
                .map(|s| s.to_owned())
                .map_err(|e| Error::InvalidUtf8(e.to_string()))
        }
    }
    /// Checks if the NDI runtime is currently initialized.
    ///
    /// This can be useful for diagnostic purposes or conditional initialization.
    ///
    /// # Examples
    ///
    /// ```
    /// if grafton_ndi::NDI::is_running() {
    ///     println!("NDI runtime is active");
    /// }
    /// ```
    pub fn is_running() -> bool {
        RUNTIME.is_running()
    }
}

impl Clone for NDI {
    fn clone(&self) -> Self {
        RUNTIME
            .acquire()
            .expect("Runtime should be initialized when cloning existing NDI handle");
        Self
    }
}

impl Drop for NDI {
    fn drop(&mut self) {
        RUNTIME.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    /// A testable runtime manager that uses mock init/destroy functions.
    /// This allows testing lifecycle invariants without the real NDI SDK.
    struct TestableRuntimeManager {
        state: Mutex<RuntimeState>,
        cv: Condvar,
        init_count: AtomicUsize,
        destroy_count: AtomicUsize,
        init_should_fail: AtomicBool,
        init_delay_ms: AtomicUsize,
        destroy_delay_ms: AtomicUsize,
    }

    impl TestableRuntimeManager {
        fn new() -> Self {
            Self {
                state: Mutex::new(RuntimeState::new()),
                cv: Condvar::new(),
                init_count: AtomicUsize::new(0),
                destroy_count: AtomicUsize::new(0),
                init_should_fail: AtomicBool::new(false),
                init_delay_ms: AtomicUsize::new(0),
                destroy_delay_ms: AtomicUsize::new(0),
            }
        }

        fn recover_guard<'a>(
            result: std::sync::LockResult<MutexGuard<'a, RuntimeState>>,
        ) -> MutexGuard<'a, RuntimeState> {
            result.unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        fn mock_initialize(&self) -> bool {
            let delay = self.init_delay_ms.load(Ordering::Acquire);
            if delay > 0 {
                thread::sleep(Duration::from_millis(delay as u64));
            }
            self.init_count.fetch_add(1, Ordering::AcqRel);
            !self.init_should_fail.load(Ordering::Acquire)
        }

        fn mock_destroy(&self) {
            let delay = self.destroy_delay_ms.load(Ordering::Acquire);
            if delay > 0 {
                thread::sleep(Duration::from_millis(delay as u64));
            }
            self.destroy_count.fetch_add(1, Ordering::AcqRel);
        }

        fn acquire(&self) -> Result<()> {
            let mut guard = Self::recover_guard(self.state.lock());

            loop {
                match guard.phase {
                    Phase::Uninitialized | Phase::Failed => {
                        guard.phase = Phase::Initializing;
                        drop(guard);

                        let succeeded = self.mock_initialize();

                        guard = Self::recover_guard(self.state.lock());

                        if succeeded {
                            guard.phase = Phase::Running;
                            guard.refcount = 1;
                            self.cv.notify_all();
                            return Ok(());
                        } else {
                            guard.phase = Phase::Failed;
                            self.cv.notify_all();
                            return Err(Error::InitializationFailed(
                                "Mock NDIlib_initialize failed".into(),
                            ));
                        }
                    }

                    Phase::Initializing | Phase::Destroying => {
                        guard = Self::recover_guard(self.cv.wait(guard));
                    }

                    Phase::Running => {
                        guard.refcount += 1;
                        return Ok(());
                    }
                }
            }
        }

        fn release(&self) {
            let mut guard = Self::recover_guard(self.state.lock());

            assert!(guard.refcount > 0, "release() called with refcount 0");
            assert!(
                guard.phase == Phase::Running,
                "release() called in phase {:?}",
                guard.phase
            );

            guard.refcount -= 1;

            if guard.refcount == 0 {
                guard.phase = Phase::Destroying;
                drop(guard);

                self.mock_destroy();

                let mut guard = Self::recover_guard(self.state.lock());
                guard.phase = Phase::Uninitialized;
                self.cv.notify_all();
            }
        }

        fn is_running(&self) -> bool {
            let guard = Self::recover_guard(self.state.lock());
            guard.phase == Phase::Running && guard.refcount > 0
        }

        fn phase(&self) -> Phase {
            let guard = Self::recover_guard(self.state.lock());
            guard.phase
        }

        fn refcount(&self) -> usize {
            let guard = Self::recover_guard(self.state.lock());
            guard.refcount
        }
    }

    // ========== Lifecycle Invariant Tests ==========

    #[test]
    fn test_reinit_after_teardown() {
        // Issue requirement: create NDI, drop all, create NDI again
        // => init called twice, destroy called twice, both NDI::new() return Ok
        let manager = Arc::new(TestableRuntimeManager::new());

        // First cycle
        manager.acquire().expect("First init should succeed");
        assert_eq!(manager.init_count.load(Ordering::Acquire), 1);
        assert!(manager.is_running());

        manager.release();
        assert_eq!(manager.destroy_count.load(Ordering::Acquire), 1);
        assert!(!manager.is_running());
        assert_eq!(manager.phase(), Phase::Uninitialized);

        // Second cycle - must re-initialize
        manager.acquire().expect("Second init should succeed");
        assert_eq!(manager.init_count.load(Ordering::Acquire), 2);
        assert!(manager.is_running());

        manager.release();
        assert_eq!(manager.destroy_count.load(Ordering::Acquire), 2);
        assert!(!manager.is_running());
    }

    #[test]
    fn test_init_failure_retry() {
        // Issue requirement: first init fails => error, next call succeeds => Ok
        let manager = Arc::new(TestableRuntimeManager::new());

        // Configure first init to fail
        manager.init_should_fail.store(true, Ordering::Release);

        let result = manager.acquire();
        assert!(result.is_err());
        assert_eq!(manager.init_count.load(Ordering::Acquire), 1);
        assert_eq!(manager.phase(), Phase::Failed);
        assert!(!manager.is_running());

        // Configure next init to succeed
        manager.init_should_fail.store(false, Ordering::Release);

        let result = manager.acquire();
        assert!(result.is_ok());
        assert_eq!(manager.init_count.load(Ordering::Acquire), 2);
        assert!(manager.is_running());

        manager.release();
    }

    #[test]
    fn test_no_ok_while_uninitialized() {
        // Issue requirement: cannot get Ok from acquire without init in that cycle
        let manager = Arc::new(TestableRuntimeManager::new());

        // Verify initial state
        assert_eq!(manager.phase(), Phase::Uninitialized);
        assert!(!manager.is_running());
        assert_eq!(manager.init_count.load(Ordering::Acquire), 0);

        // Acquire must call init
        manager.acquire().expect("Should succeed");
        assert_eq!(manager.init_count.load(Ordering::Acquire), 1);

        // Clone/acquire with running does NOT call init
        manager.acquire().expect("Clone should succeed");
        assert_eq!(manager.init_count.load(Ordering::Acquire), 1);
        assert_eq!(manager.refcount(), 2);

        // Release both
        manager.release();
        manager.release();

        // After teardown, next acquire MUST call init
        assert_eq!(manager.phase(), Phase::Uninitialized);
        manager.acquire().expect("Re-init should succeed");
        assert_eq!(manager.init_count.load(Ordering::Acquire), 2);

        manager.release();
    }

    #[test]
    fn test_destroy_called_exactly_once_per_cycle() {
        // Issue requirement: destroy called exactly once per successful init cycle
        let manager = Arc::new(TestableRuntimeManager::new());

        // First cycle with multiple refs
        manager.acquire().expect("Init");
        manager.acquire().expect("Clone 1");
        manager.acquire().expect("Clone 2");
        assert_eq!(manager.refcount(), 3);
        assert_eq!(manager.destroy_count.load(Ordering::Acquire), 0);

        manager.release();
        assert_eq!(manager.destroy_count.load(Ordering::Acquire), 0);
        manager.release();
        assert_eq!(manager.destroy_count.load(Ordering::Acquire), 0);
        manager.release(); // Last one
        assert_eq!(manager.destroy_count.load(Ordering::Acquire), 1);

        // Second cycle
        manager.acquire().expect("Re-init");
        manager.acquire().expect("Clone");
        assert_eq!(manager.destroy_count.load(Ordering::Acquire), 1);

        manager.release();
        manager.release();
        assert_eq!(manager.destroy_count.load(Ordering::Acquire), 2);
    }

    // ========== Concurrency Stress Tests ==========

    #[test]
    fn test_concurrent_acquire_single_init() {
        // Issue requirement: concurrent NDI::new() calls result in at most one init per cycle
        let manager = Arc::new(TestableRuntimeManager::new());

        // Add a small delay to init to increase chance of race
        manager.init_delay_ms.store(5, Ordering::Release);

        let handles: Vec<_> = (0..10)
            .map(|_| {
                let mgr = Arc::clone(&manager);
                thread::spawn(move || mgr.acquire())
            })
            .collect();

        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // All should succeed
        for result in &results {
            assert!(result.is_ok());
        }

        // Only one init call should have happened
        assert_eq!(manager.init_count.load(Ordering::Acquire), 1);
        assert_eq!(manager.refcount(), 10);

        // Cleanup
        for _ in 0..10 {
            manager.release();
        }
    }

    #[test]
    fn test_concurrent_acquire_during_destroy() {
        // Issue requirement: callers block during destroy, then succeed after
        let manager = Arc::new(TestableRuntimeManager::new());

        // Initialize
        manager.acquire().expect("Init");
        assert!(manager.is_running());

        // Add delay to destroy
        manager.destroy_delay_ms.store(50, Ordering::Release);

        let mgr_clone = Arc::clone(&manager);

        // Start a thread that will try to acquire while destroy is in progress
        let acquirer = thread::spawn(move || {
            // Wait a bit to ensure destroy starts
            thread::sleep(Duration::from_millis(10));
            mgr_clone.acquire()
        });

        // Trigger destroy
        manager.release();

        // The acquirer should have blocked and then succeeded
        let result = acquirer.join().unwrap();
        assert!(result.is_ok());

        // A new init cycle should have started
        assert_eq!(manager.init_count.load(Ordering::Acquire), 2);
        assert_eq!(manager.destroy_count.load(Ordering::Acquire), 1);

        manager.release();
    }

    #[test]
    fn test_mixed_acquire_release_concurrent() {
        // Stress test with mixed operations
        let manager = Arc::new(TestableRuntimeManager::new());
        let success_count = Arc::new(AtomicUsize::new(0));
        let failure_count = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..20)
            .map(|i| {
                let mgr = Arc::clone(&manager);
                let success = Arc::clone(&success_count);
                let failure = Arc::clone(&failure_count);

                thread::spawn(move || {
                    for _ in 0..10 {
                        match mgr.acquire() {
                            Ok(()) => {
                                success.fetch_add(1, Ordering::Relaxed);
                                // Hold for a bit
                                if i % 3 == 0 {
                                    thread::sleep(Duration::from_micros(100));
                                }
                                mgr.release();
                            }
                            Err(_) => {
                                failure.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("Thread panicked");
        }

        // All should succeed (no failures configured)
        assert_eq!(success_count.load(Ordering::Relaxed), 200);
        assert_eq!(failure_count.load(Ordering::Relaxed), 0);

        // Final state should be clean
        assert!(!manager.is_running());
        assert_eq!(manager.refcount(), 0);
        assert_eq!(manager.phase(), Phase::Uninitialized);
    }

    #[test]
    fn test_concurrent_init_with_failures() {
        // Test that failures during concurrent init are handled correctly
        let manager = Arc::new(TestableRuntimeManager::new());
        manager.init_should_fail.store(true, Ordering::Release);
        manager.init_delay_ms.store(5, Ordering::Release);

        let handles: Vec<_> = (0..5)
            .map(|_| {
                let mgr = Arc::clone(&manager);
                thread::spawn(move || mgr.acquire())
            })
            .collect();

        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        // All should fail
        for result in &results {
            assert!(result.is_err());
        }

        // State should be Failed
        assert_eq!(manager.phase(), Phase::Failed);
        assert!(!manager.is_running());

        // Now allow init to succeed
        manager.init_should_fail.store(false, Ordering::Release);

        // Retry should work
        manager.acquire().expect("Retry should succeed");
        assert!(manager.is_running());

        manager.release();
    }

    #[test]
    fn test_source_cache_pattern() {
        // Test the pattern used by SourceCache: create, cache, clear, recreate
        let manager = Arc::new(TestableRuntimeManager::new());

        // Simulate SourceCache.find_by_host() - creates NDI, caches it
        manager.acquire().expect("First source lookup");
        assert_eq!(manager.init_count.load(Ordering::Acquire), 1);

        // Simulate multiple cached sources
        manager.acquire().expect("Second source");
        manager.acquire().expect("Third source");
        assert_eq!(manager.refcount(), 3);

        // Simulate SourceCache.clear() - drops all cached NDI handles
        manager.release();
        manager.release();
        manager.release();

        // Runtime should be destroyed
        assert_eq!(manager.destroy_count.load(Ordering::Acquire), 1);
        assert!(!manager.is_running());

        // Simulate new find_by_host() after clear - must reinit
        manager.acquire().expect("New lookup after clear");
        assert_eq!(manager.init_count.load(Ordering::Acquire), 2);
        assert!(manager.is_running());

        manager.release();
    }
}
