//! Select an NDI implementation and manage its initialization lifetime.
//!
//! Using NDI takes two steps, and each is explicit:
//!
//! 1. **Select.** [`Runtime::select`] loads the NDI runtime library and pins it
//!    for the rest of the process. Nothing is started. The [`Runtime`] it
//!    returns describes the library: where it was loaded from, its edition and
//!    version ([`Runtime::info`]), what it can do ([`Runtime::capabilities`])
//!    and whether it can run on this CPU ([`Runtime::is_supported_cpu`]).
//! 2. **Initialize.** [`Runtime::initialize`] starts NDI and returns an [`Ndi`]
//!    handle, from which every finder, sender and receiver is created. NDI
//!    stays started while any handle exists, and stops when the last one is
//!    dropped unless [`Ndi::process_lifetime`] was chosen.
//!
//! [`Ndi::new`] performs both steps with the default options, and
//! [`Ndi::with_options`] with the options it is given. Nothing else in the
//! crate selects or starts a runtime: every operation that needs NDI takes a
//! handle, and a question such as [`Ndi::is_running`] only reports the current
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
//! Default builds on dynamically loaded targets need no NDI SDK. A selected shared library stays
//! loaded for the life of the process, including after the last [`Ndi`] handle
//! is dropped. Statically linked targets use the SDK archive linked at build
//! time, and selecting it loads nothing.
//!
//! # Where the runtime is loaded from
//!
//! **Deployment trust boundary:** [`Ndi::new`] uses a packaging-oriented
//! search. Every searched directory must be controlled by a trusted owner.
//! Loading a native library executes its constructors before its symbols or
//! edition can be checked, including libraries later rejected during fallback.
//! For services and elevated processes, make the very first selection use an
//! absolute path in an application-owner-controlled directory:
//!
//! ```no_run
//! # fn main() -> grafton_ndi::Result<()> {
//! use grafton_ndi::{Runtime, RuntimeOptions};
//! let runtime = Runtime::select(&RuntimeOptions::new()
//!     .library_path("/opt/my-service/ndi/libndi.so.6"))?;
//! println!("Selected runtime: {:?}", runtime.info().library_path());
//! let ndi = runtime.initialize()?;
//! # Ok(())
//! # }
//! ```
//!
//! An explicit path must be absolute and name an existing file. A relative
//! path returns [`Error::InvalidInput`]. An absent file returns an absent-path
//! refusal before calling the platform loader. The crate makes no
//! executable, environment or platform search for an explicit path. A later
//! explicit selection cannot undo code already run by an earlier load. Native
//! dependencies still use platform loader rules and must also come from
//! trusted locations. With the
//! File API, explicitly select `FileApiOptions::library_path`
//! too, or establish that its companion and other search directories are
//! trusted; inspect the selection through `FileApi::library_path`.
//!
//! Unless [`RuntimeOptions::library_path`] names an exact file, the first usable
//! library in this order is selected:
//!
//! 1. the directory containing the running executable;
//! 2. the absolute directory named by `NDI_RUNTIME_DIR_V6`; relative values
//!    are recorded as refused locations and never searched;
//! 3. platform-specific locations: on Windows, absolute `PATH` directories
//!    followed by a System32-only name lookup; on macOS, `@rpath/<name>` and
//!    absolute `DYLD_LIBRARY_PATH` directories; on Linux and Android, the
//!    native loader search (`rpath`, `LD_LIBRARY_PATH` and system locations).
//!    Explicitly configured relative or empty `LD_LIBRARY_PATH` entries can
//!    still name the working directory on Linux and Android.
//!
//! Windows and macOS never add the working directory to this search. Relative
//! or empty `PATH`/`DYLD_LIBRARY_PATH` entries are recorded as refused locations.
//! On macOS, a relative or empty entry in either `DYLD_LIBRARY_PATH` or
//! `DYLD_FALLBACK_LIBRARY_PATH` refuses the entire default search before opening
//! a library. The check reads the process environment at search time; dyld uses
//! its launch-time values, so changing `DYLD_*` after launch is unsupported.
//! On unrestricted macOS processes dyld can use `DYLD_LIBRARY_PATH` to
//! substitute an existing explicit file. Such substitutions are refused with
//! [`crate::LoadRefusal::ResolvedToAnotherFile`] after loading the candidate,
//! when its initializers have already run. Configure trusted absolute
//! `DYLD_*` entries before launching.
//!
//! Each location is tried for the file names of both SDK editions, for
//! example `libndi.so.6` and `libndi_advanced.so.6` on Linux, unless
//! [`RuntimeOptions::edition`] asks for one. A build with the `advanced`
//! feature, or with `file-io` on a platform where the File library needs the
//! Advanced runtime, tries the Advanced name first at each location, and other
//! builds the Standard name; where a library is installed matters more than
//! which edition it is. Nothing installed on the build machine changes the
//! search.
//!
//! A file that is not there is skipped. A library that cannot be loaded, or
//! that loads but is rejected because it lacks a required entry point or is
//! not the requested edition, is recorded and the search moves on. When
//! nothing is usable,
//! [`Error::RuntimeLoadFailed`] carries [`crate::LoadFailure`] with every
//! location, [`crate::LoadOrigin`] and typed [`crate::LoadRefusal`]. Its
//! `loaded_any()` distinguishes code that loaded and was refused from locations
//! that could not be opened; `Display` retains the diagnostic text, including
//! the operating system's own. [`Error::RuntimeConflict`] carries a
//! [`crate::SelectionFailure`] with a typed kind and selected, requested or
//! foreign module paths, alongside its diagnostic text.
//!
//! On dynamically loaded, non-Apple ELF targets, a runtime already linked or
//! preloaded into the process's global symbol scope must also be the selected
//! runtime. Other candidates are refused before opening them, because that
//! global runtime would capture their native calls. Search failures record
//! [`crate::LoadRefusal::ForeignRuntimeInScope`]; an explicit different file
//! returns [`crate::SelectionConflict::ForeignRuntimeInScope`]. Select the
//! module named in the diagnostic, or stop linking or preloading it.
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
    num::NonZeroUsize,
    path::PathBuf,
    sync::{Condvar, Mutex, MutexGuard, OnceLock},
};

use crate::{
    capabilities::{Capabilities, RuntimeFeature, RuntimeInfo, SdkEdition},
    sys, Error, Result,
};

/// Which NDI runtime to select; see [`Runtime::select`].
///
/// The default options search for the runtime library of either edition in
/// the [documented order](crate::runtime#where-the-runtime-is-loaded-from)
/// and accept whichever is found first.
/// On dynamically loaded, non-Apple ELF targets, any runtime already in the
/// process's global symbol scope must be the selected module; see
/// [`crate::LoadRefusal::ForeignRuntimeInScope`].
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct RuntimeOptions {
    pub(crate) library_path: Option<PathBuf>,
    pub(crate) edition: Option<SdkEdition>,
}

impl RuntimeOptions {
    /// Restore the normal library search instead of an explicit path.
    #[must_use]
    pub fn clear_library_path(mut self) -> Self {
        self.library_path = None;
        self
    }

    /// Accept either SDK edition instead of requiring one.
    #[must_use]
    pub fn clear_edition(mut self) -> Self {
        self.edition = None;
        self
    }

    /// Requested exact library path, or `None` for the normal search.
    #[must_use]
    pub fn configured_library_path(&self) -> Option<&std::path::Path> {
        self.library_path.as_deref()
    }

    /// Requested edition, or `None` to accept either edition.
    #[must_use]
    pub fn configured_edition(&self) -> Option<SdkEdition> {
        self.edition
    }

    /// The default options: the runtime library of either edition, from the
    /// [documented search](crate::runtime#where-the-runtime-is-loaded-from).
    pub fn new() -> Self {
        Self::default()
    }

    /// Load exactly this absolute shared-library path.
    ///
    /// A path that does not name an existing file is refused before native
    /// loading. A failure to load the file is reported without a fallback. A statically linked target has no library to
    /// load and rejects an absolute path with [`Error::RuntimeConflict`].
    /// A relative path returns [`Error::InvalidInput`] on every target,
    /// before an implementation is selected.
    /// On dynamically loaded, non-Apple ELF targets, a different runtime
    /// already linked or preloaded into the global symbol scope causes
    /// [`crate::SelectionConflict::ForeignRuntimeInScope`] before this file
    /// is opened. Aliases identifying the same loaded module are accepted.
    #[must_use]
    pub fn library_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.library_path = Some(path.into());
        self
    }

    /// Search only for the runtime library of this SDK edition.
    ///
    /// On dynamically loaded targets, requesting [`SdkEdition::Advanced`]
    /// requires the selected library to export the Advanced SDK entry points. Requesting
    /// [`SdkEdition::Standard`] never rejects an implementation for exporting
    /// more than the Standard SDK does, so an Advanced runtime found under the
    /// Standard file name, or named by [`library_path`](Self::library_path),
    /// meets it. Without a request, the libraries of both editions are sought.
    ///
    /// A statically linked target has one implementation, the archive linked
    /// at build time. An Advanced request is met only when the build linked the
    /// Advanced archive; a Standard request accepts either archive. See
    /// [`SdkEdition`] for how the build chooses the archive.
    #[must_use]
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
    ///   wanted. The [`crate::LoadFailure`] records each attempted location,
    ///   its origin, typed refusal and whether native code loaded.
    /// * [`Error::RuntimeConflict`] when an implementation is already selected
    ///   and does not meet `options`: they name a different library file, or
    ///   ask for the Advanced SDK when the selected implementation is not one.
    ///   Also returned for an explicit path on a linked target, or when native
    ///   loading re-enters selection on the same thread before selection finishes.
    ///   On dynamically loaded, non-Apple ELF targets, an explicit file is also
    ///   refused before opening when another runtime is in the global symbol scope.
    ///   [`crate::SelectionConflict`] identifies the cause.
    /// * [`Error::InvalidInput`] when `options` name a relative
    ///   library path.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use grafton_ndi::{Error, Runtime, RuntimeOptions};
    ///
    /// match Runtime::select(&RuntimeOptions::default()) {
    ///     Ok(runtime) => println!("selected {:?}", runtime.info().library_path()),
    ///     Err(Error::RuntimeLoadFailed(failure)) if !failure.loaded_any() => {
    ///         eprintln!("no runtime could be opened: {failure}");
    ///     }
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
        self.selected.gates.capabilities()
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
    #[doc(alias = "NDIlib_is_supported_CPU")]
    pub fn is_supported_cpu(&self) -> bool {
        is_supported_cpu(&self.selected.gates.api().core)
    }

    /// Start NDI and return a handle to it.
    ///
    /// NDI is started by the first handle and stays started while any handle
    /// exists; each further call, like [`Ndi::clone`], only adds a handle.
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
    #[doc(alias = "NDIlib_initialize")]
    pub fn initialize(&self) -> Result<Ndi> {
        self.selected.lifecycle.acquire()?;
        Ok(Ndi { runtime: *self })
    }
}

impl fmt::Debug for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runtime")
            .field("info", self.info())
            .finish_non_exhaustive()
    }
}

/// An explicit promise that the selected NDI runtime stays initialized until exit.
///
/// Created by [`Ndi::process_lifetime`]. The first creation retains one runtime
/// reference permanently. Dropping this token does not reverse that choice.
/// This is intended for native callbacks whose completion cannot be bounded by a
/// normal sender/receiver lifetime. It does not grant an SDK license entitlement.
#[derive(Clone, Copy)]
pub struct ProcessLifetimeRuntime {
    ndi: &'static Ndi,
}

impl fmt::Debug for ProcessLifetimeRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessLifetimeRuntime")
            .field("info", self.ndi.info())
            .finish_non_exhaustive()
    }
}

impl ProcessLifetimeRuntime {
    /// Borrow the permanently retained runtime handle.
    pub fn ndi(&self) -> &Ndi {
        self.ndi
    }
}

static PROCESS_LIFETIME: OnceLock<Ndi> = OnceLock::new();

/// Lifecycle state; only an active runtime can own live handle counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Stopped,
    Starting,
    Running(NonZeroUsize),
    Stopping,
}

/// The native calls the lifecycle needs. Tests substitute their own, so the
/// production state machine runs without an SDK.
pub(crate) trait RuntimeBackend {
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
pub(crate) struct SdkBackend {
    lifecycle: sys::Lifecycle,
    supported_cpu: unsafe extern "C" fn() -> bool,
}

impl RuntimeBackend for SdkBackend {
    fn initialize(&self) -> bool {
        // SAFETY: the manager serializes initialization against destruction.
        unsafe { self.lifecycle.NDIlib_initialize() }
    }

    fn is_supported_cpu(&self) -> bool {
        // SAFETY: this no-argument query needs no initialization.
        unsafe { (self.supported_cpu)() }
    }

    fn destroy(&self) {
        // SAFETY: the manager calls this only after the last handle is released,
        // so no SDK object created through this initialization remains.
        unsafe { self.lifecycle.NDIlib_destroy() };
    }
}

/// Process-global runtime manager for NDI.
///
/// This implementation uses a `Mutex` + `Condvar` state machine that:
/// - Allows re-initialization after teardown
/// - Allows retry after initialization failure
/// - Avoids spin loops by using `Condvar` waits
/// - Maintains the invariant: `Ndi::new()` returns `Ok` only when runtime is initialized
pub(crate) struct RuntimeManager<B = SdkBackend> {
    state: Mutex<Phase>,
    cv: Condvar,
    backend: B,
}

impl<B: RuntimeBackend> RuntimeManager<B> {
    const fn with_backend(backend: B) -> Self {
        Self {
            state: Mutex::new(Phase::Stopped),
            cv: Condvar::new(),
            backend,
        }
    }

    /// Recover from mutex poisoning, preferring progress over panic.
    fn recover_guard<'a>(
        result: std::sync::LockResult<MutexGuard<'a, Phase>>,
    ) -> MutexGuard<'a, Phase> {
        result.unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn acquire(&self) -> Result<()> {
        let mut guard = Self::recover_guard(self.state.lock());
        loop {
            match *guard {
                Phase::Stopped => {
                    *guard = Phase::Starting;
                    drop(guard);
                    // Native calls run outside the state lock.
                    let started = self.start();
                    guard = Self::recover_guard(self.state.lock());
                    *guard = if started.is_ok() {
                        Phase::Running(NonZeroUsize::MIN)
                    } else {
                        Phase::Stopped
                    };
                    self.cv.notify_all();
                    return started;
                }
                Phase::Starting | Phase::Stopping => {
                    guard = Self::recover_guard(self.cv.wait(guard));
                }
                Phase::Running(count) => {
                    let next = count.checked_add(1).ok_or(Error::InitializationFailed(
                        crate::InitializationFailure::HandleCountExhausted,
                    ))?;
                    *guard = Phase::Running(next);
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
                crate::InitializationFailure::NotStarted,
            ))
        } else {
            Err(Error::UnsupportedCpu)
        }
    }

    fn release(&self) {
        let mut guard = Self::recover_guard(self.state.lock());
        match *guard {
            Phase::Running(count) => {
                // NonZeroUsize proves the subtraction cannot underflow.
                if let Some(remaining) = NonZeroUsize::new(count.get() - 1) {
                    *guard = Phase::Running(remaining);
                    return;
                }
                *guard = Phase::Stopping;
                drop(guard);
                // The last handle stops NDI outside the state lock.
                self.backend.destroy();
                let mut guard = Self::recover_guard(self.state.lock());
                *guard = Phase::Stopped;
                self.cv.notify_all();
            }
            other => {
                debug_invariant!(false, "release() called while runtime was {other:?}");
            }
        }
    }

    pub(crate) fn is_running(&self) -> bool {
        matches!(*Self::recover_guard(self.state.lock()), Phase::Running(_))
    }
}

impl RuntimeManager {
    pub(crate) fn new(
        lifecycle: sys::Lifecycle,
        supported_cpu: unsafe extern "C" fn() -> bool,
    ) -> Self {
        Self::with_backend(SdkBackend {
            lifecycle,
            supported_cpu,
        })
    }
}

/// A handle to the started NDI runtime.
///
/// Every finder, sender and receiver is created from an `Ndi` handle, and NDI
/// stays started while any handle exists: those objects each retain one, and
/// [`Clone::clone`] and [`Runtime::initialize`] add more. Dropping the last
/// handle stops NDI but leaves the selected library loaded, so NDI can be
/// started again. [`Ndi::process_lifetime`] keeps it started until the process
/// exits.
///
/// [`Ndi::new`] selects the default runtime and starts it in one call. To
/// inspect a runtime before starting it, or to choose which one is loaded, use
/// [`Runtime::select`] and then [`Runtime::initialize`].
///
/// # Examples
///
/// ```no_run
/// use grafton_ndi::Ndi;
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = Ndi::new()?;
///
/// // NDI stays started for as long as any handle exists.
/// let another = ndi.clone();
/// drop(ndi);
/// assert!(Ndi::is_running());
///
/// // Dropping the last handle stops it.
/// drop(another);
/// # Ok(())
/// # }
/// ```
pub struct Ndi {
    runtime: Runtime,
}

/// The initialized runtime already retained by a native resource.
/// Binding an existing owner never acquires another runtime reference.
pub(crate) trait LiveOwner {
    fn ndi(&self) -> &Ndi;
}

impl LiveOwner for Ndi {
    fn ndi(&self) -> &Ndi {
        self
    }
}

impl LiveOwner for std::sync::Arc<crate::receiver::ReceiverCore> {
    fn ndi(&self) -> &Ndi {
        self.as_ref().ndi()
    }
}

/// How a native capability reaches its table through its retained owner.
/// The core table is already in `Ndi`, so it needs no second pointer.
/// Optional and File tables cache their gated pointer beside that owner.
pub(crate) trait BoundTable: 'static {
    type Storage: Copy;

    fn store(table: &'static Self) -> Self::Storage;
    fn borrow<'a>(table: &'a Self::Storage, ndi: &'a Ndi) -> &'a Self;
}

impl<T: sys::Table> BoundTable for T {
    type Storage = &'static T;

    fn store(table: &'static Self) -> Self::Storage {
        table
    }

    fn borrow<'a>(table: &'a Self::Storage, _: &'a Ndi) -> &'a Self {
        table
    }
}

impl BoundTable for sys::Core {
    type Storage = ();

    fn store(_: &'static Self) {}

    fn borrow<'a>(_: &'a (), ndi: &'a Ndi) -> &'a Self {
        &ndi.api().core
    }
}

#[cfg(feature = "file-io")]
macro_rules! file_bound_tables {
    ($($table:ty),+ $(,)?) => {$(
        impl BoundTable for $table {
            type Storage = &'static Self;

            fn store(table: &'static Self) -> Self::Storage {
                table
            }

            fn borrow<'a>(table: &'a Self::Storage, _: &'a Ndi) -> &'a Self {
                table
            }
        }
    )+};
}

#[cfg(feature = "file-io")]
file_bound_tables!(sys::FileRuntime, sys::FilePlayback, sys::FileRecording);

/// A native constructor's result and any configuration it may have retained.
pub(crate) struct NativeCreation<I> {
    pub(crate) call: sys::table::Called<I>,
    #[cfg(feature = "advanced")]
    pub(crate) config: crate::advanced::RetainedConfig,
}

impl<I> NativeCreation<I> {
    pub(crate) fn plain(call: sys::table::Called<I>) -> Self {
        Self {
            call,
            #[cfg(feature = "advanced")]
            config: crate::advanced::RetainedConfig::default(),
        }
    }
}

/// Native entry points paired with the initialized runtime they require.
/// Only runtime binding paths can construct this capability. Dereferencing it
/// borrows the table for no longer than the retained owner.
pub(crate) struct Bound<T: BoundTable, O: LiveOwner = Ndi> {
    table: T::Storage,
    owner: O,
}

impl<T: BoundTable, O: LiveOwner> Bound<T, O> {
    pub(crate) fn ndi(&self) -> &Ndi {
        self.owner().ndi()
    }

    pub(crate) fn owner(&self) -> &O {
        &self.owner
    }
}

impl<T: sys::Table, O: LiveOwner> Bound<T, O> {
    /// Gate through an existing live owner and move its proof into the table.
    #[allow(
        dead_code,
        reason = "existing-owner bindings are used by optional receiver adapters"
    )]
    pub(crate) fn with_owner(owner: O) -> Result<Self> {
        let table = T::of(owner.ndi().runtime.selected)?;
        Ok(Self {
            table: T::store(table),
            owner,
        })
    }
}

#[cfg(test)]
impl<T: BoundTable> Bound<T> {
    pub(crate) fn for_test(ndi: Ndi, table: &'static T) -> Self {
        Self {
            table: T::store(table),
            owner: ndi,
        }
    }
}

#[cfg(feature = "file-io")]
impl Bound<sys::FileRuntime> {
    pub(crate) fn into_file_table<T: sys::FileTable>(self) -> Result<Bound<T>> {
        let table = self.table.table::<T>()?;
        Ok(Bound {
            table: T::store(table),
            owner: self.owner,
        })
    }

    pub(crate) fn bind_file_table<T: sys::FileTable>(&self) -> Result<Bound<T>> {
        // Gate before cloning, so an unsupported family does not acquire a handle.
        let table = self.table.table::<T>()?;
        Ok(Bound {
            table: T::store(table),
            owner: self.owner.clone(),
        })
    }
}

impl<T: BoundTable, O: LiveOwner> std::ops::Deref for Bound<T, O> {
    type Target = T;

    fn deref(&self) -> &T {
        T::borrow(&self.table, self.owner.ndi())
    }
}

impl<T: BoundTable, O: LiveOwner + Clone> Clone for Bound<T, O> {
    fn clone(&self) -> Self {
        Self {
            table: self.table,
            owner: self.owner.clone(),
        }
    }
}

impl fmt::Debug for Ndi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ndi")
            .field("info", self.info())
            .finish_non_exhaustive()
    }
}

impl Ndi {
    #[cfg(test)]
    pub(crate) fn test_double(core: sys::Core) -> Self {
        Runtime {
            selected: sys::Runtime::test_double(core),
        }
        .initialize()
        .unwrap()
    }

    /// Select the default NDI runtime and start it.
    ///
    /// This is [`Runtime::select`] with the default [`RuntimeOptions`]
    /// followed by [`Runtime::initialize`]. It is thread-safe: the first
    /// handle starts NDI and later calls add handles to the running runtime.
    /// See the [search and trust policy](crate::runtime#where-the-runtime-is-loaded-from)
    /// before selecting a runtime in a service or elevated process.
    ///
    /// # Errors
    ///
    /// Returns [`Error::RuntimeLoadFailed`] if no usable runtime library is
    /// found in the [search order](crate::runtime#where-the-runtime-is-loaded-from).
    /// Its [`crate::LoadFailure`] distinguishes code that loaded and was refused
    /// from locations that could not be opened. Returns
    /// [`Error::RuntimeConflict`] if native loading re-enters runtime selection
    /// on the same thread. Returns [`Error::UnsupportedCpu`] if the runtime does not start because this
    /// CPU cannot run it, and [`Error::InitializationFailed`] if it does not
    /// start for another reason or the count of handles is exhausted.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::Ndi;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = Ndi::new()?;
    /// // Use NDI operations...
    /// # Ok(())
    /// # }
    /// ```
    #[doc(alias = "NDIlib_initialize")]
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
    /// does not meet, with a typed [`crate::SelectionConflict`]. A
    /// [`crate::LoadFailure`] retains the structured attempts when loading fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use grafton_ndi::{RuntimeOptions, SdkEdition, Ndi};
    ///
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = Ndi::with_options(&RuntimeOptions::default().edition(SdkEdition::Advanced))?;
    /// # drop(ndi);
    /// # Ok(())
    /// # }
    /// ```
    #[doc(alias = "NDIlib_initialize")]
    pub fn with_options(options: &RuntimeOptions) -> Result<Self> {
        Runtime::select(options)?.initialize()
    }

    /// The selected implementation this handle keeps started.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::Ndi;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = Ndi::new()?;
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

    #[cfg(feature = "file-io")]
    pub(crate) fn selected(&self) -> &sys::Runtime {
        self.runtime.selected
    }

    /// The selected implementation's entry points.
    pub(crate) fn api(&self) -> &sys::Api {
        self.runtime.selected.gates.api()
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
    /// obtained without its gate or be reported as another capability.
    pub(crate) fn table<T: sys::Table>(&self) -> Result<&T> {
        T::of(self.runtime.selected)
    }

    /// Check the capability and acquire its runtime before native creation.
    pub(crate) fn bind<T: sys::Table>(&self) -> Result<Bound<T>> {
        let table = T::of(self.runtime.selected)?;
        Ok(Bound {
            table: T::store(table),
            owner: self.clone(),
        })
    }

    pub(crate) fn bind_core(&self) -> Bound<sys::Core> {
        Bound {
            table: (),
            owner: self.clone(),
        }
    }

    #[cfg(feature = "file-io")]
    pub(crate) fn bind_file_runtime(
        &self,
        table: &'static sys::FileRuntime,
    ) -> Bound<sys::FileRuntime> {
        Bound {
            table,
            owner: self.clone(),
        }
    }

    pub(crate) fn evidence(&self) -> sys::Evidence<'_> {
        self.runtime.selected.evidence()
    }

    /// The receiver color format that selects `preset`, or the error
    /// [`Self::require_feature`] reports for it. The preset's gate includes
    /// the configured receiver's, so that table is then available too.
    #[cfg(feature = "advanced")]
    pub(crate) fn passthrough_format(
        &self,
        preset: crate::advanced::PassthroughPreset,
    ) -> Result<sys::bindings::NDIlib_recv_color_format_e> {
        self.runtime.selected.evidence().passthrough_format(preset)
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
    /// if grafton_ndi::Ndi::is_running() {
    ///     println!("NDI runtime is active");
    /// }
    /// ```
    pub fn is_running() -> bool {
        sys::is_running()
    }
}

impl Clone for Ndi {
    /// Acquires another reference to the active runtime.
    ///
    /// # Panics
    ///
    /// Panics if the runtime handle reference count is exhausted. The original
    /// handle and runtime remain valid.
    #[expect(
        clippy::expect_used,
        reason = "Clone cannot report handle-count exhaustion; its documented panic preserves the original handle"
    )]
    fn clone(&self) -> Self {
        self.runtime
            .selected
            .lifecycle
            .acquire()
            .expect("Unable to acquire another reference to the existing NDI runtime");
        Self {
            runtime: self.runtime,
        }
    }
}

impl Drop for Ndi {
    fn drop(&mut self) {
        self.runtime.selected.lifecycle.release();
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
            *Self::recover_guard(self.state.lock())
        }

        fn refcount(&self) -> usize {
            match *Self::recover_guard(self.state.lock()) {
                Phase::Running(count) => count.get(),
                _ => 0,
            }
        }
    }

    fn isolated_ndi() -> Ndi {
        // No native operation is invoked by these lifecycle/binding tests.
        let core = sys::Core::load(&sys::table::ExportsWhere(|_| true))
            .unwrap()
            .unwrap();
        Ndi::test_double(core)
    }

    #[test]
    fn a_bound_table_retains_exactly_one_runtime_reference() {
        let ndi = isolated_ndi();
        let manager = &ndi.runtime.selected.lifecycle;
        let bound = ndi.bind_core();
        assert!(
            matches!(*manager.state.lock().unwrap(), Phase::Running(count) if count.get() == 2)
        );
        drop(ndi);
        assert!(manager.is_running());
        assert_eq!(bound.ndi().info().raw_version(), "6.4.0.2");
        drop(bound);
        assert!(matches!(*manager.state.lock().unwrap(), Phase::Stopped));
        assert_eq!(
            std::mem::size_of::<Bound<sys::Core>>(),
            std::mem::size_of::<Ndi>()
        );
        assert_eq!(
            std::mem::size_of::<Bound<sys::Routing>>(),
            std::mem::size_of::<Ndi>() + std::mem::size_of::<&sys::Routing>()
        );
    }

    #[test]
    fn binding_an_existing_owner_needs_no_additional_runtime_handle() {
        struct TestTable;
        impl sys::Table for TestTable {
            fn within(_: sys::Evidence<'_>) -> Result<&Self> {
                static TABLE: TestTable = TestTable;
                Ok(&TABLE)
            }
        }
        struct Owner(Ndi);
        impl LiveOwner for std::sync::Arc<Owner> {
            fn ndi(&self) -> &Ndi {
                &self.0
            }
        }

        let ndi = isolated_ndi();
        let manager = &ndi.runtime.selected.lifecycle;
        let owner = std::sync::Arc::new(Owner(ndi));
        *manager.state.lock().unwrap() = Phase::Running(NonZeroUsize::MAX);
        let bound = Bound::<TestTable, _>::with_owner(owner.clone()).unwrap();
        assert!(std::sync::Arc::ptr_eq(bound.owner(), &owner));
        assert!(
            matches!(*manager.state.lock().unwrap(), Phase::Running(count) if count == NonZeroUsize::MAX)
        );
        *manager.state.lock().unwrap() = Phase::Running(NonZeroUsize::MIN);
        assert_eq!(
            std::mem::size_of_val(&bound),
            std::mem::size_of_val(&owner) + std::mem::size_of::<&TestTable>()
        );
        drop(owner);
        assert!(manager.is_running());
        drop(bound);
        assert!(matches!(*manager.state.lock().unwrap(), Phase::Stopped));
    }

    #[test]
    fn exhausted_binding_never_calls_native_creation() {
        let ndi = isolated_ndi();
        let manager = &ndi.runtime.selected.lifecycle;
        *manager.state.lock().unwrap() = Phase::Running(NonZeroUsize::MAX);
        let creates = AtomicUsize::new(0);
        let rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // SAFETY: no native instance is returned; an exhausted bind must
            // panic before this closure can observe the settings at all.
            let _ = unsafe {
                crate::Finder::create(&ndi, &crate::FinderOptions::new(), |_, _| {
                    creates.fetch_add(1, Ordering::Relaxed);
                    NativeCreation::plain(sys::table::for_test(
                        std::ptr::null_mut(),
                        c"NDIlib_find_create_v2",
                    ))
                })
            };
        }));
        // An unavailable capability is still refused before the exhausted clone.
        let unavailable = ndi.bind::<sys::Routing>();
        *manager.state.lock().unwrap() = Phase::Running(NonZeroUsize::MIN);
        assert!(rejected.is_err());
        assert_eq!(creates.load(Ordering::Relaxed), 0);
        assert!(matches!(
            unavailable,
            Err(Error::UnsupportedCapability { .. })
        ));
    }

    #[test]
    fn refcount_overflow_preserves_live_runtime() {
        let manager = RuntimeManager::with_backend(MockBackend::default());
        manager.acquire().unwrap();
        *manager.state.lock().unwrap() = Phase::Running(NonZeroUsize::MAX);
        assert!(manager.acquire().is_err());
        assert_eq!(manager.refcount(), usize::MAX);
        assert!(manager.is_running());
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 0);
        // Restore the injected count so the acquired reference can be released.
        *manager.state.lock().unwrap() = Phase::Running(NonZeroUsize::MIN);
        manager.release();
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 1);
    }

    #[test]
    fn two_handles_release_the_runtime_exactly_once() {
        let manager = RuntimeManager::with_backend(MockBackend::default());
        manager.acquire().unwrap();
        manager.acquire().unwrap();
        assert_eq!(manager.refcount(), 2);
        manager.release();
        assert_eq!(manager.phase(), Phase::Running(NonZeroUsize::MIN));
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 0);
        manager.release();
        assert_eq!(manager.phase(), Phase::Stopped);
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
        assert_eq!(manager.phase(), Phase::Stopped);

        manager
            .backend
            .cpu_unsupported
            .store(true, Ordering::Release);
        assert!(matches!(manager.acquire(), Err(Error::UnsupportedCpu)));
        assert_eq!(manager.phase(), Phase::Stopped);
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
        // => init called twice, destroy called twice, both Ndi::new() return Ok
        let manager = Arc::new(RuntimeManager::with_backend(MockBackend::default()));

        // First cycle
        manager.acquire().expect("First init should succeed");
        assert_eq!(manager.backend.init_count.load(Ordering::Acquire), 1);
        assert!(manager.is_running());

        manager.release();
        assert_eq!(manager.backend.destroy_count.load(Ordering::Acquire), 1);
        assert!(!manager.is_running());
        assert_eq!(manager.phase(), Phase::Stopped);

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
        assert_eq!(manager.phase(), Phase::Stopped);
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
        assert_eq!(manager.phase(), Phase::Stopped);
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
        assert_eq!(manager.phase(), Phase::Stopped);
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
        // Issue requirement: concurrent Ndi::new() calls result in at most one init per cycle
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
        assert_eq!(manager.phase(), Phase::Stopped);
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
        assert_eq!(manager.phase(), Phase::Stopped);
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
