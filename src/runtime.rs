//! NDI runtime management and initialization.

use once_cell::sync::Lazy;

use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Once,
};

use crate::{ndi_lib::*, Error, Result};

/// Process-global runtime manager for NDI using lock-free atomics.
///
/// This implementation avoids mutex poisoning hazards by using:
/// - `AtomicUsize` for reference counting (0 = uninitialized, >0 = refcount)
/// - `std::sync::Once` for exactly-once initialization
/// - `AtomicBool` for destruction-in-progress signaling
struct RuntimeManager {
    /// One-shot initialization guard.
    init: Once,
    /// Reference count: 0 means uninitialized, positive values are live refcount.
    refcount: AtomicUsize,
    /// True while destruction is in progress (rare path).
    destroying: AtomicBool,
    /// Initialization result: true if NDIlib_initialize succeeded.
    init_succeeded: AtomicBool,
}

impl RuntimeManager {
    const fn new() -> Self {
        Self {
            init: Once::new(),
            refcount: AtomicUsize::new(0),
            destroying: AtomicBool::new(false),
            init_succeeded: AtomicBool::new(false),
        }
    }

    fn acquire(&self) -> Result<()> {
        // Spin-wait if destruction is in progress (rare path, only during final
        // release concurrent with new acquire)
        while self.destroying.load(Ordering::Acquire) {
            std::hint::spin_loop();
        }

        // Increment refcount atomically
        let prev = self.refcount.fetch_add(1, Ordering::AcqRel);

        if prev == 0 {
            // We're the first acquirer - need to initialize
            self.init.call_once(|| {
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
                self.init_succeeded.store(succeeded, Ordering::Release);
            });

            // Check if initialization succeeded
            if !self.init_succeeded.load(Ordering::Acquire) {
                // Initialization failed - decrement refcount and return error
                self.refcount.fetch_sub(1, Ordering::Release);
                return Err(Error::InitializationFailed(
                    "NDIlib_initialize failed".into(),
                ));
            }
        } else {
            // Not the first acquirer - wait for initialization to complete
            // This is necessary because another thread might be in call_once
            while !self.init.is_completed() {
                std::hint::spin_loop();
            }

            // Check if initialization succeeded
            if !self.init_succeeded.load(Ordering::Acquire) {
                // Initialization failed - decrement refcount and return error
                self.refcount.fetch_sub(1, Ordering::Release);
                return Err(Error::InitializationFailed(
                    "NDIlib_initialize failed".into(),
                ));
            }
        }

        Ok(())
    }

    fn release(&self) {
        let prev = self.refcount.fetch_sub(1, Ordering::AcqRel);

        debug_assert!(
            prev > 0,
            "release() called when refcount was already 0 (double-free or unbalanced release)"
        );

        if prev == 1 {
            // We were the last reference - destroy the runtime
            self.destroying.store(true, Ordering::Release);

            unsafe { NDIlib_destroy() };

            self.destroying.store(false, Ordering::Release);
        }
    }

    fn is_running(&self) -> bool {
        self.refcount.load(Ordering::Acquire) > 0
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
