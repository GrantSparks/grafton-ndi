//! NDI source discovery and network browsing.

mod budget;

pub(crate) use budget::SnapshotBudget;

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    ffi::{c_char, CString},
    fmt::{self, Display, Formatter},
    net::IpAddr,
    ptr,
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::{
    change_feed::ChangeFeed,
    frames::copy_sdk_string,
    sys::{self, bindings::*},
    timeout::Deadline,
    to_ms_checked, Error, Ndi, ResourceKind, Result,
};

/// Configuration for NDI source discovery.
///
/// The defaults find the sources of the groups the NDI configuration names,
/// including those of this machine, with no extra addresses to query.
///
/// # Examples
///
/// ```
/// use grafton_ndi::FinderOptions;
///
/// // Find sources in specific groups, other machines only
/// let options = FinderOptions::new()
///     .show_local_sources(false)
///     .groups("Public,Studio");
///
/// // Also query hosts that discovery does not reach
/// let options = FinderOptions::new().extra_ips("192.168.1.100,10.0.0.100");
/// ```
#[derive(Debug, Clone)]
pub struct FinderOptions {
    show_local_sources: bool,
    groups: Option<String>,
    extra_ips: Option<String>,
    #[cfg(feature = "advanced")]
    config: Option<crate::advanced::AdvancedConfig>,
}

impl Default for FinderOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl FinderOptions {
    /// Inherit discovery groups from the NDI configuration.
    #[must_use]
    pub fn clear_groups(mut self) -> Self {
        self.groups = None;
        self
    }

    /// Remove the explicitly requested direct IP addresses.
    #[must_use]
    pub fn clear_extra_ips(mut self) -> Self {
        self.extra_ips = None;
        self
    }

    /// Remove the per-instance Advanced configuration override.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn clear_config(mut self) -> Self {
        self.config = None;
        self
    }

    /// Whether local sources were requested.
    #[must_use]
    pub fn configured_show_local_sources(&self) -> bool {
        self.show_local_sources
    }

    /// Requested discovery groups, or `None` to inherit the NDI configuration.
    #[must_use]
    pub fn configured_groups(&self) -> Option<&str> {
        self.groups.as_deref()
    }

    /// Requested comma-separated direct IP addresses, if any.
    #[must_use]
    pub fn configured_extra_ips(&self) -> Option<&str> {
        self.extra_ips.as_deref()
    }

    /// The stored Advanced configuration handle, if any.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn configured_config(&self) -> Option<&crate::advanced::AdvancedConfig> {
        self.config.as_ref()
    }

    /// The default options: local sources included, the groups of the NDI
    /// configuration, and no extra addresses.
    pub fn new() -> Self {
        Self {
            show_local_sources: true,
            groups: None,
            extra_ips: None,
            #[cfg(feature = "advanced")]
            config: None,
        }
    }

    /// Set whether sources on this machine are found.
    #[must_use]
    pub fn show_local_sources(mut self, show: bool) -> Self {
        self.show_local_sources = show;
        self
    }

    /// Set the comma-separated groups to search, such as `"Public,Studio"`,
    /// instead of those of the NDI configuration.
    #[must_use]
    pub fn groups(mut self, groups: impl Into<String>) -> Self {
        self.groups = Some(groups.into());
        self
    }

    /// Set comma-separated IP addresses to query directly, for sources that
    /// discovery does not reach.
    #[must_use]
    pub fn extra_ips(mut self, ips: impl Into<String>) -> Self {
        self.extra_ips = Some(ips.into());
        self
    }

    /// Create the finder with this per-instance Advanced SDK configuration,
    /// which it keeps for as long as it lives.
    ///
    /// [`Finder::new`] then needs an Advanced runtime, and a build with the
    /// Advanced SDK.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn config(mut self, config: crate::advanced::AdvancedConfig) -> Self {
        self.config = Some(config);
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FindInstance(NDIlib_find_instance_t);

// SAFETY: the SDK gives finders no thread affinity and allows source-change
// waits beside snapshot queries. Finder's snapshot lock covers every fetch and
// copy before another query can invalidate its buffers; its change feed permits
// only one native waiter. The surrounding Rust fields determine Finder's traits.
unsafe impl Send for FindInstance {}
// SAFETY: the same snapshot and waiter serialization applies to shared uses.
unsafe impl Sync for FindInstance {}

/// Discovers NDI sources on the network.
///
/// `Finder` provides methods to discover and monitor NDI sources. It maintains
/// a background thread that continuously updates the list of available sources.
/// Concurrent lookups share native change reports: every waiting lookup is
/// notified when the source list changes, then takes its own owned snapshot.
///
/// # Examples
///
/// ```no_run
/// # use grafton_ndi::{Ndi, FinderOptions, Finder};
/// # use std::time::Duration;
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = Ndi::new()?;
/// let finder = Finder::new(&ndi, &FinderOptions::new())?;
///
/// // Wait for initial discovery
/// let mut watch = finder.watch();
/// if watch.wait_for_change(Duration::from_secs(5))? {
///     let sources = watch.current_sources()?;
///     for source in sources {
///         println!("Found: {}", source);
///     }
/// }
/// # Ok(())
/// # }
/// ```
pub struct Finder {
    instance: FindInstance,
    // SDK snapshots and their strings expire on the next snapshot call.
    sources_lock: parking_lot::Mutex<FindInstance>,
    changes: ChangeFeed,
    _groups: Option<CString>,    // Hold ownership of CStrings
    _extra_ips: Option<CString>, // to ensure they outlive SDK usage
    #[cfg(feature = "advanced")]
    _config: Option<crate::advanced::AdvancedConfig>,
    _ndi: Ndi,
}

// The mutex guards a native snapshot's lifetime, not mutable Rust state. A
// panic releases it without poisoning, and a later snapshot replaces all SDK
// buffers before reading them. No partially updated Rust invariant survives.
impl std::panic::RefUnwindSafe for Finder {}

pub(crate) mod snapshot_sealed {
    pub trait Sealed {}
}

/// One SDK snapshot entry point and its bounded descriptor copier.
///
/// # Safety
///
/// For a live finder, `fetch` returns null or the initialized descriptor array
/// whose length it writes. The descriptors and their non-null strings remain
/// readable through their NUL or the SDK string bound until the next snapshot
/// call on that finder. `copy` reads only those descriptor fields and returns
/// owned values; it neither replaces nor retains the native snapshot.
pub(crate) unsafe trait FinderSnapshot: snapshot_sealed::Sealed {
    type Descriptor;
    type Item;

    /// # Safety
    /// `instance` is live and no snapshot call can overlap the fetch and copy.
    unsafe fn fetch(
        &self,
        instance: NDIlib_find_instance_t,
        count: &mut u32,
    ) -> *const Self::Descriptor;

    /// # Safety
    /// `source` comes from this entry point's still-live snapshot.
    unsafe fn copy(
        &self,
        source: &Self::Descriptor,
        budget: &mut SnapshotBudget,
    ) -> Result<Option<Self::Item>>;
}

impl snapshot_sealed::Sealed for sys::Core {}

// SAFETY: the v1 SDK snapshot follows FinderSnapshot's contract, and its copier
// produces owned source names and addresses within the shared byte budget.
unsafe impl FinderSnapshot for sys::Core {
    type Descriptor = NDIlib_source_t;
    type Item = Source;

    unsafe fn fetch(
        &self,
        instance: NDIlib_find_instance_t,
        count: &mut u32,
    ) -> *const Self::Descriptor {
        // SAFETY: the guard supplies a live serialized finder instance.
        unsafe { self.NDIlib_find_get_current_sources(instance, count) }
    }

    unsafe fn copy(
        &self,
        source: &Self::Descriptor,
        budget: &mut SnapshotBudget,
    ) -> Result<Option<Self::Item>> {
        // SAFETY: FinderSnapshot's caller keeps this snapshot's strings live.
        unsafe { snapshot_entry(source, budget) }
    }
}

/// An acquired snapshot lock and the exact native handle it guards. Fetching
/// gets its instance from this guard, never from a separate Finder argument.
struct SnapshotGuard<'finder> {
    instance: parking_lot::MutexGuard<'finder, FindInstance>,
}

impl SnapshotGuard<'_> {
    fn snapshot<S: FinderSnapshot>(&self, snapshot: &S) -> Result<Vec<S::Item>> {
        let mut count = 0;
        // SAFETY: this private guard keeps its own finder's handle serialized
        // until all copying completes. The sealed entry point supplies the
        // array and string lifetimes required by the bounded copy kernel.
        unsafe {
            let descriptors = snapshot.fetch(self.instance.0, &mut count);
            budget::copy_snapshot(descriptors, count, |source, budget| {
                snapshot.copy(source, budget)
            })
        }
    }
}

/// One consumer's cursor over a finder's source-list changes.
///
/// Take snapshots and wait through the same watch, so reports published by
/// concurrent lookups between those operations cannot be missed. A report
/// during a snapshot remains visible to the next wait. Each consumer must
/// create its own watch; a watch can move between threads but is not `Sync`.
///
/// ```no_run
/// # use grafton_ndi::{Finder, FinderOptions, Ndi};
/// # use std::time::Duration;
/// # fn main() -> grafton_ndi::Result<()> {
/// # let ndi = Ndi::new()?;
/// # let finder = Finder::new(&ndi, &FinderOptions::new())?;
/// let mut watch = finder.watch();
/// println!("{:?}", watch.current_sources()?);
/// while watch.wait_for_change(Duration::from_secs(5))? {
///     println!("{:?}", watch.current_sources()?);
/// }
/// # Ok(())
/// # }
/// ```
pub struct SourceWatch<'finder> {
    finder: &'finder Finder,
    // Cell makes a cursor Send but not Sync: one consumer advances it.
    observed: Cell<u64>,
}

impl SourceWatch<'_> {
    /// Copy the currently known sources and record the generation before
    /// copying, preserving any report that arrives during the snapshot.
    pub fn current_sources(&mut self) -> Result<Vec<Source>> {
        self.observed.set(self.finder.changes.generation());
        self.finder.current_sources()
    }

    /// Wait for a report newer than this consumer's last snapshot or wait.
    ///
    /// Returns `true` immediately if another lookup already published a report.
    /// A zero timeout reads such reports even while another native waiter is
    /// active; otherwise it makes one nonblocking native query. Returns `false`
    /// when the timeout expires without a report.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] if `timeout` exceeds [`crate::MAX_TIMEOUT`].
    pub fn wait_for_change(&mut self, timeout: Duration) -> Result<bool> {
        Ok(self.wait_until(Deadline::after(timeout)?))
    }

    fn current_sources_until(&mut self, deadline: Deadline) -> Result<Option<Vec<Source>>> {
        self.observed.set(self.finder.changes.generation());
        self.finder.current_sources_until(deadline)
    }

    fn wait_until(&mut self, deadline: Deadline) -> bool {
        if self
            .finder
            .wait_for_sources_since(self.observed.get(), deadline)
        {
            self.observed.set(self.finder.changes.generation());
            true
        } else {
            false
        }
    }
}

/// One entry of a [`Finder::current_sources`] snapshot, or `None` for a
/// descriptor the SDK should not produce, which the snapshot leaves out.
///
/// # Safety
///
/// As for [`Source::try_from_raw`].
pub(crate) unsafe fn snapshot_entry(
    source: &NDIlib_source_t,
    budget: &mut SnapshotBudget,
) -> Result<Option<Source>> {
    let (name, address) = source_pointers(source);
    // SAFETY: the caller keeps both strings readable through their terminator
    // or the SDK string bound. This policy skips malformed strings while
    // retaining their scan charges in the snapshot's shared budget.
    unsafe { decode_source(name, address, |pointer| budget.text(pointer)) }
}

/// The descriptor's address is one pointer, whether the SDK calls it an IP
/// address or a URL. Decoding classifies the copied text by its scheme.
fn source_pointers(source: &NDIlib_source_t) -> (*const c_char, *const c_char) {
    // SAFETY: both union fields are pointers of the same type.
    let address = unsafe { source.__bindgen_anon_1.p_url_address };
    (source.p_ndi_name, address)
}

/// Decode a source's two strings with the caller's copy policy. A null name
/// is allowed when an address is present; both null pointers name no source.
/// `copy` is called only for non-null pointers, and `None` omits the source.
///
/// # Safety
///
/// Each non-null pointer must stay readable through its first NUL or
/// [`crate::frames::MAX_SDK_STRING_BYTES`] bytes, whichever comes first,
/// until its copy finishes. The copy policy must preserve those lifetimes.
pub(crate) unsafe fn decode_source(
    name: *const c_char,
    address: *const c_char,
    mut copy: impl FnMut(*const c_char) -> Result<Option<String>>,
) -> Result<Option<Source>> {
    if name.is_null() && address.is_null() {
        return Ok(None);
    }
    let name = if name.is_null() {
        String::new()
    } else {
        let Some(name) = copy(name)? else {
            return Ok(None);
        };
        name
    };
    let address = if address.is_null() {
        None
    } else {
        let Some(address) = copy(address)? else {
            return Ok(None);
        };
        Some(SourceAddress::from_sdk_text(address))
    };
    Ok(Some(Source { name, address }))
}

impl Finder {
    /// Creates a new source finder with the specified settings.
    ///
    /// # Arguments
    ///
    /// * `ndi` - The NDI instance (cloned internally to keep the runtime alive)
    /// * `options` - Configuration for source discovery
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] when the groups or extra addresses
    /// contain a NUL byte, [`Error::UnsupportedCapability`] when the options
    /// carry an Advanced configuration that the runtime or this build cannot
    /// take, and [`Error::NativeOperationFailed`] when the SDK creates no
    /// finder.
    pub fn new(ndi: &Ndi, options: &FinderOptions) -> Result<Self> {
        #[cfg(feature = "advanced")]
        if let Some(config) = &options.config {
            return Self::new_configured(ndi, options, config);
        }
        // SAFETY: the SDK's finder creation returns null or a fresh finder
        // that nothing else owns, and the settings outlive the call.
        unsafe {
            Self::create(
                ndi,
                options,
                "NDIlib_find_create_v2",
                #[cfg(feature = "advanced")]
                None,
                |core, raw| core.NDIlib_find_create_v2(raw),
            )
        }
    }

    /// The one creation path: create a finder through `create`, the entry
    /// point `operation`, which is handed the core table and the native
    /// settings for `settings`, and keep `config` for as long as the finder
    /// lives.
    ///
    /// # Safety
    ///
    /// `create` returns null or a live finder of `ndi`'s runtime, made from
    /// the settings it was handed, that nothing else owns.
    pub(crate) unsafe fn create(
        ndi: &Ndi,
        settings: &FinderOptions,
        operation: &'static str,
        #[cfg(feature = "advanced")] config: Option<crate::advanced::AdvancedConfig>,
        create: impl FnOnce(&'static sys::Core, &NDIlib_find_create_t) -> NDIlib_find_instance_t,
    ) -> Result<Self> {
        let groups = settings.groups.as_deref().map(CString::new).transpose()?;
        let extra_ips = settings
            .extra_ips
            .as_deref()
            .map(CString::new)
            .transpose()?;
        let instance = create(
            &ndi.api().core,
            &NDIlib_find_create_t {
                show_local_sources: settings.show_local_sources,
                p_groups: groups.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
                p_extra_ips: extra_ips.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
            },
        );
        if instance.is_null() {
            return Err(Error::NativeOperationFailed { operation });
        }
        Ok(Self {
            instance: FindInstance(instance),
            sources_lock: parking_lot::Mutex::new(FindInstance(instance)),
            changes: ChangeFeed::default(),
            _groups: groups,
            _extra_ips: extra_ips,
            #[cfg(feature = "advanced")]
            _config: config,
            _ndi: ndi.clone(),
        })
    }

    /// Copy one snapshot through the entry point's shared lifetime contract.
    /// The guard carries the handle it protects, so fetching cannot pair a
    /// different finder's instance with this lock.
    pub(crate) fn snapshot<S: FinderSnapshot>(&self, snapshot: &S) -> Result<Vec<S::Item>> {
        self.lock_snapshots(None)
            .expect("an unbounded snapshot lock acquisition succeeds")
            .snapshot(snapshot)
    }

    fn lock_snapshots(&self, deadline: Option<Deadline>) -> Option<SnapshotGuard<'_>> {
        let instance = match deadline {
            Some(deadline) => self.sources_lock.try_lock_until(deadline.end())?,
            None => self.sources_lock.lock(),
        };
        Some(SnapshotGuard { instance })
    }

    /// The runtime this finder was created through.
    #[cfg(feature = "advanced")]
    pub(crate) fn runtime(&self) -> &Ndi {
        &self._ndi
    }

    /// Monitor this finder's source list with a cursor for one consumer.
    ///
    /// Every consumer needs its own watch. Reports observed by a concurrent
    /// lookup remain visible to this watch until it snapshots or waits again.
    /// Creating a watch records the current generation without allocating.
    #[must_use]
    pub fn watch(&self) -> SourceWatch<'_> {
        SourceWatch {
            finder: self,
            observed: Cell::new(self.changes.generation()),
        }
    }

    /// Share one native report with every observer of the earlier generation.
    fn wait_for_sources_since(&self, generation: u64, deadline: Deadline) -> bool {
        // SAFETY: the finder instance is live for this borrow.
        self.changes
            .wait_since(generation, deadline, |timeout_ms| unsafe {
                self._ndi
                    .api()
                    .core
                    .NDIlib_find_wait_for_sources(self.instance.0, timeout_ms)
            })
    }

    /// Returns the current list of discovered sources (snapshot).
    ///
    /// This method uses `NDIlib_find_get_current_sources` which provides a snapshot
    /// of the current source list without any additional network discovery.
    /// Concurrent snapshots on this finder are serialized until all source names
    /// and addresses have been copied into owned Rust values.
    ///
    /// Available since NDI SDK 6.0.
    ///
    /// Each snapshot permits at most 16,384 descriptors and 16 MiB total
    /// native descriptors, result storage, scanned string bytes (including
    /// rejected entries) and produced UTF-8. Exceeding either budget returns
    /// [`Error::ResourceLimit`]; reservations are fallible. An isolated
    /// malformed descriptor within the budget is skipped.
    ///
    /// # Returns
    ///
    /// A vector of currently known sources. May be empty if no sources are found.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, FinderOptions, Finder};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// // Get current snapshot of sources
    /// let sources = finder.current_sources()?;
    ///
    /// for source in sources {
    ///     println!("Current source: {}", source);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn current_sources(&self) -> Result<Vec<Source>> {
        self.snapshot(&self._ndi.api().core)
    }

    /// The lookup's next snapshot, or none when contention exhausts its budget.
    fn current_sources_until(&self, deadline: Deadline) -> Result<Option<Vec<Source>>> {
        let Some(guard) = self.lock_snapshots(Some(deadline)) else {
            return Ok(None);
        };
        guard.snapshot(&self._ndi.api().core).map(Some)
    }

    /// Poll discovery until `accept` selects a value or `timeout` elapses.
    ///
    /// After each source-list snapshot, `accept` is handed the current sources
    /// and may return `Some` to stop immediately; otherwise this blocks on the
    /// next source-list change and retries until the deadline. A single
    /// [`SourceWatch::wait_for_change`] returns on the *first*
    /// change, yielding only a partial snapshot when discovery is staggered
    /// (unicast / IP-hint-only networks where responders trickle in across
    /// several notifications); polling to the deadline makes resolution robust
    /// against that while still returning the instant `accept` is satisfied.
    ///
    /// Returns `Ok(None)` if the deadline passes without `accept` selecting a
    /// value.
    fn poll_until_deadline<T>(
        &self,
        timeout: Duration,
        mut accept: impl FnMut(Vec<Source>) -> Result<Option<T>>,
    ) -> Result<Option<T>> {
        // Validate up front so an over-long timeout is rejected even when the
        // first snapshot already satisfies `accept`.
        let deadline = Deadline::after(timeout)?;
        let mut watch = self.watch();
        loop {
            let Some(sources) = watch.current_sources_until(deadline)? else {
                return Ok(None);
            };
            if let Some(selected) = accept(sources)? {
                return Ok(Some(selected));
            }
            if deadline.is_expired() {
                return Ok(None);
            }
            // Block until the source list changes or the window ends; either
            // way the next iteration re-snapshots. The wait is rounded up, so
            // the last one lasts until the deadline rather than polling.
            watch.wait_until(deadline);
        }
    }

    /// Wait until a source satisfies `select`, using this finder's discovery scope.
    ///
    /// Attempts a current snapshot immediately, then waits for source-list changes
    /// within one total wait budget, including snapshot-lock contention.
    /// `Duration::ZERO` makes one non-waiting snapshot attempt; a busy snapshot lock
    /// returns `None`. Returns the first matching complete descriptor in a snapshot,
    /// or `None` when the budget expires without a match. Predicates run after
    /// releasing the snapshot lock, may inspect name and address together, and may
    /// call this finder's snapshot APIs. No source's
    /// continued availability is guaranteed.
    ///
    /// This blocks the calling thread and allocates bounded owned snapshots as
    /// [`Self::current_sources`] describes. It retains this finder's groups, IP
    /// hints, local-source policy, configuration and runtime; it creates no finder.
    /// Concurrent selection operations can use the same finder. Native snapshot
    /// execution, bounded descriptor copies, and caller predicates cannot be
    /// interrupted and may finish after the wait budget; `current_sources` itself
    /// keeps its ordinary blocking snapshot-lock behavior.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] for a timeout above [`crate::MAX_TIMEOUT`],
    /// including immediate matches, and snapshot errors such as
    /// [`Error::ResourceLimit`]. A panic in `select` propagates outside SDK locks.
    pub fn find_source(
        &self,
        timeout: Duration,
        mut select: impl FnMut(&Source) -> bool,
    ) -> Result<Option<Source>> {
        self.poll_until_deadline(timeout, |sources| {
            Ok(sources.into_iter().find(|source| select(source)))
        })
    }

    /// Wait for an exact, case-sensitive complete NDI source name.
    ///
    /// Names normally look like `"STUDIO (Camera 2)"`. Duplicate names at different
    /// addresses select the first match in the current snapshot; use
    /// [`Self::find_source`] with an address predicate to disambiguate them.
    /// Timeout, allocation, concurrency and error behavior match that method.
    /// An empty name or a name containing a NUL is refused with
    /// [`Error::InvalidInput`] before inspecting a snapshot.
    pub fn find_source_by_name(&self, name: &str, timeout: Duration) -> Result<Option<Source>> {
        to_ms_checked(timeout)?;
        if name.is_empty() || name.contains('\0') {
            return Err(Error::InvalidInput(
                "Source name must be nonempty and contain no NUL".into(),
            ));
        }
        self.find_source(timeout, |source| source.name() == name)
    }

    /// Discovers sources for up to `timeout`, returning the complete set.
    ///
    /// This honors the full `timeout` window and returns every source observed
    /// within it. Unlike a single [`SourceWatch::wait_for_change`]
    /// followed by a snapshot — which returns on the *first* source-list change
    /// and so can miss responders that announce late on staggered or unicast
    /// (IP-hint-only) networks — enumeration here retains each source observed
    /// during the window.
    ///
    /// For an immediate, non-blocking snapshot of already-known sources use
    /// [`current_sources`](Self::current_sources); to resolve a single specific
    /// feed use [`Self::find_source`] or [`Self::find_source_by_name`].
    ///
    /// # Arguments
    ///
    /// * `timeout` - Discovery window, including waits for the snapshot lock.
    ///   [`Duration::ZERO`] makes one non-waiting snapshot attempt, returning no
    ///   sources if the lock is busy. Must not exceed [`crate::MAX_TIMEOUT`]
    ///   (~49.7 days). Native snapshot execution and descriptor copies can finish
    ///   after the wait budget; lock contention returns the results observed so far.
    ///
    /// # Returns
    ///
    /// Every source observed within the window, in first-observed order.
    /// Identity includes the name and the complete address (including its variant);
    /// a changed descriptor is returned as a distinct source. Network discovery
    /// cannot guarantee observation of every source. May be empty if none appear.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] if `timeout` exceeds [`crate::MAX_TIMEOUT`].
    /// Returns [`Error::ResourceLimit`] if a snapshot exceeds the limits of
    /// [`Self::current_sources`], or distinct results retained across the
    /// whole window exceed 16,384 entries or 16 MiB. The retained byte budget
    /// counts both copies of each source in the result vector and the
    /// deduplication set, including their strings and container capacity.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, FinderOptions, Finder};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// // Discover for up to 5 seconds, then list everything found
    /// let sources = finder.find_sources(Duration::from_secs(5))?;
    ///
    /// for source in sources {
    ///     println!("Found: {}", source);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn find_sources(&self, timeout: Duration) -> Result<Vec<Source>> {
        // Preserve first-observed order and deduplicate by complete descriptor.
        let mut seen = HashSet::new();
        let mut discovered = Vec::new();
        let mut retained_bytes = 0;
        self.poll_until_deadline(timeout, |sources| {
            accumulate_sources(&mut discovered, &mut seen, &mut retained_bytes, sources)?;
            Ok(None::<()>)
        })?;
        Ok(discovered)
    }
}

impl Drop for Finder {
    fn drop(&mut self) {
        // SAFETY: the instance belongs to this runtime and is destroyed once.
        unsafe { self._ndi.api().core.NDIlib_find_destroy(self.instance.0) };
    }
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Finder>();
};

fn accumulate_sources(
    discovered: &mut Vec<Source>,
    seen: &mut HashSet<Source>,
    retained_bytes: &mut usize,
    sources: Vec<Source>,
) -> Result<()> {
    for source in sources {
        if seen.contains(&source) {
            continue;
        }
        if discovered.len() == budget::MAX_SOURCES {
            return Err(budget::limit(
                ResourceKind::FinderDiscoverySources,
                budget::MAX_SOURCES,
            ));
        }
        let text_bytes = source.name.capacity()
            + source.address.as_ref().map_or(0, |a| match a {
                SourceAddress::Ip(text) | SourceAddress::Url(text) => text.capacity(),
            });
        let bytes = retained_bytes
            .checked_add(text_bytes * 2)
            .ok_or_else(|| budget::limit(ResourceKind::FinderDiscoveryBytes, budget::MAX_BYTES))?;
        // Check before growing either container or copying strings. Round
        // up for vector growth, hash buckets and control bytes; the actual
        // capacities are checked as well before retaining the source.
        let next_capacity = (discovered.len() + 1).max(4).next_power_of_two();
        let growth_bound = next_capacity * 4 * (std::mem::size_of::<Source>() + 1);
        if bytes + growth_bound > budget::MAX_BYTES {
            return Err(budget::limit(
                ResourceKind::FinderDiscoveryBytes,
                budget::MAX_BYTES,
            ));
        }
        discovered
            .try_reserve(1)
            .map_err(|_| budget::allocation_failed())?;
        seen.try_reserve(1)
            .map_err(|_| budget::allocation_failed())?;
        // HashSet capacity excludes unused buckets. Charging twice its
        // capacity covers bucket rounding and control bytes conservatively.
        let containers = discovered.capacity() * std::mem::size_of::<Source>()
            + seen.capacity() * 2 * (std::mem::size_of::<Source>() + 1);
        if bytes + containers > budget::MAX_BYTES {
            return Err(budget::limit(
                ResourceKind::FinderDiscoveryBytes,
                budget::MAX_BYTES,
            ));
        }
        seen.insert(source.try_copy()?);
        discovered.push(source);
        *retained_bytes = bytes;
    }
    Ok(())
}

struct Authority<'a> {
    host: &'a str,
    port: Option<u16>,
}

fn parse_authority(address: &str) -> Option<Authority<'_>> {
    let address = address.split_once("://").map_or(address, |(_, rest)| rest);
    let authority = address.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    if authority.is_empty() {
        return None;
    }
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, suffix) = rest.split_once(']')?;
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            return None;
        }
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':')?.parse().ok()?)
        };
        return Some(Authority { host, port });
    }
    if authority.matches(':').count() > 1 {
        authority.parse::<std::net::Ipv6Addr>().ok()?;
        return Some(Authority {
            host: authority,
            port: None,
        });
    }
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, Some(port.parse().ok()?)),
        None => (authority, None),
    };
    if host.is_empty() {
        return None;
    }
    Some(Authority { host, port })
}

#[derive(Clone, Copy)]
enum HostQuery<'a> {
    Ip(IpAddr),
    Name(&'a str),
}

impl<'a> HostQuery<'a> {
    fn parse(host: &'a str) -> Option<Self> {
        let ip_text = if host.starts_with('[') {
            host.strip_prefix('[')?.strip_suffix(']')?
        } else {
            host
        };
        if let Ok(ip) = ip_text.parse::<IpAddr>() {
            return Some(Self::Ip(ip));
        }
        if host != ip_text || host.is_empty() || host.contains('\0') {
            return None;
        }
        let name = host.strip_suffix('.').unwrap_or(host);
        if name.is_empty()
            || (name.contains('.') && name.bytes().all(|c| c.is_ascii_digit() || c == b'.'))
            || !name.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label.as_bytes()[0].is_ascii_alphanumeric()
                    && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                    && label
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            })
            || name.len() > 253
        {
            return None;
        }
        Some(Self::Name(name))
    }

    fn matches(self, candidate: &str) -> bool {
        match self {
            Self::Ip(ip) => candidate.parse::<IpAddr>() == Ok(ip),
            Self::Name(name) => candidate
                .strip_suffix('.')
                .unwrap_or(candidate)
                .eq_ignore_ascii_case(name),
        }
    }
}

/// Canonical retained cache identity. IPs use their numeric display spelling;
/// names use the parser's final-dot policy and ASCII lowercase. Those domains
/// are disjoint: valid names cannot contain colons or dotted numeric IP text.
#[derive(Debug, PartialEq, Eq, Hash)]
struct HostKey(String);

impl std::borrow::Borrow<str> for HostKey {
    fn borrow(&self) -> &str {
        &self.0
    }
}

/// Bounded normalization keeps lookups and eviction free of key allocations.
/// Only a published entry needs an owned string.
struct HostKeyBuffer {
    bytes: [u8; 253],
    len: usize,
}

impl HostKeyBuffer {
    fn new(query: HostQuery<'_>) -> Self {
        let mut key = Self {
            bytes: [0; 253],
            len: 0,
        };
        match query {
            HostQuery::Ip(ip) => {
                use std::io::Write;
                let mut cursor = std::io::Cursor::new(key.bytes.as_mut_slice());
                // The longest numeric IP spelling is shorter than 253 bytes.
                write!(cursor, "{ip}").expect("IP fits in host key buffer");
                key.len = cursor.position() as usize;
            }
            HostQuery::Name(name) => {
                key.len = name.len();
                key.bytes[..key.len].copy_from_slice(name.as_bytes());
                key.bytes[..key.len].make_ascii_lowercase();
            }
        }
        key
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len]).expect("host keys are ASCII")
    }

    fn into_owned(self) -> HostKey {
        HostKey(self.as_str().to_owned())
    }
}

/// Network address of an NDI source.
///
/// The SDK supplies one address string. A string containing `://` is classified
/// as a URL; other strings use the IP variant. This classification does not
/// establish the source's codec or whether it uses NDI|HX.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceAddress {
    /// A URL address. SDK address strings containing `://` use this variant.
    Url(String),
    /// A host or IP address, optionally with a port. SDK strings without
    /// `://` use this variant.
    Ip(String),
}

impl SourceAddress {
    fn from_sdk_text(address: String) -> Self {
        if address.contains("://") {
            Self::Url(address)
        } else {
            Self::Ip(address)
        }
    }

    /// Check whether this address has the exact host or IP.
    ///
    /// Hostnames compare without ASCII case or a final DNS dot. Invalid bare-host
    /// queries return false.
    ///
    /// # Arguments
    ///
    /// * `host` - The hostname or IP address to search for
    ///
    /// # Examples
    ///
    /// ```
    /// use grafton_ndi::SourceAddress;
    ///
    /// let addr = SourceAddress::Ip("192.168.1.100:5960".to_string());
    /// assert!(addr.matches_host("192.168.1.100"));
    /// assert!(!addr.matches_host("192.168.1"));
    ///
    /// let url = SourceAddress::Url("http://camera.local:8080".to_string());
    /// assert!(url.matches_host("camera.local"));
    /// ```
    pub fn matches_host(&self, host: &str) -> bool {
        HostQuery::parse(host).is_some_and(|query| self.matches_query(query))
    }

    fn matches_query(&self, query: HostQuery<'_>) -> bool {
        self.authority()
            .is_some_and(|authority| query.matches(authority.host))
    }

    /// Extract an explicit port from conventional host/port or URL syntax.
    ///
    /// SDK address strings are opaque; unrecognized syntax returns `None`.
    ///
    /// Parses the port from addresses in the format `host:port`.
    ///
    /// # Returns
    ///
    /// `Some(port)` if a valid port is found, `None` otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// use grafton_ndi::SourceAddress;
    ///
    /// let addr = SourceAddress::Ip("192.168.1.100:5960".to_string());
    /// assert_eq!(addr.port(), Some(5960));
    ///
    /// let no_port = SourceAddress::Ip("192.168.1.100".to_string());
    /// assert_eq!(no_port.port(), None);
    ///
    /// let url = SourceAddress::Url("http://camera.local:8080".to_string());
    /// assert_eq!(url.port(), Some(8080));
    /// ```
    pub fn port(&self) -> Option<u16> {
        self.authority().and_then(|authority| authority.port)
    }

    // SDK addresses are opaque. Only interpret conventional URL authorities and
    // host[:port] forms; never inspect their path, query, or fragment as a port.
    fn authority(&self) -> Option<Authority<'_>> {
        match self {
            Self::Ip(address) | Self::Url(address) => parse_authority(address),
        }
    }
}

/// An NDI source: its name and, when known, its network address.
///
/// Equality and hashing compare the complete name and address, including the
/// address variant; they do not imply persistent device identity.
///
/// The name typically includes the machine name and source name (e.g.,
/// "MACHINE (Source)"). A source found by a [`Finder`] has the address it
/// advertises; a receiver given a source without one looks it up by name.
///
/// # Examples
///
/// ```
/// use grafton_ndi::{Source, SourceAddress};
///
/// let source = Source::new("LAPTOP (Camera 1)")
///     .with_address(SourceAddress::Ip("192.168.1.100:5960".to_string()));
/// assert_eq!(source.name(), "LAPTOP (Camera 1)");
/// assert_eq!(source.to_string(), "LAPTOP (Camera 1)@192.168.1.100:5960");
/// assert!(Source::new("LAPTOP (Camera 1)").address().is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Source {
    name: String,
    address: Option<SourceAddress>,
}

#[repr(C)]
pub(crate) struct RawSource {
    _name: CString,
    _url_address: Option<CString>,
    _ip_address: Option<CString>,
    pub raw: NDIlib_source_t,
}

// SAFETY: the descriptor's pointers lead only into the strings this value
// owns, whose heap buffers do not move when the value does, and nothing
// writes through them. Moving it to another thread moves those strings with
// it, as moving a `CString` does.
unsafe impl Send for RawSource {}

// SAFETY: a shared descriptor exposes only immutable pointers into the C strings
// this value owns. Their buffers stay allocated for the shared borrow and no SDK
// operation writes through them, so concurrent readers cannot race with mutation.
unsafe impl Sync for RawSource {}

impl Source {
    /// Copy strings with exact reservations while preserving address variants.
    fn try_copy(&self) -> Result<Self> {
        let name = budget::copy_string(&self.name)?;
        let address = self
            .address
            .as_ref()
            .map(|address| -> Result<SourceAddress> {
                Ok(match address {
                    SourceAddress::Ip(text) => SourceAddress::Ip(budget::copy_string(text)?),
                    SourceAddress::Url(text) => SourceAddress::Url(budget::copy_string(text)?),
                })
            })
            .transpose()?;
        Ok(Self { name, address })
    }

    /// The source named `name`, without an address.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            address: None,
        }
    }

    /// This source, at `address`.
    #[must_use]
    pub fn with_address(self, address: SourceAddress) -> Self {
        Self {
            address: Some(address),
            ..self
        }
    }

    /// The NDI source name (e.g., "MACHINE (Source Name)"). A source the SDK
    /// described by its address alone has an empty name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The network address for connecting to this source, when known.
    pub fn address(&self) -> Option<&SourceAddress> {
        self.address.as_ref()
    }

    /// Check if this source matches a given host or IP address.
    ///
    /// This checks the complete address host, or for hostname queries the
    /// machine component of an SDK `MACHINE (Source Name)` label.
    ///
    /// # Arguments
    ///
    /// * `host` - The hostname or IP address to match against
    ///
    /// # Examples
    ///
    /// ```
    /// use grafton_ndi::{Source, SourceAddress};
    ///
    /// let source = Source::new("CAMERA1 (Chan1, 192.168.0.107)")
    ///     .with_address(SourceAddress::Ip("192.168.0.107:5960".to_string()));
    ///
    /// assert!(source.matches_host("192.168.0.107"));
    /// assert!(source.matches_host("CAMERA1"));
    /// assert!(!source.matches_host("192.168.1.1"));
    /// ```
    pub fn matches_host(&self, host: &str) -> bool {
        HostQuery::parse(host).is_some_and(|query| self.matches_query(query))
    }

    fn matches_query(&self, query: HostQuery<'_>) -> bool {
        if self
            .address
            .as_ref()
            .is_some_and(|address| address.matches_query(query))
        {
            return true;
        }
        match query {
            HostQuery::Ip(_) => false,
            HostQuery::Name(_) => self
                .name
                .split_once(" (")
                .filter(|(_, source)| source.ends_with(')'))
                .is_some_and(|(machine, _)| query.matches(machine)),
        }
    }

    /// Extract the host from conventional host/port or URL syntax.
    ///
    /// Brackets around IPv6 literals are removed. SDK addresses are opaque;
    /// unrecognized syntax returns `None`.
    ///
    /// For IP-based sources, this returns the IP portion without the port.
    /// For URL-based sources, this extracts the hostname portion, which may be
    /// a name such as `camera.local` rather than an IP address.
    ///
    /// # Returns
    ///
    /// `Some(host)` if an IP or hostname is found, `None` otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// use grafton_ndi::{Source, SourceAddress};
    ///
    /// let source = Source::new("CAMERA1")
    ///     .with_address(SourceAddress::Ip("192.168.1.100:5960".to_string()));
    ///
    /// assert_eq!(source.host(), Some("192.168.1.100"));
    /// ```
    pub fn host(&self) -> Option<&str> {
        self.address
            .as_ref()?
            .authority()
            .map(|authority| authority.host)
    }

    /// Copy a source descriptor that the SDK function `operation` returned
    /// into owned strings after null checks, replacing bytes that are not
    /// UTF-8.
    ///
    /// Null checks cannot validate dangling or otherwise invalid pointers. The
    /// SDK must provide a live descriptor and readable NUL-terminated strings.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NativeOperationFailed`] naming `operation` when the
    /// descriptor is null, or names neither a source nor an address, and
    /// [`Error::UnexpectedSdkResult`] when its name or address is longer than
    /// the 4 MiB one SDK string may be.
    ///
    /// # Safety
    ///
    /// A non-null `source_ptr` must point to a live `NDIlib_source_t`. Its
    /// non-null string pointers must remain readable through their NUL
    /// terminators, or for 4 MiB, until copying finishes.
    pub(crate) unsafe fn try_from_raw(
        source_ptr: *const NDIlib_source_t,
        operation: &'static str,
    ) -> Result<Self> {
        let failed = || Error::NativeOperationFailed { operation };
        if source_ptr.is_null() {
            return Err(failed());
        }

        // SAFETY: the caller guarantees a non-null descriptor is live.
        let ndi_source = unsafe { &*source_ptr };
        let (name, address) = source_pointers(ndi_source);
        // SAFETY: the caller keeps both strings readable through their
        // terminator or the SDK string bound. The decoder passes only non-null
        // pointers to this strict policy, which reports malformed SDK text.
        unsafe {
            decode_source(name, address, |pointer| {
                copy_sdk_string(pointer, operation).map(Some)
            })
        }?
        .ok_or_else(failed)
    }

    /// Copy the source's strings into owned C strings and build the native
    /// descriptor that points into them. RawSource keeps those allocations
    /// alive for every SDK call that borrows its descriptor.
    pub(crate) fn to_raw(&self) -> Result<RawSource> {
        let name = CString::new(self.name.clone()).map_err(Error::InvalidCString)?;

        let (url_address, ip_address, __bindgen_anon_1) = match &self.address {
            Some(SourceAddress::Url(url)) => {
                let url_cstr = CString::new(url.clone()).map_err(Error::InvalidCString)?;
                let p_url = url_cstr.as_ptr();
                (
                    Some(url_cstr),
                    None,
                    NDIlib_source_t__bindgen_ty_1 {
                        p_url_address: p_url,
                    },
                )
            }
            Some(SourceAddress::Ip(ip)) => {
                let ip_cstr = CString::new(ip.clone()).map_err(Error::InvalidCString)?;
                let p_ip = ip_cstr.as_ptr();
                (
                    None,
                    Some(ip_cstr),
                    NDIlib_source_t__bindgen_ty_1 { p_ip_address: p_ip },
                )
            }
            None => (
                None,
                None,
                NDIlib_source_t__bindgen_ty_1 {
                    p_ip_address: ptr::null(),
                },
            ),
        };

        let p_ndi_name = name.as_ptr();

        Ok(RawSource {
            _name: name,
            _url_address: url_address,
            _ip_address: ip_address,
            raw: NDIlib_source_t {
                p_ndi_name,
                __bindgen_anon_1,
            },
        })
    }
}

impl Display for Source {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.address {
            Some(SourceAddress::Url(address) | SourceAddress::Ip(address)) => {
                write!(f, "{name}@{address}", name = self.name)
            }
            None => write!(f, "{name}", name = self.name),
        }
    }
}

/// A thread-safe cache of complete host result sets in one finder's scope.
///
/// A host can publish several feeds. [`Self::sources_by_host`] retains every
/// distinct matching descriptor observed over its discovery window, rather than
/// choosing a feed by arrival order. Use [`Finder::find_source`] for a selection
/// that should return as soon as a particular feed appears.
///
/// The cache owns its configured finder, keeping it and the NDI runtime alive.
/// Configure groups, IP hints and Advanced settings before constructing the
/// cache: a lookup never creates another finder or changes its configuration.
/// Store it directly in server state, or share it through [`std::sync::Arc`].
/// [`finder`](Self::finder) lends the configured finder for uncached selection.
///
/// Entries remain until [`Self::invalidate`] or [`Self::clear`]; they can contain
/// offline endpoints and omit feeds announced after the original window. Empty
/// results are not cached. Equivalent hostname case/final-dot and numeric IP
/// spellings share an entry. DNS aliases otherwise remain distinct identities.
///
/// The cache is `Send + Sync`. Discovery runs outside its mutex so unrelated
/// lookups and invalidation remain available; SDK snapshots retain their normal
/// per-finder serialization. Returned vectors own copies of the descriptors.
/// Concurrent discovery waits share change reports through that finder, so
/// one lookup cannot consume another lookup's notification.
/// Dropping the cache destroys its native finder and releases its runtime
/// handle synchronously on the dropping thread.
///
/// ```no_run
/// # use grafton_ndi::{Ndi, Finder, FinderOptions, SourceCache};
/// # use std::time::Duration;
/// # fn main() -> grafton_ndi::Result<()> {
/// let ndi = Ndi::new()?;
/// let finder = Finder::new(&ndi, &FinderOptions::new().groups("Studio")
///     .extra_ips("192.168.0.107"))?;
/// let cache = SourceCache::new(finder);
/// let feeds = cache.sources_by_host("192.168.0.107", Duration::from_secs(5))?;
/// for feed in feeds { println!("{feed}"); }
/// # Ok(())
/// # }
/// ```
pub struct SourceCache {
    finder: Finder,
    entries: CachedSources,
}

#[derive(Default)]
struct CachedSources {
    cache: Mutex<CacheState>,
}

struct CacheState {
    entries: HashMap<HostKey, Arc<Vec<Source>>>,
    generation: Option<u64>,
}

impl Default for CacheState {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            generation: Some(0),
        }
    }
}

impl CacheState {
    fn invalidate_pending(&mut self) {
        self.generation = self.generation.and_then(|value| value.checked_add(1));
    }
}

impl SourceCache {
    /// Create an empty cache owning `finder` and its discovery configuration.
    ///
    /// This moves the finder without allocating, cloning or making an SDK call.
    /// It keeps the finder alive and NDI initialized until the cache is dropped.
    /// Share the cache through [`std::sync::Arc`] for long-lived server workers;
    /// [`finder`](Self::finder) reaches the same finder for uncached lookups.
    ///
    /// ```no_run
    /// use grafton_ndi::{Finder, FinderOptions, Ndi, Result, SourceCache};
    /// use std::sync::Arc;
    ///
    /// struct ServerState {
    ///     sources: SourceCache,
    /// }
    ///
    /// fn server_state(ndi: &Ndi) -> Result<Arc<ServerState>> {
    ///     let options = FinderOptions::new().groups("Studio");
    ///     let finder = Finder::new(ndi, &options)?;
    ///     Ok(Arc::new(ServerState {
    ///         sources: SourceCache::new(finder),
    ///     }))
    /// }
    /// ```
    pub fn new(finder: Finder) -> Self {
        Self {
            finder,
            entries: CachedSources::default(),
        }
    }

    /// The configured finder, borrowed for uncached discovery and selection.
    ///
    /// This allocates nothing and makes no SDK call. The finder cannot be
    /// replaced through this borrow, so cached entries retain their scope.
    #[must_use]
    pub fn finder(&self) -> &Finder {
        &self.finder
    }

    /// All feeds observed on a complete hostname or IP within `timeout`.
    ///
    /// A cache hit returns owned descriptor copies without an SDK call. On a
    /// miss, observes the full window using the configured finder; zero makes
    /// one non-waiting snapshot attempt. Results retain first-observed order and distinguish names,
    /// full addresses, and address variants. Select the desired feed explicitly.
    /// Empty results return an empty vector and are not retained.
    ///
    /// Discovery runs outside the cache lock; its window includes snapshot-lock
    /// contention. Access to cached descriptor copies can briefly serialize on the
    /// cache mutex. Copying and native snapshot execution can finish after the
    /// discovery wait budget. Discovery uses the same bounded, fallible snapshot
    /// and accumulated-result allocations as
    /// [`Finder::find_sources`]. Returned cached descriptors are fallibly copied.
    /// Entries have the explicit stale-data policy described on [`SourceCache`].
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] for a non-bare host or a timeout above
    /// [`crate::MAX_TIMEOUT`], even on a cache hit; discovery/resource errors
    /// propagate without filling an entry. IP hints must already be configured
    /// on the finder; this method does not change its discovery scope.
    pub fn sources_by_host(&self, host: &str, timeout: Duration) -> Result<Vec<Source>> {
        self.entries.sources_by_host_with(host, timeout, |query| {
            let mut seen = HashSet::new();
            let mut discovered = Vec::new();
            let mut retained_bytes = 0;
            self.finder().poll_until_deadline(timeout, |mut sources| {
                sources.retain(|source| source.matches_query(query));
                accumulate_sources(&mut discovered, &mut seen, &mut retained_bytes, sources)?;
                Ok(None::<()>)
            })?;
            Ok(discovered)
        })
    }

    /// Remove a host's complete result set and suppress pending fills.
    /// Equivalent spellings remove the same entry. An invalid host removes no
    /// entry but still suppresses publication from every pending discovery.
    pub fn invalidate(&self, host: &str) {
        self.entries.invalidate(host);
    }

    /// Remove every result set and suppress publication from pending discovery.
    pub fn clear(&self) {
        self.entries.clear();
    }

    /// Number of cached host identities, including every feed in each identity.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no host result sets are cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Fallible owned descriptor copies: discovery has already bounded their total
/// count and bytes, and each copy keeps that bound rather than growing capacity.
fn copy_sources(sources: &[Source]) -> Result<Vec<Source>> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(sources.len())
        .map_err(|_| budget::allocation_failed())?;
    for source in sources {
        copy.push(source.try_copy()?);
    }
    Ok(copy)
}

impl CachedSources {
    fn sources_by_host_with(
        &self,
        host: &str,
        timeout: Duration,
        discover: impl FnOnce(HostQuery<'_>) -> Result<Vec<Source>>,
    ) -> Result<Vec<Source>> {
        self.sources_by_host_with_copy(host, timeout, discover, copy_sources)
    }

    fn sources_by_host_with_copy(
        &self,
        host: &str,
        timeout: Duration,
        discover: impl FnOnce(HostQuery<'_>) -> Result<Vec<Source>>,
        mut copy: impl FnMut(&[Source]) -> Result<Vec<Source>>,
    ) -> Result<Vec<Source>> {
        let query = HostQuery::parse(host).ok_or_else(|| {
            Error::InvalidInput("Host must be a bare hostname or IP address".into())
        })?;
        to_ms_checked(timeout)?;
        let key = HostKeyBuffer::new(query);
        let (snapshot, generation) = {
            let cache = self
                .cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (cache.entries.get(key.as_str()).cloned(), cache.generation)
        };
        if let Some(snapshot) = snapshot {
            return copy(&snapshot);
        }
        // SDK waits and user discovery run outside the cache mutex. Invalidation
        // changes the generation, so an old result cannot repopulate the cache.
        let sources = discover(query)?;
        if !sources.is_empty() {
            {
                let cache = self
                    .cache
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if generation.is_none()
                    || cache.generation != generation
                    || cache.entries.contains_key(key.as_str())
                {
                    return Ok(sources);
                }
            }
            // The bounded, fallible deep copy happens outside the mutex.
            // Publication must still recheck both generation and key: another
            // discovery or invalidation may have completed during this copy.
            let snapshot = Arc::new(copy(&sources)?);
            let mut cache = self
                .cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if generation.is_some()
                && cache.generation == generation
                && !cache.entries.contains_key(key.as_str())
            {
                cache
                    .entries
                    .try_reserve(1)
                    .map_err(|_| budget::allocation_failed())?;
                cache.entries.insert(key.into_owned(), snapshot);
            }
        }
        Ok(sources)
    }

    fn invalidate(&self, host: &str) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.invalidate_pending();
        if let Some(query) = HostQuery::parse(host) {
            cache.entries.remove(HostKeyBuffer::new(query).as_str());
        }
    }

    fn clear(&self) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.invalidate_pending();
        cache.entries.clear();
    }

    fn len(&self) -> usize {
        self.cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entries
            .len()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_descriptor_copies_release_the_global_mutex_and_recheck_publication() {
        let cache = CachedSources::default();
        let sources = vec![Source::new("camera (Program)")];
        for hit in [false, true] {
            let result = cache
                .sources_by_host_with_copy(
                    "camera",
                    Duration::ZERO,
                    |_| {
                        assert!(!hit);
                        Ok(sources.clone())
                    },
                    |snapshot| {
                        assert!(
                            cache.cache.try_lock().is_ok(),
                            "descriptor copies must not hold the cache mutex"
                        );
                        copy_sources(snapshot)
                    },
                )
                .unwrap();
            assert_eq!(result, sources);
        }
        cache.clear();
        let result = cache
            .sources_by_host_with_copy(
                "camera",
                Duration::ZERO,
                |_| Ok(sources.clone()),
                |snapshot| {
                    cache.invalidate("camera");
                    copy_sources(snapshot)
                },
            )
            .unwrap();
        assert_eq!(result, sources);
        assert!(
            cache.is_empty(),
            "invalidation during snapshot preparation suppresses publication"
        );
        cache
            .sources_by_host_with_copy(
                "camera",
                Duration::ZERO,
                |_| {
                    cache.clear();
                    Ok(sources.clone())
                },
                |_| panic!("stale discovery must not allocate a publication copy"),
            )
            .unwrap();
    }

    #[test]
    fn source_cache_moves_its_finder_without_allocating_or_recreating_it() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let finder = Finder::new(&ndi, &FinderOptions::new().groups("Studio")).unwrap();
        let instance = finder.instance;
        let (cache, allocations) = crate::test_alloc::allocation_count(|| SourceCache::new(finder));
        assert_eq!(allocations, 0);
        assert_eq!(cache.finder().instance, instance);
        assert_eq!(cache.finder()._groups.as_deref(), Some(c"Studio"));
        assert!(cache.is_empty());
    }

    fn host_key(host: &str) -> HostKey {
        HostKeyBuffer::new(HostQuery::parse(host).unwrap()).into_owned()
    }

    #[test]
    fn canonical_host_keys_obey_identity_and_hash_laws() {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let hash = |key: &HostKey| {
            let mut state = DefaultHasher::new();
            key.hash(&mut state);
            let owned_hash = state.finish();
            let mut borrowed_state = DefaultHasher::new();
            std::borrow::Borrow::<str>::borrow(key).hash(&mut borrowed_state);
            assert_eq!(owned_hash, borrowed_state.finish());
            owned_hash
        };
        for variants in [
            vec!["camera.local", "CAMERA.LOCAL", "Camera.Local."],
            vec!["2001:db8::1", "[2001:0DB8:0:0:0:0:0:1]", "2001:db8:0::1"],
            vec!["192.0.2.1", "[192.0.2.1]"],
        ] {
            for a in &variants {
                for b in &variants {
                    assert_eq!(host_key(a), host_key(b));
                    assert_eq!(hash(&host_key(a)), hash(&host_key(b)));
                }
            }
        }
        for name in ["camera", "camera-1.local", "a.b.c", "123", &"a".repeat(63)] {
            let expected = host_key(name);
            // Vary each ASCII letter's case independently, then the final dot.
            for i in 0..name.len() {
                let mut bytes = name.as_bytes().to_vec();
                bytes[i].make_ascii_uppercase();
                let variant = String::from_utf8(bytes).unwrap();
                for variant in [variant.clone(), format!("{variant}.")] {
                    let key = host_key(&variant);
                    assert_eq!(expected, key);
                    assert_eq!(hash(&expected), hash(&key));
                }
            }
        }
        let maximum = [
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(61),
        ]
        .join(".");
        assert_eq!(maximum.len(), 253);
        assert_eq!(
            host_key(&maximum),
            host_key(&format!("{}.", maximum.to_uppercase()))
        );
        let distinct = [
            "camera",
            "camera1",
            "camera.local",
            "192.0.2.1",
            "192.0.2.2",
            "::1",
            "::ffff:192.0.2.1",
            "123",
        ];
        assert_eq!(
            distinct
                .iter()
                .map(|host| host_key(host))
                .collect::<HashSet<_>>()
                .len(),
            distinct.len()
        );
    }

    #[test]
    fn equivalent_cache_hits_and_invalidation_share_one_identity() {
        for (a, b) in [
            ("CAMERA.LOCAL", "camera.local"),
            ("camera.local.", "camera.local"),
            ("[2001:0db8:0:0:0:0:0:1]", "2001:db8::1"),
        ] {
            for (first, second) in [(a, b), (b, a)] {
                let cache = CachedSources::default();
                let old =
                    Source::new("camera").with_address(SourceAddress::Ip("192.0.2.1:5960".into()));
                let fresh = old
                    .clone()
                    .with_address(SourceAddress::Ip("192.0.2.1:5961".into()));
                cache
                    .sources_by_host_with(first, Duration::ZERO, |_| Ok(vec![old.clone()]))
                    .unwrap();
                let (identical, identical_allocs) = crate::test_alloc::allocation_count(|| {
                    cache
                        .sources_by_host_with(first, Duration::ZERO, |_| panic!("cache hit"))
                        .unwrap()
                });
                let (equivalent, equivalent_allocs) = crate::test_alloc::allocation_count(|| {
                    cache
                        .sources_by_host_with(second, Duration::ZERO, |_| {
                            panic!("equivalent cache hit")
                        })
                        .unwrap()
                });
                assert_eq!(identical, vec![old.clone()]);
                assert_eq!(equivalent, vec![old]);
                assert_eq!(identical_allocs, 3); // Vector plus two descriptor strings.
                assert_eq!(equivalent_allocs, identical_allocs);
                assert_eq!(cache.len(), 1);
                assert!(cache
                    .sources_by_host_with(second, Duration::MAX, |_| panic!("invalid timeout"))
                    .is_err());
                cache.invalidate(second);
                assert!(cache.is_empty());
                assert_eq!(
                    cache
                        .sources_by_host_with(first, Duration::ZERO, |_| Ok(vec![fresh.clone()]))
                        .unwrap(),
                    vec![fresh]
                );
                assert_eq!(cache.len(), 1);
                cache.invalidate("invalid:host:port");
                assert_eq!(cache.len(), 1);
                cache.clear();
                assert!(cache
                    .sources_by_host_with(first, Duration::ZERO, |_| Err(Error::InvalidInput(
                        "discovery failed".into()
                    )))
                    .is_err());
                assert!(cache
                    .sources_by_host_with(second, Duration::ZERO, |_| Ok(Vec::new()))
                    .unwrap()
                    .is_empty());
                assert!(cache.is_empty());
            }
        }
    }

    #[test]
    fn exact_host_matching_ignores_labels_ports_and_url_payloads() {
        let other = Source::new("CAMERA10 (192.168.1.1)")
            .with_address(SourceAddress::Ip("192.168.1.100:5960".into()));
        assert!(!other.matches_host("192.168.1.1"));
        assert!(!other.matches_host("5960"));
        assert!(!other.matches_host("CAMERA1"));
        let exact = Source::new("CAMERA1 (Program)").with_address(SourceAddress::Url(
            "http://user:pass@Camera.Local:8080/ndi?next=192.0.2.1".into(),
        ));
        assert!(exact.matches_host("camera.local."));
        assert!(exact.matches_host("camera1"));
        assert!(!exact.matches_host("192.0.2.1"));
        assert!(!exact.matches_host("Program"));
        let ipv6 = SourceAddress::Ip("[2001:0db8:0:0:0:0:0:1]:5960".into());
        assert!(ipv6.matches_host("2001:db8::1"));
        assert!(ipv6.matches_host("[2001:db8::1]"));
        for invalid in [
            "",
            "camera:5960",
            "http://camera.local",
            "camera/local",
            "192.168.1",
            "foo\0bar",
            "[bad]",
        ] {
            assert!(HostQuery::parse(invalid).is_none(), "{invalid:?}");
        }
        let (_, allocations) = crate::test_alloc::allocation_count(|| {
            for _ in 0..100 {
                assert!(exact.matches_host("CAMERA.LOCAL"));
                assert!(ipv6.matches_host("2001:db8::1"));
            }
        });
        assert_eq!(allocations, 0);
    }

    #[test]
    fn cache_selects_exact_host_and_rejects_invalid_queries_before_discovery() {
        let cache = CachedSources::default();
        let wrong = Source::new("CAMERA10 (Program)")
            .with_address(SourceAddress::Ip("192.168.1.100:5960".into()));
        let right = Source::new("CAMERA1 (Program)")
            .with_address(SourceAddress::Ip("192.168.1.1:5961".into()));
        let timeout = Duration::ZERO;
        let selected = cache
            .sources_by_host_with("192.168.1.1", timeout, |query| {
                Ok([wrong.clone(), right.clone()]
                    .into_iter()
                    .filter(|s| s.matches_query(query))
                    .collect())
            })
            .unwrap();
        assert_eq!(selected, vec![right.clone()]);
        assert_eq!(cache.len(), 1);
        assert_eq!(
            cache
                .sources_by_host_with("192.168.1.1", timeout, |_| panic!("cache hit"))
                .unwrap(),
            vec![right]
        );
        for invalid in ["", "192.168.1.1:5961", "camera?query", "foo\0bar"] {
            assert!(matches!(
                cache.sources_by_host_with(invalid, timeout, |_| panic!(
                    "invalid query reached discovery"
                )),
                Err(Error::InvalidInput(_))
            ));
        }
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn invalidation_prevents_pending_lookup_from_repopulating_cache() {
        use std::sync::{mpsc, Arc};
        for clear in [false, true] {
            let cache = Arc::new(CachedSources::default());
            let (started_tx, started_rx) = mpsc::channel();
            let (resume_tx, resume_rx) = mpsc::channel();
            let old = Source::new("old").with_address(SourceAddress::Ip("192.0.2.1:5960".into()));
            let fresh =
                Source::new("fresh").with_address(SourceAddress::Ip("192.0.2.1:5961".into()));
            let pending = Arc::clone(&cache);
            let old_result = old.clone();
            let thread = std::thread::spawn(move || {
                pending
                    .sources_by_host_with("CAMERA.LOCAL.", Duration::ZERO, |_| {
                        started_tx.send(()).unwrap();
                        resume_rx.recv().unwrap();
                        Ok(vec![old_result])
                    })
                    .unwrap()
            });
            started_rx.recv().unwrap();
            if clear {
                cache.clear();
            } else {
                cache.invalidate("camera.local");
            }
            assert_eq!(cache.len(), 0);
            let found = cache
                .sources_by_host_with("camera.local", Duration::ZERO, |_| Ok(vec![fresh.clone()]))
                .unwrap();
            assert_eq!(found, vec![fresh.clone()]);
            resume_tx.send(()).unwrap();
            assert_eq!(thread.join().unwrap(), vec![old]);
            assert_eq!(
                cache
                    .sources_by_host_with("Camera.Local.", Duration::ZERO, |_| panic!(
                        "fresh entry was overwritten"
                    ))
                    .unwrap(),
                vec![fresh]
            );
        }
    }

    #[test]
    fn absent_invalidation_and_empty_clear_suppress_pending_fills() {
        use std::sync::{mpsc, Arc};
        for clear in [false, true] {
            let cache = Arc::new(CachedSources::default());
            let (started_tx, started_rx) = mpsc::channel();
            let (resume_tx, resume_rx) = mpsc::channel();
            let pending = Arc::clone(&cache);
            let thread = std::thread::spawn(move || {
                pending
                    .sources_by_host_with("camera.local", Duration::ZERO, |_| {
                        started_tx.send(()).unwrap();
                        resume_rx.recv().unwrap();
                        Ok(vec![Source::new("CAMERA (Program)")])
                    })
                    .unwrap()
            });
            started_rx.recv().unwrap();
            if clear {
                cache.clear();
            } else {
                cache.invalidate("absent.local");
            }
            resume_tx.send(()).unwrap();
            thread.join().unwrap();
            assert!(cache.is_empty());
        }
    }

    #[test]
    fn exhausted_generation_suppresses_publication() {
        let cache = CachedSources::default();
        cache.cache.lock().unwrap().generation = Some(u64::MAX);
        cache.invalidate("absent");
        let source = Source::new("camera").with_address(SourceAddress::Ip("192.0.2.1".into()));
        assert_eq!(
            cache
                .sources_by_host_with("192.0.2.1", Duration::ZERO, |_| Ok(vec![source.clone()]))
                .unwrap(),
            vec![source]
        );
        assert!(cache.is_empty());
    }
    use std::ffi::CString;

    #[test]
    fn defaults_include_local_sources() {
        for options in [FinderOptions::default(), FinderOptions::new()] {
            assert!(options.show_local_sources);
            assert!(options.groups.is_none() && options.extra_ips.is_none());
        }
        assert!(
            !FinderOptions::new()
                .show_local_sources(false)
                .show_local_sources
        );
    }

    #[test]
    fn authority_parsing_separates_hosts_ports_and_url_components() {
        for (address, host, port) in [
            ("192.0.2.1:5960", "192.0.2.1", Some(5960)),
            ("2001:db8::1", "2001:db8::1", None),
            ("[::1]:5960", "::1", Some(5960)),
            (
                "http://camera:8080/path:123?port=456#789",
                "camera",
                Some(8080),
            ),
            ("http://camera?x=:123", "camera", None),
            ("http://user:pass@[::1]:8080/path", "::1", Some(8080)),
        ] {
            let source = Source::new("").with_address(SourceAddress::Url(address.into()));
            assert_eq!(source.host(), Some(host), "{address}");
            assert_eq!(source.address().unwrap().port(), port, "{address}");
        }
        assert!(parse_authority("[::1").is_none());
        assert!(parse_authority("camera:invalid").is_none());
        assert!(parse_authority("").is_none());
    }

    #[test]
    fn discovery_accumulates_structurally_distinct_sources_in_order() {
        let first = Source::new("Camera").with_address(SourceAddress::Ip("192.0.2.1".into()));
        let changed = first
            .clone()
            .with_address(SourceAddress::Ip("192.0.2.2".into()));
        let distinct_variant = first
            .clone()
            .with_address(SourceAddress::Url("192.0.2.1".into()));
        let mut seen = HashSet::new();
        let mut discovered = Vec::new();
        let mut bytes = 0;
        accumulate_sources(&mut discovered, &mut seen, &mut bytes, vec![first.clone()]).unwrap();
        accumulate_sources(&mut discovered, &mut seen, &mut bytes, Vec::new()).unwrap();
        accumulate_sources(
            &mut discovered,
            &mut seen,
            &mut bytes,
            vec![first.clone(), changed.clone(), distinct_variant.clone()],
        )
        .unwrap();
        assert_eq!(discovered, vec![first, changed, distinct_variant]);
    }

    #[test]
    fn cache_validates_timeout_before_returning_cached_source() {
        let cache = CachedSources::default();
        cache.cache.lock().unwrap().entries.insert(
            HostKeyBuffer::new(HostQuery::parse("camera").unwrap()).into_owned(),
            Arc::new(vec![Source::new("camera")]),
        );
        for host in ["camera", "not_cached"] {
            assert!(matches!(
                cache.sources_by_host_with(host, Duration::MAX, |_| panic!("invalid timeout")),
                Err(Error::InvalidInput(_))
            ));
        }
        // A hit is answered without reaching discovery, the only step that
        // uses the runtime.
        assert_eq!(
            cache
                .sources_by_host_with("camera", Duration::ZERO, |_| panic!("cache hit"))
                .unwrap(),
            vec![Source::new("camera")]
        );
    }

    #[test]
    fn concurrent_snapshots_keep_sdk_buffers_exclusive_until_copied() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Barrier,
        };

        struct Probe<'a> {
            finder: &'a Finder,
            raw: &'a NDIlib_source_t,
            active: &'a AtomicUsize,
        }
        impl snapshot_sealed::Sealed for Probe<'_> {}
        // SAFETY: the probe borrows its stack descriptor and live C strings
        // through the entire serialized call and retains no native reference.
        unsafe impl FinderSnapshot for Probe<'_> {
            type Descriptor = NDIlib_source_t;
            type Item = Source;

            unsafe fn fetch(
                &self,
                instance: NDIlib_find_instance_t,
                count: &mut u32,
            ) -> *const Self::Descriptor {
                assert_eq!(instance, self.finder.instance.0);
                assert_eq!(self.active.fetch_add(1, Ordering::SeqCst), 0);
                assert!(self.finder.sources_lock.try_lock().is_none());
                *count = 1;
                std::thread::yield_now();
                self.raw
            }

            unsafe fn copy(
                &self,
                source: &Self::Descriptor,
                budget: &mut SnapshotBudget,
            ) -> Result<Option<Source>> {
                assert!(self.finder.sources_lock.try_lock().is_none());
                std::thread::yield_now();
                // SAFETY: the probe's borrowed C strings remain live.
                let owned = unsafe { snapshot_entry(source, budget) };
                assert_eq!(self.active.fetch_sub(1, Ordering::SeqCst), 1);
                owned
            }
        }

        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let finder = Finder::new(&ndi, &FinderOptions::default()).unwrap();
        let start = Barrier::new(8);
        let active_snapshot = AtomicUsize::new(0);

        std::thread::scope(|scope| {
            for index in 0..8 {
                let finder = &finder;
                let start = &start;
                let active_snapshot = &active_snapshot;
                scope.spawn(move || {
                    let name = CString::new(format!("Camera {index}")).unwrap();
                    let address = CString::new(format!("192.0.2.{index}:5960")).unwrap();
                    let raw = NDIlib_source_t {
                        p_ndi_name: name.as_ptr(),
                        __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                            p_ip_address: address.as_ptr(),
                        },
                    };
                    start.wait();
                    let sources = finder.snapshot(&Probe {
                        finder,
                        raw: &raw,
                        active: active_snapshot,
                    }).unwrap();
                    drop(name);
                    drop(address);
                    assert_eq!(sources[0].name(), format!("Camera {index}"));
                    assert!(matches!(sources[0].address(), Some(SourceAddress::Ip(ip)) if ip == &format!("192.0.2.{index}:5960")));
                });
            }
        });
        assert_eq!(active_snapshot.load(Ordering::SeqCst), 0);

        // A panic while copying must not disable later snapshots. The mutex
        // protects a lifetime window, not mutable Rust state needing repair.
        std::thread::scope(|scope| {
            assert!(scope
                .spawn(|| {
                    let _guard = finder.sources_lock.lock();
                    panic!("interrupted snapshot");
                })
                .join()
                .is_err());
        });
        assert!(finder.current_sources().is_ok());
    }

    #[test]
    fn test_try_from_raw_null_pointer() {
        // A null descriptor is a failure of the function that returned it.
        let result = unsafe { Source::try_from_raw(ptr::null(), "NDIlib_send_get_source_name") };
        assert!(matches!(
            result,
            Err(Error::NativeOperationFailed {
                operation: "NDIlib_send_get_source_name"
            })
        ));
    }

    #[test]
    fn source_copy_policies_decode_the_same_descriptors() {
        enum Expected {
            Source(Source),
            Missing,
            Malformed,
        }

        let name = c"Camera".as_ptr();
        let ip = c"192.0.2.1:5960".as_ptr();
        let url = c"ndi://camera.local:5960".as_ptr();
        let empty = c"".as_ptr();
        let unterminated = vec![b'x'; crate::frames::MAX_SDK_STRING_BYTES];
        let malformed = unterminated.as_ptr().cast();
        let cases = [
            (
                "name and IP",
                name,
                ip,
                Expected::Source(
                    Source::new("Camera").with_address(SourceAddress::Ip("192.0.2.1:5960".into())),
                ),
            ),
            (
                "name and URL",
                name,
                url,
                Expected::Source(
                    Source::new("Camera")
                        .with_address(SourceAddress::Url("ndi://camera.local:5960".into())),
                ),
            ),
            (
                "address only",
                ptr::null(),
                ip,
                Expected::Source(
                    Source::new("").with_address(SourceAddress::Ip("192.0.2.1:5960".into())),
                ),
            ),
            (
                "neither pointer",
                ptr::null(),
                ptr::null(),
                Expected::Missing,
            ),
            (
                "lossy name",
                c"Cam\xff".as_ptr(),
                ptr::null(),
                Expected::Source(Source::new("Cam\u{fffd}")),
            ),
            ("unterminated name", malformed, ip, Expected::Malformed),
            (
                "name only",
                name,
                ptr::null(),
                Expected::Source(Source::new("Camera")),
            ),
            (
                "empty name",
                empty,
                ptr::null(),
                Expected::Source(Source::new("")),
            ),
            (
                "empty address only",
                ptr::null(),
                empty,
                Expected::Source(Source::new("").with_address(SourceAddress::Ip("".into()))),
            ),
            (
                "empty address",
                name,
                empty,
                Expected::Source(Source::new("Camera").with_address(SourceAddress::Ip("".into()))),
            ),
            ("unterminated address", name, malformed, Expected::Malformed),
        ];

        for (label, name, address, expected) in cases {
            let raw = NDIlib_source_t {
                p_ndi_name: name,
                __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                    p_url_address: address,
                },
            };
            // SAFETY: pointers are null, live C string literals, or the live
            // buffer readable for the complete SDK string bound. Both copies
            // finish before any backing storage is dropped.
            let (snapshot, single) = unsafe {
                (
                    snapshot_entry(&raw, &mut SnapshotBudget::new()),
                    Source::try_from_raw(&raw, "test source query"),
                )
            };
            match expected {
                Expected::Source(expected) => {
                    assert_eq!(snapshot.unwrap().as_ref(), Some(&expected), "{label}");
                    assert_eq!(single.unwrap(), expected, "{label}");
                }
                Expected::Missing => {
                    assert_eq!(snapshot.unwrap(), None, "{label}");
                    assert!(
                        matches!(
                            &single,
                            Err(Error::NativeOperationFailed {
                                operation: "test source query"
                            })
                        ),
                        "{label}: {single:?}"
                    );
                }
                Expected::Malformed => {
                    assert_eq!(snapshot.unwrap(), None, "{label}");
                    assert!(
                        matches!(&single, Err(Error::UnexpectedSdkResult(_))),
                        "{label}: {single:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn source_strings_are_bounded_by_the_sdk_string_limit() {
        use crate::frames::MAX_SDK_STRING_BYTES;
        // The longest name that fits: its terminator is the last byte the
        // scan may read. One byte longer, the terminator lies past the limit.
        let mut longest = vec![b'x'; MAX_SDK_STRING_BYTES];
        longest[MAX_SDK_STRING_BYTES - 1] = 0;
        let mut too_long = vec![b'x'; MAX_SDK_STRING_BYTES + 1];
        too_long[MAX_SDK_STRING_BYTES] = 0;
        let address = c"192.0.2.1:5960";
        let descriptor = |name: &[u8], address: *const std::ffi::c_char| NDIlib_source_t {
            p_ndi_name: name.as_ptr().cast(),
            __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                p_ip_address: address,
            },
        };

        // SAFETY: every descriptor points to live NUL-terminated strings.
        unsafe {
            let fits = Source::try_from_raw(&descriptor(&longest, ptr::null()), "test").unwrap();
            assert_eq!(fits.name().len(), MAX_SDK_STRING_BYTES - 1);
            for raw in [
                descriptor(&too_long, address.as_ptr()),
                descriptor(c"Camera".to_bytes_with_nul(), too_long.as_ptr().cast()),
            ] {
                let Err(Error::UnexpectedSdkResult(reason)) =
                    Source::try_from_raw(&raw, "NDIlib_send_get_source_name")
                else {
                    panic!("a string longer than the limit must be refused");
                };
                assert!(
                    reason.starts_with("NDIlib_send_get_source_name"),
                    "{reason}"
                );
                assert!(reason.contains("4 MiB"), "{reason}");
            }
        }
    }

    #[test]
    fn a_snapshot_leaves_out_only_the_malformed_entries() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let finder = Finder::new(&ndi, &FinderOptions::default()).unwrap();
        let mut too_long = vec![b'x'; crate::frames::MAX_SDK_STRING_BYTES + 1];
        *too_long.last_mut().unwrap() = 0;
        let named = |name: *const std::ffi::c_char| NDIlib_source_t {
            p_ndi_name: name,
            __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                p_ip_address: ptr::null(),
            },
        };
        let snapshot = [
            named(c"First".as_ptr()),
            named(too_long.as_ptr().cast()),
            named(ptr::null()),
            named(c"Last".as_ptr()),
        ];
        struct SliceSnapshot<'a>(&'a [NDIlib_source_t]);
        impl snapshot_sealed::Sealed for SliceSnapshot<'_> {}
        // SAFETY: this fixture is created only below, where its array and all
        // strings stay live until the guard completes the bounded copy.
        unsafe impl FinderSnapshot for SliceSnapshot<'_> {
            type Descriptor = NDIlib_source_t;
            type Item = Source;

            unsafe fn fetch(
                &self,
                _: NDIlib_find_instance_t,
                count: &mut u32,
            ) -> *const Self::Descriptor {
                *count = self.0.len() as u32;
                self.0.as_ptr()
            }

            unsafe fn copy(
                &self,
                source: &Self::Descriptor,
                budget: &mut SnapshotBudget,
            ) -> Result<Option<Source>> {
                // SAFETY: all fixture strings remain live through this call.
                unsafe { snapshot_entry(source, budget) }
            }
        }
        let sources = finder.snapshot(&SliceSnapshot(&snapshot));
        let sources = sources.unwrap();
        let names: Vec<_> = sources.iter().map(Source::name).collect();
        assert_eq!(names, ["First", "Last"]);
    }

    #[test]
    fn test_try_from_raw_null_name() {
        // Create a source with null p_ndi_name
        let source = NDIlib_source_t {
            p_ndi_name: ptr::null(),
            __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                p_ip_address: ptr::null(),
            },
        };

        let result = unsafe { Source::try_from_raw(&source, "NDIlib_find_get_current_sources") };
        assert!(matches!(
            result,
            Err(Error::NativeOperationFailed {
                operation: "NDIlib_find_get_current_sources"
            })
        ));
    }

    #[test]
    fn test_try_from_raw_address_only_source() {
        let url = CString::new("ndi://127.0.0.1:5960").unwrap();
        let raw = NDIlib_source_t {
            p_ndi_name: ptr::null(),
            __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                p_url_address: url.as_ptr(),
            },
        };
        let source = unsafe { Source::try_from_raw(&raw, "test") }.unwrap();
        assert!(source.name().is_empty());
        assert!(
            matches!(source.address(), Some(SourceAddress::Url(address)) if address == "ndi://127.0.0.1:5960")
        );
        let roundtrip = source.to_raw().unwrap();
        assert!(!roundtrip.raw.p_ndi_name.is_null());
    }

    #[test]
    fn test_try_from_raw_valid_source_with_ip() {
        // Create valid C strings
        let name = CString::new("Test Source").unwrap();
        let ip = CString::new("192.168.1.100:5960").unwrap();

        let source = NDIlib_source_t {
            p_ndi_name: name.as_ptr(),
            __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                p_ip_address: ip.as_ptr(),
            },
        };

        let result = unsafe { Source::try_from_raw(&source, "test") };
        assert!(result.is_ok());

        let source = result.unwrap();
        assert_eq!(source.name(), "Test Source");
        match source.address() {
            Some(SourceAddress::Ip(ip_str)) => {
                assert_eq!(ip_str, "192.168.1.100:5960");
            }
            _ => panic!("Expected IP address"),
        }
    }

    #[test]
    fn test_try_from_raw_valid_source_with_url() {
        // Create valid C strings
        let name = CString::new("HX Source").unwrap();
        let url = CString::new("http://camera.local:8080/ndi").unwrap();

        let source = NDIlib_source_t {
            p_ndi_name: name.as_ptr(),
            __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                p_url_address: url.as_ptr(),
            },
        };

        let result = unsafe { Source::try_from_raw(&source, "test") };
        assert!(result.is_ok());

        let source = result.unwrap();
        assert_eq!(source.name(), "HX Source");
        match source.address() {
            Some(SourceAddress::Url(url_str)) => {
                assert_eq!(url_str, "http://camera.local:8080/ndi");
            }
            _ => panic!("Expected URL address"),
        }
    }

    #[test]
    fn test_try_from_raw_valid_source_no_address() {
        // Create valid C string for name, null for address
        let name = CString::new("Source No Addr").unwrap();

        let source = NDIlib_source_t {
            p_ndi_name: name.as_ptr(),
            __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                p_ip_address: ptr::null(),
            },
        };

        let result = unsafe { Source::try_from_raw(&source, "test") };
        assert!(result.is_ok());

        let source = result.unwrap();
        assert_eq!(source.name(), "Source No Addr");
        assert!(source.address().is_none());
    }

    #[test]
    fn test_source_cache_poison_recovery() {
        use std::sync::Arc;
        use std::thread;

        let cache = Arc::new(CachedSources::default());
        let cache_clone = Arc::clone(&cache);

        // Spawn a thread that acquires the lock and panics
        let handle = thread::spawn(move || {
            let _lock = cache_clone
                .cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            panic!("intentional panic while holding lock");
        });

        // Wait for the thread to panic
        let _ = handle.join();

        // Verify all cache methods still work after poison
        assert!(cache.is_empty(), "is_empty should work after poison");
        assert_eq!(cache.len(), 0, "len should work after poison");
        cache.invalidate("test");
        cache.clear();
    }
}
