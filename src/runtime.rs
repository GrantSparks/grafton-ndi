//! NDI runtime management and initialization.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::{ndi_lib::*, Error, Result};

static INIT: AtomicBool = AtomicBool::new(false);
static INIT_FAILED: AtomicBool = AtomicBool::new(false);
static REFCOUNT: AtomicUsize = AtomicUsize::new(0);

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
    /// Creates a new NDI instance, initializing the runtime if necessary.
    ///
    /// This method is thread-safe and can be called from multiple threads. The first
    /// call initializes the NDI runtime, subsequent calls increment a reference count.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InitializationFailed`] if the NDI runtime cannot be initialized.
    /// This typically happens when the NDI SDK is not properly installed.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::NDI;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::acquire()?;
    /// // Use NDI operations...
    /// # Ok(())
    /// # }
    /// ```
    pub fn acquire() -> Result<Self> {
        // 1. Bump the counter immediately.
        let prev = REFCOUNT.fetch_add(1, Ordering::SeqCst);

        if prev == 0 {
            // We are the first handle → initialise the runtime.

            #[cfg(all(target_os = "windows", debug_assertions))]
            {
                if std::env::var("CI").is_ok() {
                    eprintln!("[NDI] Initializing NDI runtime in CI environment...");
                    if let Ok(sdk_dir) = std::env::var("NDI_SDK_DIR") {
                        eprintln!("[NDI] NDI_SDK_DIR: {}", sdk_dir);
                    }
                }
            }

            if !unsafe { NDIlib_initialize() } {
                // Roll the counter back and mark init as failed
                REFCOUNT.fetch_sub(1, Ordering::SeqCst);
                INIT_FAILED.store(true, Ordering::SeqCst);
                return Err(Error::InitializationFailed(
                    "NDIlib_initialize failed".into(),
                ));
            }
            INIT.store(true, Ordering::SeqCst);
        } else {
            // Someone else is (or was) doing the initialisation.
            // Check if it failed first
            if INIT_FAILED.load(Ordering::SeqCst) {
                REFCOUNT.fetch_sub(1, Ordering::SeqCst);
                return Err(Error::InitializationFailed(
                    "NDI initialization failed previously".into(),
                ));
            }
            // Wait until initialization is done so the caller never sees an
            // un-initialised runtime while REFCOUNT > 0.
            let mut spin_count = 0;
            let mut sleep_us = 1;
            const MAX_SPIN: u32 = 100;
            const MAX_SLEEP_US: u64 = 1000; // 1ms max

            let start = std::time::Instant::now();
            const INIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

            while !INIT.load(Ordering::SeqCst) && !INIT_FAILED.load(Ordering::SeqCst) {
                // Check for timeout
                if start.elapsed() > INIT_TIMEOUT {
                    REFCOUNT.fetch_sub(1, Ordering::SeqCst);
                    return Err(Error::InitializationFailed(
                        "NDI initialization timed out after 30 seconds".into(),
                    ));
                }

                if spin_count < MAX_SPIN {
                    // First, spin briefly for fast initialization
                    std::hint::spin_loop();
                    spin_count += 1;
                } else {
                    // Then use exponential backoff with sleep
                    std::thread::sleep(std::time::Duration::from_micros(sleep_us));
                    sleep_us = (sleep_us * 2).min(MAX_SLEEP_US);
                }
            }
            // Check again after waiting
            if INIT_FAILED.load(Ordering::SeqCst) {
                REFCOUNT.fetch_sub(1, Ordering::SeqCst);
                return Err(Error::InitializationFailed(
                    "NDI initialization failed previously".into(),
                ));
            }
        }

        Ok(Self)
    }

    /// Creates a new NDI instance.
    ///
    /// Alias for [`NDI::acquire()`].
    pub fn new() -> Result<Self> {
        Self::acquire()
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
        INIT.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Clone for NDI {
    fn clone(&self) -> Self {
        REFCOUNT.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for NDI {
    fn drop(&mut self) {
        // When the last handle vanishes, shut the runtime down.
        if REFCOUNT.fetch_sub(1, Ordering::SeqCst) == 1 {
            unsafe { NDIlib_destroy() };
            INIT.store(false, Ordering::SeqCst);
            INIT_FAILED.store(false, Ordering::SeqCst);
        }
    }
}
