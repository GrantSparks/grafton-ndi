//! NDI runtime management and initialization.

use once_cell::sync::Lazy;

use std::sync::{Condvar, Mutex};

use crate::{ndi_lib::*, Error, Result};

/// State of the NDI runtime lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Runtime has not been initialized yet.
    Uninitialized,
    /// Runtime is currently being initialized by another thread.
    Initializing,
    /// Runtime is initialized and active with the given reference count.
    Initialized { refcount: usize },
    /// Runtime is currently being destroyed.
    Destroying,
}

/// Process-global runtime manager for NDI.
struct RuntimeManager {
    state: Mutex<State>,
    cv: Condvar,
}

impl RuntimeManager {
    const fn new() -> Self {
        Self {
            state: Mutex::new(State::Uninitialized),
            cv: Condvar::new(),
        }
    }

    fn acquire(&self) -> Result<()> {
        let mut state = self.state.lock().unwrap();

        loop {
            match *state {
                State::Uninitialized => {
                    // We'll be the initializer
                    *state = State::Initializing;
                    drop(state); // Release lock before calling FFI

                    #[cfg(all(target_os = "windows", debug_assertions))]
                    {
                        if std::env::var("CI").is_ok() {
                            eprintln!("[NDI] Initializing NDI runtime in CI environment...");
                            if let Ok(sdk_dir) = std::env::var("NDI_SDK_DIR") {
                                eprintln!("[NDI] NDI_SDK_DIR: {}", sdk_dir);
                            }
                        }
                    }

                    let init_succeeded = unsafe { NDIlib_initialize() };

                    // Reacquire lock to update state
                    state = self.state.lock().unwrap();

                    if init_succeeded {
                        *state = State::Initialized { refcount: 1 };
                        self.cv.notify_all();
                        return Ok(());
                    } else {
                        *state = State::Uninitialized;
                        self.cv.notify_all();
                        return Err(Error::InitializationFailed(
                            "NDIlib_initialize failed".into(),
                        ));
                    }
                }
                State::Initializing | State::Destroying => {
                    // Wait for the state to change
                    state = self.cv.wait(state).unwrap();
                }
                State::Initialized { refcount } => {
                    *state = State::Initialized {
                        refcount: refcount + 1,
                    };
                    return Ok(());
                }
            }
        }
    }

    fn release(&self) {
        let mut state = self.state.lock().unwrap();

        match *state {
            State::Initialized { refcount } => {
                if refcount == 1 {
                    // We're the last reference, destroy the runtime
                    *state = State::Destroying;
                    drop(state); // Release lock before calling FFI

                    unsafe { NDIlib_destroy() };

                    // Reacquire lock to update state
                    state = self.state.lock().unwrap();
                    *state = State::Uninitialized;
                    self.cv.notify_all();
                } else {
                    *state = State::Initialized {
                        refcount: refcount - 1,
                    };
                }
            }
            _ => {
                // This should never happen in correct usage
                #[cfg(debug_assertions)]
                panic!("release() called in invalid state: {:?}", *state);
            }
        }
    }

    fn is_running(&self) -> bool {
        let state = self.state.lock().unwrap();
        matches!(*state, State::Initialized { .. })
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
