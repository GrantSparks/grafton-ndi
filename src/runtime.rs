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

// Private SDK hooks let tests exercise the production state machine.
trait RuntimeBackend {
    fn initialize(&self) -> bool;
    fn destroy(&self);
}

struct SdkBackend;

impl RuntimeBackend for SdkBackend {
    fn initialize(&self) -> bool {
        unsafe { NDIlib_initialize() }
    }

    fn destroy(&self) {
        unsafe { NDIlib_destroy() };
    }
}

/// Process-global runtime manager for NDI.
///
/// This implementation uses a `Mutex` + `Condvar` state machine that:
/// - Allows re-initialization after teardown
/// - Allows retry after initialization failure
/// - Avoids spin loops by using `Condvar` waits
/// - Maintains the invariant: `NDI::new()` returns `Ok` only when runtime is initialized
struct RuntimeManager<B = SdkBackend> {
    state: Mutex<RuntimeState>,
    cv: Condvar,
    backend: B,
}

impl RuntimeManager {
    const fn new() -> Self {
        Self::with_backend(SdkBackend)
    }
}

impl<B: RuntimeBackend> RuntimeManager<B> {
    const fn with_backend(backend: B) -> Self {
        Self {
            state: Mutex::new(RuntimeState::new()),
            cv: Condvar::new(),
            backend,
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

                    let succeeded = self.backend.initialize();

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
                    guard.refcount = guard.refcount.checked_add(1).ok_or_else(|| {
                        Error::InitializationFailed("NDI runtime reference count exhausted".into())
                    })?;
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
            self.backend.destroy();

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
/// Create handles with [`NDI::new`] or [`Clone::clone`]; direct construction is private
/// so every handle owns a reference to the initialized runtime.
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
pub struct NDI {
    _private: (),
}

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
    /// Returns [`Error::InitializationFailed`] if the NDI SDK fails to initialize
    /// or the process has exhausted the runtime handle reference count.
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
        Ok(Self { _private: () })
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
    /// Acquires another reference to the active runtime.
    ///
    /// # Panics
    ///
    /// Panics if the runtime handle reference count is exhausted. The original
    /// handle and runtime remain valid.
    fn clone(&self) -> Self {
        RUNTIME
            .acquire()
            .expect("Unable to acquire another reference to the existing NDI runtime");
        Self { _private: () }
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

    #[derive(Default)]
    struct MockBackend {
        init_count: AtomicUsize,
        destroy_count: AtomicUsize,
        init_should_fail: AtomicBool,
        init_delay_ms: AtomicUsize,
        destroy_delay_ms: AtomicUsize,
    }

    impl RuntimeBackend for MockBackend {
        fn initialize(&self) -> bool {
            thread::sleep(Duration::from_millis(
                self.init_delay_ms.load(Ordering::Acquire) as u64,
            ));
            self.init_count.fetch_add(1, Ordering::AcqRel);
            !self.init_should_fail.load(Ordering::Acquire)
        }

        fn destroy(&self) {
            thread::sleep(Duration::from_millis(
                self.destroy_delay_ms.load(Ordering::Acquire) as u64,
            ));
            self.destroy_count.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl RuntimeManager<MockBackend> {
        fn phase(&self) -> Phase {
            Self::recover_guard(self.state.lock()).phase
        }

        fn refcount(&self) -> usize {
            Self::recover_guard(self.state.lock()).refcount
        }
    }

    #[test]
    fn refcount_overflow_preserves_live_runtime() {
        let manager = RuntimeManager::with_backend(MockBackend::default());
        manager.acquire().unwrap();
        manager.state.lock().unwrap().refcount = usize::MAX;
        assert!(manager.acquire().is_err());
        assert_eq!(manager.refcount(), usize::MAX);
        assert!(manager.is_running());
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 0);
        // Restore the injected count so the acquired reference can be released.
        manager.state.lock().unwrap().refcount = 1;
        manager.release();
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 1);
    }

    // ========== Lifecycle Invariant Tests ==========

    #[test]
    fn test_reinit_after_teardown() {
        // Issue requirement: create NDI, drop all, create NDI again
        // => init called twice, destroy called twice, both NDI::new() return Ok
        let manager = Arc::new(RuntimeManager::with_backend(MockBackend::default()));

        // First cycle
        manager.acquire().expect("First init should succeed");
        assert_eq!(manager.backend.init_count.load(Ordering::Acquire), 1);
        assert!(manager.is_running());

        manager.release();
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 1);
        assert!(!manager.is_running());
        assert_eq!(manager.phase(), Phase::Uninitialized);

        // Second cycle - must re-initialize
        manager.acquire().expect("Second init should succeed");
        assert_eq!(manager.backend.init_count.load(Ordering::Acquire), 2);
        assert!(manager.is_running());

        manager.release();
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 2);
        assert!(!manager.is_running());
    }

    #[test]
    fn test_init_failure_retry() {
        // Issue requirement: first init fails => error, next call succeeds => Ok
        let manager = Arc::new(RuntimeManager::with_backend(MockBackend::default()));

        // Configure first init to fail
        manager
            .backend
            .init_should_fail
            .store(true, Ordering::Release);

        let result = manager.acquire();
        assert!(result.is_err());
        assert_eq!(manager.backend.init_count.load(Ordering::Acquire), 1);
        assert_eq!(manager.phase(), Phase::Failed);
        assert!(!manager.is_running());

        // Configure next init to succeed
        manager
            .backend
            .init_should_fail
            .store(false, Ordering::Release);

        let result = manager.acquire();
        assert!(result.is_ok());
        assert_eq!(manager.backend.init_count.load(Ordering::Acquire), 2);
        assert!(manager.is_running());

        manager.release();
    }

    #[test]
    fn test_no_ok_while_uninitialized() {
        // Issue requirement: cannot get Ok from acquire without init in that cycle
        let manager = Arc::new(RuntimeManager::with_backend(MockBackend::default()));

        // Verify initial state
        assert_eq!(manager.phase(), Phase::Uninitialized);
        assert!(!manager.is_running());
        assert_eq!(manager.backend.init_count.load(Ordering::Acquire), 0);

        // Acquire must call init
        manager.acquire().expect("Should succeed");
        assert_eq!(manager.backend.init_count.load(Ordering::Acquire), 1);

        // Clone/acquire with running does NOT call init
        manager.acquire().expect("Clone should succeed");
        assert_eq!(manager.backend.init_count.load(Ordering::Acquire), 1);
        assert_eq!(manager.refcount(), 2);

        // Release both
        manager.release();
        manager.release();

        // After teardown, next acquire MUST call init
        assert_eq!(manager.phase(), Phase::Uninitialized);
        manager.acquire().expect("Re-init should succeed");
        assert_eq!(manager.backend.init_count.load(Ordering::Acquire), 2);

        manager.release();
    }

    #[test]
    fn test_destroy_called_exactly_once_per_cycle() {
        // Issue requirement: destroy called exactly once per successful init cycle
        let manager = Arc::new(RuntimeManager::with_backend(MockBackend::default()));

        // First cycle with multiple refs
        manager.acquire().expect("Init");
        manager.acquire().expect("Clone 1");
        manager.acquire().expect("Clone 2");
        assert_eq!(manager.refcount(), 3);
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 0);

        manager.release();
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 0);
        manager.release();
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 0);
        manager.release(); // Last one
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 1);

        // Second cycle
        manager.acquire().expect("Re-init");
        manager.acquire().expect("Clone");
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 1);

        manager.release();
        manager.release();
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 2);
    }

    // ========== Concurrency Stress Tests ==========

    #[test]
    fn test_concurrent_acquire_single_init() {
        // Issue requirement: concurrent NDI::new() calls result in at most one init per cycle
        let manager = Arc::new(RuntimeManager::with_backend(MockBackend::default()));

        // Add a small delay to init to increase chance of race
        manager.backend.init_delay_ms.store(5, Ordering::Release);

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
        assert_eq!(manager.backend.init_count.load(Ordering::Acquire), 1);
        assert_eq!(manager.refcount(), 10);

        // Cleanup
        for _ in 0..10 {
            manager.release();
        }
    }

    #[test]
    fn test_concurrent_acquire_during_destroy() {
        // Issue requirement: callers block during destroy, then succeed after
        let manager = Arc::new(RuntimeManager::with_backend(MockBackend::default()));

        // Initialize
        manager.acquire().expect("Init");
        assert!(manager.is_running());

        // Add delay to destroy
        manager
            .backend
            .destroy_delay_ms
            .store(50, Ordering::Release);

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
        assert_eq!(manager.backend.init_count.load(Ordering::Acquire), 2);
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 1);

        manager.release();
    }

    #[test]
    fn test_mixed_acquire_release_concurrent() {
        // Stress test with mixed operations
        let manager = Arc::new(RuntimeManager::with_backend(MockBackend::default()));
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
        let manager = Arc::new(RuntimeManager::with_backend(MockBackend::default()));
        manager
            .backend
            .init_should_fail
            .store(true, Ordering::Release);
        manager.backend.init_delay_ms.store(5, Ordering::Release);

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
        manager
            .backend
            .init_should_fail
            .store(false, Ordering::Release);

        // Retry should work
        manager.acquire().expect("Retry should succeed");
        assert!(manager.is_running());

        manager.release();
    }

    #[test]
    fn test_client_handles_can_be_released_and_recreated() {
        // Independent clients can share the runtime, release it, and start again.
        let manager = Arc::new(RuntimeManager::with_backend(MockBackend::default()));

        // First client initializes the runtime.
        manager.acquire().expect("First client");
        assert_eq!(manager.backend.init_count.load(Ordering::Acquire), 1);

        // Additional clients acquire independent references.
        manager.acquire().expect("Second client");
        manager.acquire().expect("Third client");
        assert_eq!(manager.refcount(), 3);

        // All clients release their handles.
        manager.release();
        manager.release();
        manager.release();

        // Runtime should be destroyed
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 1);
        assert!(!manager.is_running());

        // A later client must reinitialize the runtime.
        manager.acquire().expect("New client after teardown");
        assert_eq!(manager.backend.init_count.load(Ordering::Acquire), 2);
        assert!(manager.is_running());

        manager.release();
    }
}
