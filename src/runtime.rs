//! Select an NDI implementation and manage its initialization lifetime.
//!
//! Using NDI takes two steps, and each is explicit:
//!
//! 1. **Select.** [`Runtime::select`] loads the NDI runtime library and pins it
//!    for the rest of the process. Nothing is started. The [`Runtime`] it
//!    returns describes the library: where it was loaded from, its edition and
//!    version ([`Runtime::info`]), what it can do ([`Runtime::capabilities`])
//!    and whether it can run on this CPU ([`Runtime::is_supported_cpu`]).
//! 2. **Initialize.** [`Runtime::initialize`] starts NDI and returns an [`NDI`]
//!    handle, from which every finder, sender and receiver is created. NDI
//!    stays started while any handle exists, and stops when the last one is
//!    dropped unless [`NDI::process_lifetime`] was chosen.
//!
//! [`NDI::new`] performs both steps with the default options, and
//! [`NDI::with_options`] with the options it is given. Nothing else in the
//! crate selects or starts a runtime: every operation that needs NDI takes a
//! handle, and a question such as [`NDI::is_running`] only reports the current
//! state.
//!
//! ```no_run
//! use grafton_ndi::{Runtime, RuntimeOptions};
//!
//! # fn main() -> Result<(), grafton_ndi::Error> {
//! let runtime = Runtime::select(&RuntimeOptions::default())?;
//! println!("NDI {}", runtime.info().raw_version());
//! if !runtime.is_supported_cpu() {
//!     eprintln!("this CPU cannot run the selected NDI runtime");
//! }
//! let ndi = runtime.initialize()?;
//! # drop(ndi);
//! # Ok(())
//! # }
//! ```
//!
//! Building this crate needs no NDI SDK. A selected shared library stays
//! loaded for the life of the process, including after the last [`NDI`] handle
//! is dropped. Statically linked targets use the SDK archive linked at build
//! time, and selecting it loads nothing.
//!
//! # Where the runtime is loaded from
//!
//! Unless [`RuntimeOptions::library_path`] names an exact file, the first usable
//! library in this order is selected:
//!
//! 1. the directory containing the running executable;
//! 2. the directory named by the `NDI_RUNTIME_DIR_V6` environment variable;
//! 3. the platform loader's own search for the library name (`rpath`,
//!    `LD_LIBRARY_PATH`, `PATH` and the system library directories).
//!
//! A directory that does not contain the library is skipped. A library that
//! cannot be loaded, or that loads but is rejected because it lacks a required
//! entry point or is not the requested edition, is recorded and the search
//! moves on to the next location. When nothing is usable,
//! [`Error::RuntimeLoadFailed`] names every location tried and the reason for
//! each, including the operating system's own.
//!
//! One limit applies to a library that loads but is rejected: it stays loaded,
//! because a native library can never be unloaded safely. If the platform
//! loader's own search then matches the library name to that module, it
//! returns the module again instead of looking further, so no other library
//! of that name can be reached by name in this process. The error names the
//! rejected file. Remove or fix that file, or name the wanted library with
//! [`RuntimeOptions::library_path`].

use std::{
    fmt,
    path::PathBuf,
    sync::{Condvar, Mutex, MutexGuard, OnceLock},
};

use crate::{
    capabilities::{Capabilities, RuntimeFeature, RuntimeInfo, SdkEdition},
    sys, Error, Result,
};

/// Which NDI runtime to select; see [`Runtime::select`].
///
/// The default options search for the runtime library in the
/// [documented order](crate::runtime#where-the-runtime-is-loaded-from) and ask
/// for the edition chosen when the crate was built.
///
/// # Examples
///
/// ```no_run
/// use grafton_ndi::{Runtime, RuntimeOptions, SdkEdition};
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let options = RuntimeOptions::default()
///     .library_path("/opt/ndi/lib/libndi.so.6")
///     .edition(SdkEdition::Standard);
/// let runtime = Runtime::select(&options)?;
/// # let _ = runtime;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, Default)]
pub struct RuntimeOptions {
    pub(crate) library_path: Option<PathBuf>,
    pub(crate) edition: Option<SdkEdition>,
}

impl RuntimeOptions {
    /// Load exactly this absolute shared-library path.
    ///
    /// No other location is tried, so a failure to load it is reported rather
    /// than hidden by a fallback. A statically linked target has no library to
    /// load and rejects an explicit path.
    pub fn library_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.library_path = Some(path.into());
        self
    }

    /// Search for the runtime library of this SDK edition.
    ///
    /// Requesting [`SdkEdition::Advanced`] also requires the selected
    /// implementation to export the Advanced SDK entry points. Requesting
    /// [`SdkEdition::Standard`] never rejects an implementation for exporting
    /// more than the Standard SDK does, so an Advanced runtime meets it.
    /// Without a request, the edition chosen when the crate was built applies:
    /// `NDI_SDK_EDITION` if it was set, otherwise Advanced when an Advanced SDK
    /// was installed, otherwise Standard.
    ///
    /// A statically linked target has one implementation, the archive linked
    /// at build time, and the same rule decides whether it meets the request.
    pub fn edition(mut self, edition: SdkEdition) -> Self {
        self.edition = Some(edition);
        self
    }
}

/// The NDI implementation selected for this process.
///
/// A `Runtime` is a small `Copy` handle. Obtaining one loads and pins the NDI
/// runtime library but does not start NDI, so it can be inspected first:
/// [`info`](Self::info) for its path, edition and version,
/// [`capabilities`](Self::capabilities) for what it supports and
/// [`is_supported_cpu`](Self::is_supported_cpu) for whether it can run here.
/// [`initialize`](Self::initialize) then starts NDI.
///
/// A process has one selected implementation. Once [`select`](Self::select)
/// succeeds, every handle refers to it, and what the accessors return stays
/// valid until the process exits.
///
/// # Examples
///
/// ```no_run
/// use grafton_ndi::{Finder, FinderOptions, Runtime, RuntimeFeature, RuntimeOptions};
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let runtime = Runtime::select(&RuntimeOptions::default())?;
///
/// let info = runtime.info();
/// println!("{:?} {} from {:?}", info.edition(), info.raw_version(), info.library_path());
/// if !runtime.capabilities().supports(RuntimeFeature::Planar444Video) {
///     println!("this runtime predates the planar 4:4:4 formats");
/// }
///
/// let ndi = runtime.initialize()?;
/// let finder = Finder::new(&ndi, &FinderOptions::default())?;
/// # drop(finder);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Copy)]
pub struct Runtime {
    selected: &'static sys::Runtime,
}

impl Runtime {
    /// Select the NDI implementation `options` describe, without starting NDI.
    ///
    /// The first successful call loads the runtime library, checks that it
    /// exports every entry point this crate requires, and pins it for the rest
    /// of the process. Later calls return the same implementation, provided
    /// their options are met by it. A statically linked target selects the
    /// archive linked at build time.
    ///
    /// Loading a native library runs its initialization code: select only
    /// trusted SDK files.
    ///
    /// # Errors
    ///
    /// * [`Error::RuntimeLoadFailed`] when no usable library is found. Nothing
    ///   is selected and the call can be repeated, with other options if
    ///   wanted. The message names every location tried and the reason for
    ///   each.
    /// * [`Error::RuntimeConflict`] when an implementation is already selected
    ///   and does not meet `options`: they name a different library file, or
    ///   ask for the Advanced SDK when the selected implementation is not one.
    /// * [`Error::InvalidConfiguration`] when `options` name a relative
    ///   library path.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use grafton_ndi::{Error, Runtime, RuntimeOptions};
    ///
    /// match Runtime::select(&RuntimeOptions::default()) {
    ///     Ok(runtime) => println!("selected {:?}", runtime.info().library_path()),
    ///     Err(Error::RuntimeLoadFailed(tried)) => eprintln!("NDI is not installed: {tried}"),
    ///     Err(error) => eprintln!("{error}"),
    /// }
    /// ```
    pub fn select(options: &RuntimeOptions) -> Result<Self> {
        sys::select(options).map(|selected| Self { selected })
    }

    /// The selected implementation's identity: the file it was loaded from,
    /// its edition, how it is linked and the version it reports.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Runtime, RuntimeOptions};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let runtime = Runtime::select(&RuntimeOptions::default())?;
    /// println!("NDI version: {}", runtime.info().raw_version());
    /// # Ok(())
    /// # }
    /// ```
    pub fn info(&self) -> &'static RuntimeInfo {
        &self.selected.info
    }

    /// What the selected implementation supports, by optional capability.
    ///
    /// This is evidence about the library and this build, not about hardware
    /// or licenses.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Runtime, RuntimeFeature, RuntimeOptions};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let runtime = Runtime::select(&RuntimeOptions::default())?;
    /// let status = runtime.capabilities().status(RuntimeFeature::ReceiveDelay);
    /// println!("receive delay: {status:?}");
    /// # Ok(())
    /// # }
    /// ```
    pub fn capabilities(&self) -> &'static Capabilities {
        &self.selected.capabilities
    }

    /// Check that the selected implementation provides `feature`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] naming the capability and why
    /// the selected implementation or this build cannot provide it.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Runtime, RuntimeFeature, RuntimeOptions};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let runtime = Runtime::select(&RuntimeOptions::default())?;
    /// runtime.require_feature(RuntimeFeature::Routing)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn require_feature(&self, feature: RuntimeFeature) -> Result<()> {
        self.selected.require(feature)
    }

    /// Whether the selected implementation can run on this CPU.
    ///
    /// The implementation itself answers, because the instructions it needs
    /// depend on the SDK version and the platform. A runtime that does not
    /// start for this reason makes [`Runtime::initialize`] report
    /// [`Error::UnsupportedCpu`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Runtime, RuntimeOptions};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let runtime = Runtime::select(&RuntimeOptions::default())?;
    /// if !runtime.is_supported_cpu() {
    ///     eprintln!("this CPU lacks instructions the NDI runtime needs");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn is_supported_cpu(&self) -> bool {
        is_supported_cpu(&self.selected.api.core)
    }

    /// Start NDI and return a handle to it.
    ///
    /// NDI is started by the first handle and stays started while any handle
    /// exists; each further call, like [`NDI::clone`], only adds a handle.
    /// Dropping the last one stops NDI, after which this can be called again.
    /// The call is thread-safe.
    ///
    /// # Errors
    ///
    /// * [`Error::UnsupportedCpu`] when the implementation does not start and
    ///   reports that this CPU cannot run it.
    /// * [`Error::InitializationFailed`] when it does not start for any other
    ///   reason, or the count of handles is exhausted.
    ///
    /// A failed start can be retried.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Error, Runtime, RuntimeOptions};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let runtime = Runtime::select(&RuntimeOptions::default())?;
    /// match runtime.initialize() {
    ///     Ok(ndi) => println!("NDI is running: {ndi:?}"),
    ///     Err(Error::UnsupportedCpu) => eprintln!("NDI cannot run on this CPU"),
    ///     Err(error) => return Err(error),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn initialize(&self) -> Result<NDI> {
        lifecycle(self.selected).acquire()?;
        Ok(NDI { runtime: *self })
    }
}

impl fmt::Debug for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runtime")
            .field("info", self.info())
            .finish()
    }
}

/// An explicit promise that the selected NDI runtime stays initialized until exit.
///
/// Created by [`NDI::process_lifetime`]. The first creation retains one runtime
/// reference permanently. Dropping this token does not reverse that choice.
/// This is intended for native callbacks whose completion cannot be bounded by a
/// normal sender/receiver lifetime. It does not grant an SDK license entitlement.
#[derive(Debug, Clone, Copy)]
pub struct ProcessLifetimeRuntime {
    ndi: &'static NDI,
}

impl ProcessLifetimeRuntime {
    /// Borrow the permanently retained runtime handle.
    pub fn ndi(&self) -> &NDI {
        self.ndi
    }
}

static PROCESS_LIFETIME: OnceLock<NDI> = OnceLock::new();

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

/// The native calls the lifecycle needs. Tests substitute their own, so the
/// production state machine runs without an SDK.
trait RuntimeBackend {
    /// Start NDI; `false` when it does not start.
    fn initialize(&self) -> bool;
    /// Whether the implementation can run on this CPU.
    fn is_supported_cpu(&self) -> bool;
    /// Stop NDI.
    fn destroy(&self);
}

/// Whether the implementation behind `core` can run on this CPU.
fn is_supported_cpu(core: &sys::Core) -> bool {
    // SAFETY: the query takes no arguments and needs no initialization.
    unsafe { core.NDIlib_is_supported_CPU() }
}

/// Initialization through the selected implementation's own entry points.
struct SdkBackend(&'static sys::Core);

impl RuntimeBackend for SdkBackend {
    fn initialize(&self) -> bool {
        // SAFETY: the manager serializes initialization against destruction.
        unsafe { self.0.NDIlib_initialize() }
    }

    fn is_supported_cpu(&self) -> bool {
        is_supported_cpu(self.0)
    }

    fn destroy(&self) {
        // SAFETY: the manager calls this only after the last handle is released,
        // so no SDK object created through this initialization remains.
        unsafe { self.0.NDIlib_destroy() };
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

                    // The native calls run outside the lock.
                    let started = self.start();

                    // Re-acquire lock to update state
                    guard = Self::recover_guard(self.state.lock());

                    if started.is_ok() {
                        guard.phase = Phase::Running;
                        guard.refcount = 1;
                    } else {
                        guard.phase = Phase::Failed;
                    }
                    self.cv.notify_all();
                    return started;
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

    /// Start NDI, classifying a failure by what the implementation reports.
    ///
    /// The CPU is consulted only after a failed start: an implementation that
    /// starts is the authority on whether it can run.
    fn start(&self) -> Result<()> {
        if self.backend.initialize() {
            Ok(())
        } else if self.backend.is_supported_cpu() {
            Err(Error::InitializationFailed(
                "NDIlib_initialize returned false on a CPU the runtime reports as supported".into(),
            ))
        } else {
            Err(Error::UnsupportedCpu)
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

/// Initialization state of the one implementation a process can select.
static LIFECYCLE: OnceLock<RuntimeManager> = OnceLock::new();

fn lifecycle(runtime: &'static sys::Runtime) -> &'static RuntimeManager {
    LIFECYCLE.get_or_init(|| RuntimeManager::with_backend(SdkBackend(&runtime.api.core)))
}

/// A handle to the started NDI runtime.
///
/// Every finder, sender and receiver is created from an `NDI` handle, and NDI
/// stays started while any handle exists: those objects each retain one, and
/// [`Clone::clone`] and [`Runtime::initialize`] add more. Dropping the last
/// handle stops NDI but leaves the selected library loaded, so NDI can be
/// started again. [`NDI::process_lifetime`] keeps it started until the process
/// exits.
///
/// [`NDI::new`] selects the default runtime and starts it in one call. To
/// inspect a runtime before starting it, or to choose which one is loaded, use
/// [`Runtime::select`] and then [`Runtime::initialize`].
///
/// # Examples
///
/// ```no_run
/// use grafton_ndi::NDI;
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = NDI::new()?;
///
/// // NDI stays started for as long as any handle exists.
/// let another = ndi.clone();
/// drop(ndi);
/// assert!(NDI::is_running());
///
/// // Dropping the last handle stops it.
/// drop(another);
/// # Ok(())
/// # }
/// ```
pub struct NDI {
    runtime: Runtime,
}

impl fmt::Debug for NDI {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NDI").finish_non_exhaustive()
    }
}

impl NDI {
    /// Select the default NDI runtime and start it.
    ///
    /// This is [`Runtime::select`] with the default [`RuntimeOptions`]
    /// followed by [`Runtime::initialize`]. It is thread-safe: the first
    /// handle starts NDI and later calls add handles to the running runtime.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RuntimeLoadFailed`] if no usable runtime library is
    /// found in the [search order](crate::runtime#where-the-runtime-is-loaded-from),
    /// [`Error::UnsupportedCpu`] if the runtime does not start because this
    /// CPU cannot run it, and [`Error::InitializationFailed`] if it does not
    /// start for another reason or the count of handles is exhausted.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::NDI;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// // Use NDI operations...
    /// # Ok(())
    /// # }
    /// ```
    pub fn new() -> Result<Self> {
        Self::with_options(&RuntimeOptions::default())
    }

    /// Select the NDI runtime `options` describe and start it.
    ///
    /// This is [`Runtime::select`] followed by [`Runtime::initialize`].
    ///
    /// # Errors
    ///
    /// Returns the errors of those two steps. A failed selection or start can
    /// be retried; once selection has succeeded, every later call uses the
    /// selected runtime and reports [`Error::RuntimeConflict`] for options it
    /// does not meet.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use grafton_ndi::{RuntimeOptions, SdkEdition, NDI};
    ///
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::with_options(&RuntimeOptions::default().edition(SdkEdition::Advanced))?;
    /// # drop(ndi);
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_options(options: &RuntimeOptions) -> Result<Self> {
        Runtime::select(options)?.initialize()
    }

    /// The selected implementation this handle keeps started.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::NDI;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// // A second handle from the same runtime, without selecting again.
    /// let another = ndi.runtime().initialize()?;
    /// # drop(another);
    /// # Ok(())
    /// # }
    /// ```
    pub fn runtime(&self) -> Runtime {
        self.runtime
    }

    /// The selected implementation's identity; see [`Runtime::info`].
    pub fn info(&self) -> &'static RuntimeInfo {
        self.runtime.info()
    }

    /// What the selected implementation supports; see [`Runtime::capabilities`].
    pub fn capabilities(&self) -> &'static Capabilities {
        self.runtime.capabilities()
    }

    /// The selected implementation's entry points.
    pub(crate) fn api(&self) -> &'static sys::Api {
        &self.runtime.selected.api
    }

    /// Reject an unavailable operation before passing newer inputs to the SDK.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] naming the capability and why
    /// the selected runtime or this build cannot provide it.
    pub fn require_feature(&self, feature: RuntimeFeature) -> Result<()> {
        self.runtime.require_feature(feature)
    }

    /// The entry points of an optional capability, core or Advanced, or the
    /// error [`Self::require_feature`] reports for it.
    ///
    /// The table type alone selects the capability, so a table cannot be
    /// obtained without its gate or under another capability's name.
    pub(crate) fn table<T: sys::Table>(&self) -> Result<&'static T> {
        T::of(self.runtime.selected)
    }

    /// Retain an initialized runtime reference until process exit.
    ///
    /// This is an irreversible, process-wide opt-in. It does not change ordinary
    /// reference-counted teardown unless this method is explicitly called.
    pub fn process_lifetime(&self) -> ProcessLifetimeRuntime {
        ProcessLifetimeRuntime {
            ndi: PROCESS_LIFETIME.get_or_init(|| self.clone()),
        }
    }

    /// Whether NDI is started, by this or any other handle in the process.
    ///
    /// This only reads the current state: it never selects or loads a
    /// runtime, and is `false` before one has been selected.
    ///
    /// # Examples
    ///
    /// ```
    /// if grafton_ndi::NDI::is_running() {
    ///     println!("NDI runtime is active");
    /// }
    /// ```
    pub fn is_running() -> bool {
        LIFECYCLE.get().is_some_and(RuntimeManager::is_running)
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
        lifecycle(self.runtime.selected)
            .acquire()
            .expect("Unable to acquire another reference to the existing NDI runtime");
        Self {
            runtime: self.runtime,
        }
    }
}

impl Drop for NDI {
    fn drop(&mut self) {
        lifecycle(self.runtime.selected).release();
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
        cpu_query_count: AtomicUsize,
        init_should_fail: AtomicBool,
        cpu_unsupported: AtomicBool,
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

        fn is_supported_cpu(&self) -> bool {
            self.cpu_query_count.fetch_add(1, Ordering::AcqRel);
            !self.cpu_unsupported.load(Ordering::Acquire)
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

    #[test]
    fn a_failed_start_is_classified_by_what_the_cpu_supports() {
        let manager = RuntimeManager::with_backend(MockBackend::default());
        manager
            .backend
            .init_should_fail
            .store(true, Ordering::Release);

        // The implementation supports the CPU, so it failed for another reason.
        assert!(matches!(
            manager.acquire(),
            Err(Error::InitializationFailed(_))
        ));
        assert_eq!(manager.phase(), Phase::Failed);

        manager
            .backend
            .cpu_unsupported
            .store(true, Ordering::Release);
        assert!(matches!(manager.acquire(), Err(Error::UnsupportedCpu)));
        assert_eq!(manager.phase(), Phase::Failed);
        assert!(!manager.is_running());
        assert_eq!(manager.backend.init_count.load(Ordering::Acquire), 2);
        assert_eq!(manager.backend.cpu_query_count.load(Ordering::Acquire), 2);
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 0);

        // Either failure leaves the runtime ready for another attempt.
        manager
            .backend
            .init_should_fail
            .store(false, Ordering::Release);
        manager.acquire().unwrap();
        assert!(manager.is_running());
        manager.release();
    }

    #[test]
    fn a_runtime_that_starts_is_not_asked_about_the_cpu() {
        let manager = RuntimeManager::with_backend(MockBackend::default());
        // The start itself is the authority: a successful one is never
        // overruled by the separate CPU query.
        manager
            .backend
            .cpu_unsupported
            .store(true, Ordering::Release);
        manager.acquire().unwrap();
        assert!(manager.is_running());
        assert_eq!(manager.backend.cpu_query_count.load(Ordering::Acquire), 0);
        manager.release();
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
