//! NDI source discovery and network browsing.

mod budget;

#[cfg(test)]
mod cache_retirement_tests;

#[cfg(ndi_test_finder_snapshot_guard)]
#[path = "../tests/ui/internal/finder_snapshot_guard.rs"]
mod snapshot_guard_contract;

pub(crate) use budget::SnapshotBudget;

use std::{
    borrow::{Borrow, Cow},
    cell::Cell,
    collections::{HashMap, HashSet},
    ffi::{c_char, CString},
    fmt::{self, Display, Formatter},
    hash::{Hash, Hasher},
    net::IpAddr,
    ptr,
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::{
    change_feed::ChangeFeed,
    list_setting::list_cstring,
    sdk_copy::LossyText,
    sys::{self, bindings::*},
    timeout::{Deadline, NeverStop, Stop, StopResult, POLL_INTERVAL},
    Error, Ndi, ResourceKind, Result,
};

/// Configuration for NDI source discovery.
///
/// The defaults find the sources of the groups the NDI configuration names,
/// including those of this machine, and query the configured `networks.ips`.
///
/// # Examples
///
/// ```
/// use grafton_ndi::FinderOptions;
///
/// // Find sources in specific groups, other machines only
/// let options = FinderOptions::new()
///     .show_local_sources(false)
///     .groups(["Public", "Studio"]);
///
/// // Also query hosts that discovery does not reach
/// let options = FinderOptions::new().extra_ips(["192.168.1.100", "10.0.0.100"]);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FinderOptions {
    show_local_sources: bool,
    groups: Option<Vec<String>>,
    extra_ips: Option<Vec<String>>,
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

    /// Inherit direct IP addresses from the NDI configuration.
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
    pub fn configured_groups(&self) -> Option<&[String]> {
        self.groups.as_deref()
    }

    /// Requested direct IP addresses, in their stored order, if any.
    #[must_use]
    pub fn configured_extra_ips(&self) -> Option<&[String]> {
        self.extra_ips.as_deref()
    }

    /// The stored Advanced configuration handle, if any.
    #[cfg(feature = "advanced")]
    #[must_use]
    pub fn configured_config(&self) -> Option<&crate::advanced::AdvancedConfig> {
        self.config.as_ref()
    }

    /// The default options: local sources included, the groups of the NDI
    /// configuration, and the direct addresses in its `networks.ips`.
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

    /// Set a non-empty list of groups to search in place of the NDI configuration.
    /// Use [`Self::clear_groups`] to inherit the configuration.
    /// Creation refuses empty elements and elements containing a comma or NUL.
    #[must_use]
    pub fn groups(mut self, groups: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.groups = Some(groups.into_iter().map(Into::into).collect());
        self
    }

    /// Set a non-empty list of IP addresses to query directly, replacing
    /// `networks.ips` from the NDI configuration. Use [`Self::clear_extra_ips`]
    /// to inherit the configuration. Creation refuses empty elements and
    /// elements containing a comma or NUL.
    #[must_use]
    pub fn extra_ips(mut self, ips: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.extra_ips = Some(ips.into_iter().map(Into::into).collect());
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

// SAFETY: Processing.NDI.Find.h forbids overlapping current-source snapshots
// on one instance. Finder's snapshot lock covers every fetch and copy before
// another snapshot can invalidate its buffers; its change feed permits only one
// native waiter. Beyond that header contract, we rely on no thread affinity for
// creation, use and destruction, and on a native wait beside snapshots. Real
// runtime tests in tests/change_notifications.rs exercise cross-thread use and
// concurrent waits/snapshots; tests/finder_deadlines.rs pins snapshot contention.
// The surrounding Rust fields determine Finder's remaining traits.
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
    sdk: crate::runtime::Bound<sys::Core>,
}

impl fmt::Debug for Finder {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("Finder")
            .field("groups", &self._groups)
            .field("extra_ips", &self._extra_ips)
            .finish_non_exhaustive()
    }
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

/// The same native snapshot and copy budget, with allocation deferred until
/// an internal selector accepts the borrowed, bounded source text.
struct SelectingSnapshot<'a, F> {
    core: &'a sys::Core,
    select: F,
}

impl<F> snapshot_sealed::Sealed for SelectingSnapshot<'_, F> {}

// SAFETY: this delegates fetch to Core and never lets borrowed text escape
// copy; accepted entries become the same owned Sources as an ordinary snapshot.
unsafe impl<F: Fn(&BorrowedSource<'_>) -> Result<bool>> FinderSnapshot
    for SelectingSnapshot<'_, F>
{
    type Descriptor = NDIlib_source_t;
    type Item = Source;

    unsafe fn fetch(
        &self,
        instance: NDIlib_find_instance_t,
        count: &mut u32,
    ) -> *const Self::Descriptor {
        // SAFETY: forwarded to the serialized live-instance obligation.
        unsafe { self.core.NDIlib_find_get_current_sources(instance, count) }
    }

    unsafe fn copy(
        &self,
        source: &Self::Descriptor,
        budget: &mut SnapshotBudget,
    ) -> Result<Option<Source>> {
        let (name, address) = source_pointers(source);
        // SAFETY: the snapshot guard keeps both native strings readable.
        unsafe { decode_source(name, address, budget, &self.select) }
    }
}

/// An acquired snapshot lock and the exact native handle it guards. Fetching
/// gets its instance from this guard, never from a separate Finder argument.
struct SnapshotGuard<'finder> {
    instance: parking_lot::MutexGuard<'finder, FindInstance>,
}

/// A snapshot wait's expiry is part of its type: an unbounded wait cannot
/// expire, while a lookup deadline can. Both use the same cancellation kernel.
trait SnapshotWait: Copy {
    type Expired;

    fn remaining(self) -> Duration;
    fn expiration(self) -> Option<Self::Expired>;
    fn lock(
        self,
        mutex: &parking_lot::Mutex<FindInstance>,
    ) -> std::result::Result<parking_lot::MutexGuard<'_, FindInstance>, Self::Expired>;
}

#[derive(Clone, Copy)]
struct UnboundedSnapshot;

impl SnapshotWait for UnboundedSnapshot {
    type Expired = std::convert::Infallible;

    fn remaining(self) -> Duration {
        POLL_INTERVAL
    }

    fn expiration(self) -> Option<Self::Expired> {
        None
    }

    fn lock(
        self,
        mutex: &parking_lot::Mutex<FindInstance>,
    ) -> std::result::Result<parking_lot::MutexGuard<'_, FindInstance>, Self::Expired> {
        Ok(mutex.lock())
    }
}

impl SnapshotWait for Deadline {
    type Expired = ();

    fn remaining(self) -> Duration {
        Deadline::remaining(self).min(POLL_INTERVAL)
    }

    fn expiration(self) -> Option<Self::Expired> {
        self.is_expired().then_some(())
    }

    fn lock(
        self,
        mutex: &parking_lot::Mutex<FindInstance>,
    ) -> std::result::Result<parking_lot::MutexGuard<'_, FindInstance>, Self::Expired> {
        mutex.try_lock_until(self.end()).ok_or(())
    }
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

/// The shared two-step cursor protocol. Workers observe; only the owner commits.
/// A Cell keeps each cursor Send but not Sync, matching its single consumer.
pub(crate) struct SourceCursor {
    observed: Cell<u64>,
}

/// Evidence that one worker completed an observation. Only the protocol below
/// can construct it; dropping it leaves the owner's cursor unchanged.
pub(crate) struct SourceObservation(u64);

impl SourceCursor {
    pub(crate) fn new(generation: u64) -> Self {
        Self {
            observed: Cell::new(generation),
        }
    }

    pub(crate) fn observed(&self) -> u64 {
        self.observed.get()
    }

    pub(crate) fn commit(&self, observation: Option<SourceObservation>) {
        if let Some(SourceObservation(generation)) = observation {
            self.observed.set(generation);
        }
    }

    pub(crate) fn snapshot<T>(
        finder: &Finder,
        snapshot: impl FnOnce(&Finder) -> Result<T>,
        completed: impl FnOnce(&T) -> bool,
    ) -> Result<(T, Option<SourceObservation>)> {
        Self::observe_snapshot(
            || finder.changes.generation(),
            || snapshot(finder),
            completed,
        )
    }

    // Read before copying, but acknowledge only after the owner receives the
    // result. Reports during copying remain outstanding; failure or an
    // incomplete internal snapshot creates no observation to commit.
    fn observe_snapshot<T>(
        generation: impl FnOnce() -> u64,
        snapshot: impl FnOnce() -> Result<T>,
        completed: impl FnOnce(&T) -> bool,
    ) -> Result<(T, Option<SourceObservation>)> {
        let observed = generation();
        let value = snapshot()?;
        let observation = completed(&value).then_some(SourceObservation(observed));
        Ok((value, observation))
    }

    pub(crate) fn wait(
        finder: &Finder,
        observed: u64,
        deadline: Deadline,
        stop: impl Stop,
    ) -> (bool, Option<SourceObservation>) {
        Self::observe_wait(
            || finder.changes.generation(),
            || finder.wait_for_sources_since(observed, deadline, stop),
        )
    }

    // A successful wait acknowledges the generation read after its report.
    fn observe_wait(
        generation: impl FnOnce() -> u64,
        wait: impl FnOnce() -> bool,
    ) -> (bool, Option<SourceObservation>) {
        let changed = wait();
        let observation = changed.then(|| SourceObservation(generation()));
        (changed, observation)
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
    cursor: SourceCursor,
}

impl fmt::Debug for SourceWatch<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceWatch")
            .field("finder", &self.finder)
            .finish_non_exhaustive()
    }
}

impl SourceWatch<'_> {
    /// Copy the currently known sources and record the generation before
    /// copying, preserving any report that arrives during the snapshot.
    /// A failed snapshot leaves the cursor unchanged.
    ///
    /// # Errors
    ///
    /// Returns the snapshot errors of [`Finder::current_sources`]:
    /// [`Error::UnexpectedSdkResult`] for a malformed top-level descriptor array,
    /// [`Error::AllocationFailed`] when result or text storage cannot be reserved,
    /// or [`Error::ResourceLimit`] when a snapshot exceeds its copy budgets.
    pub fn current_sources(&mut self) -> Result<Vec<Source>> {
        self.snapshot(Finder::current_sources, |_| true)
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
    #[doc(alias = "NDIlib_find_wait_for_sources")]
    pub fn wait_for_change(&mut self, timeout: Duration) -> Result<bool> {
        Ok(self.wait_until(Deadline::after(timeout)?, NeverStop))
    }

    fn current_sources_until<S: Stop>(
        &mut self,
        deadline: Deadline,
        stop: S,
        select: impl Fn(&BorrowedSource<'_>) -> Result<bool>,
    ) -> StopResult<Option<Vec<Source>>, S> {
        self.snapshot(
            |finder| finder.current_sources_until(deadline, stop, select),
            |sources| matches!(sources, Ok(Some(_))),
        )
    }

    fn snapshot<T>(
        &mut self,
        snapshot: impl FnOnce(&Finder) -> Result<T>,
        completed: impl FnOnce(&T) -> bool,
    ) -> Result<T> {
        let (value, observation) = SourceCursor::snapshot(self.finder, snapshot, completed)?;
        self.cursor.commit(observation);
        Ok(value)
    }

    fn wait_until(&mut self, deadline: Deadline, stop: impl Stop) -> bool {
        let (changed, observation) =
            SourceCursor::wait(self.finder, self.cursor.observed(), deadline, stop);
        self.cursor.commit(observation);
        changed
    }
}

pub(crate) struct BorrowedSource<'a> {
    name: LossyText<'a>,
    address: Option<LossyText<'a>>,
}

impl BorrowedSource<'_> {
    fn with_key<T>(&self, inspect: impl FnOnce((&str, Option<&str>)) -> T) -> Result<T> {
        fn text<'a>(value: &LossyText<'a>) -> Result<Cow<'a, str>> {
            value.as_str().map_or_else(
                || {
                    value
                        .copy()
                        .map(Cow::Owned)
                        .map_err(|_| budget::allocation_failed())
                },
                |text| Ok(Cow::Borrowed(text)),
            )
        }
        let name = text(&self.name)?;
        let address = self.address.as_ref().map(text).transpose()?;
        Ok(inspect((&name, address.as_deref())))
    }

    fn matches_query(&self, query: HostQuery<'_>) -> Result<bool> {
        self.with_key(|(name, address)| source_matches_query(name, address, query))
    }

    fn seen_in(&self, seen: &HashSet<SeenSource>) -> Result<bool> {
        self.with_key(|key| seen.contains(&key as &dyn SourceIdentity))
    }
}

mod descriptor_sealed {
    pub trait Sealed {}
}

/// A descriptor's bounded-text policy. The decoder alone owns null handling,
/// selection order and source construction.
pub(crate) trait DescriptorText: descriptor_sealed::Sealed {
    const ALLOCATION_KIND: crate::AllocationKind;

    fn empty_source_name(&mut self) -> Result<()>;

    /// # Safety
    /// The non-null pointer remains readable through its first NUL or the SDK
    /// string bound, for the returned lifetime.
    unsafe fn view<'a>(&mut self, pointer: *const c_char) -> Result<Option<LossyText<'a>>>;
}

impl descriptor_sealed::Sealed for SnapshotBudget {}
impl DescriptorText for SnapshotBudget {
    const ALLOCATION_KIND: crate::AllocationKind = crate::AllocationKind::DiscoveryResults;

    fn empty_source_name(&mut self) -> Result<()> {
        self.charge_empty_source_name()
    }

    unsafe fn view<'a>(&mut self, pointer: *const c_char) -> Result<Option<LossyText<'a>>> {
        // SAFETY: forwarded to the decoder's bounded string obligation.
        unsafe { self.source_text_view(pointer) }
    }
}

struct SingleSourceText(sys::table::Called<()>);
impl descriptor_sealed::Sealed for SingleSourceText {}
impl DescriptorText for SingleSourceText {
    const ALLOCATION_KIND: crate::AllocationKind = crate::AllocationKind::SourceDescriptor;

    fn empty_source_name(&mut self) -> Result<()> {
        Ok(())
    }

    unsafe fn view<'a>(&mut self, pointer: *const c_char) -> Result<Option<LossyText<'a>>> {
        // SAFETY: the descriptor caller keeps each non-null string readable.
        let text = unsafe { crate::sdk_copy::sdk_text(self.0.map(|()| pointer)) }?;
        Ok(Some(LossyText::new(text)))
    }
}

/// Decode a source through one view-then-build policy. Both null pointers name
/// no source; a null name beside an address becomes the shared empty name.
/// Rejected entries retain all text charges but allocate no source storage.
///
/// # Safety
/// Each non-null pointer remains readable through its first NUL or
/// `MAX_SDK_STRING_BYTES`, whichever comes first, until selection and copying
/// finish. The policy must preserve that lifetime.
pub(crate) unsafe fn decode_source<T: DescriptorText>(
    name: *const c_char,
    address: *const c_char,
    text: &mut T,
    select: impl FnOnce(&BorrowedSource<'_>) -> Result<bool>,
) -> Result<Option<Source>> {
    if name.is_null() && address.is_null() {
        return Ok(None);
    }
    let name = if name.is_null() {
        text.empty_source_name()?;
        LossyText::new(c"")
    } else {
        // SAFETY: forwarded to the caller's bounded string obligation.
        let Some(name) = (unsafe { text.view(name) })? else {
            return Ok(None);
        };
        name
    };
    let address = if address.is_null() {
        None
    } else {
        // SAFETY: forwarded to the caller's bounded string obligation.
        let Some(address) = (unsafe { text.view(address) })? else {
            return Ok(None);
        };
        Some(address)
    };
    let source = BorrowedSource { name, address };
    if !select(&source)? {
        return Ok(None);
    }
    Ok(Some(Source::from_text(
        SourceText::from_sdk(source.name, T::ALLOCATION_KIND)?,
        source
            .address
            .map(|address| SourceText::from_sdk(address, T::ALLOCATION_KIND).map(SourceAddress))
            .transpose()?,
    )))
}

/// The descriptor's address is one pointer, whether the SDK calls it an IP
/// address or a URL. Decoding classifies the copied text by its scheme.
fn source_pointers(source: &NDIlib_source_t) -> (*const c_char, *const c_char) {
    // SAFETY: both union fields are pointers of the same type.
    let address = unsafe { source.__bindgen_anon_1.p_url_address };
    (source.p_ndi_name, address)
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
    /// Returns [`Error::InvalidInput`] when either list is empty or a group or
    /// extra address is empty or contains a comma or NUL, [`Error::UnsupportedCapability`] when the options
    /// carry an Advanced configuration that the runtime or this build cannot
    /// take, and [`Error::NativeOperationFailed`] when the SDK creates no
    /// finder.
    #[doc(alias = "NDIlib_find_create_v2")]
    #[doc(alias = "NDIlib_find_create_v3")]
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
                #[cfg(feature = "advanced")]
                None,
                |core, raw| core.named().NDIlib_find_create_v2(raw),
            )
        }
    }

    /// The one creation path: create a finder through `create`, which is handed the core table and the native
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
        #[cfg(feature = "advanced")] config: Option<crate::advanced::AdvancedConfig>,
        create: impl FnOnce(
            &sys::Core,
            &NDIlib_find_create_t,
        ) -> sys::table::Called<NDIlib_find_instance_t>,
    ) -> Result<Self> {
        let groups = list_cstring(settings.groups.as_deref(), "groups")?;
        let extra_ips = list_cstring(settings.extra_ips.as_deref(), "extra_ips")?;
        let sdk = ndi.bind_core();
        let instance = create(
            &sdk,
            &NDIlib_find_create_t {
                show_local_sources: settings.show_local_sources,
                p_groups: groups.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
                p_extra_ips: extra_ips.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
            },
        )
        .created()?
        .as_ptr();
        let finder = Self {
            instance: FindInstance(instance),
            sources_lock: parking_lot::Mutex::new(FindInstance(instance)),
            changes: ChangeFeed::default(),
            _groups: groups,
            _extra_ips: extra_ips,
            #[cfg(feature = "advanced")]
            _config: config,
            sdk,
        };
        // Some standard-library mutexes allocate their native backing on the
        // first lock. Initialize before exposure, while Finder already owns
        // the native instance so Drop also cleans it up if initialization panics.
        let _ = finder.changes.generation();
        Ok(finder)
    }

    /// Copy one snapshot through the entry point's shared lifetime contract.
    /// The guard carries the handle it protects, so fetching cannot pair a
    /// different finder's instance with this lock.
    #[cfg(any(test, all(feature = "advanced", ndi_has_advanced_source_metadata)))]
    pub(crate) fn snapshot<S: FinderSnapshot>(&self, snapshot: &S) -> Result<Vec<S::Item>> {
        let Ok(guard) = self.lock_snapshots(NeverStop);
        guard.snapshot(snapshot)
    }

    fn lock_snapshots<S: Stop>(
        &self,
        stop: S,
    ) -> std::result::Result<SnapshotGuard<'_>, S::Stopped> {
        let Ok(guard) = self.lock_snapshots_with(UnboundedSnapshot, stop, |remaining| {
            self.sources_lock.try_lock_for(remaining)
        })?;
        Ok(guard)
    }

    fn lock_snapshots_until<S: Stop>(
        &self,
        deadline: Deadline,
        stop: S,
    ) -> std::result::Result<Option<SnapshotGuard<'_>>, S::Stopped> {
        self.lock_snapshots_with(deadline, stop, |remaining| {
            self.sources_lock.try_lock_for(remaining)
        })
        .map(std::result::Result::ok)
    }

    // The acquisition is injectable so cancellation can be tested exactly
    // when a contended lock becomes available, without scheduler timing.
    fn lock_snapshots_with<'finder, W: SnapshotWait, S: Stop>(
        &'finder self,
        wait: W,
        stop: S,
        mut acquire: impl FnMut(Duration) -> Option<parking_lot::MutexGuard<'finder, FindInstance>>,
    ) -> std::result::Result<std::result::Result<SnapshotGuard<'finder>, W::Expired>, S::Stopped>
    {
        let instance = if S::CAN_STOP {
            loop {
                stop.check()?;
                if let Some(instance) = acquire(wait.remaining()) {
                    // Cancellation may arrive while the lock is waiting. Do
                    // not start a new native snapshot after that wait succeeds.
                    stop.check()?;
                    break instance;
                }
                stop.check()?;
                if let Some(expired) = wait.expiration() {
                    return Ok(Err(expired));
                }
            }
        } else {
            match wait.lock(&self.sources_lock) {
                Ok(instance) => instance,
                Err(expired) => return Ok(Err(expired)),
            }
        };
        Ok(Ok(SnapshotGuard { instance }))
    }

    #[cfg(any(feature = "tokio", feature = "smol"))]
    pub(crate) fn generation(&self) -> u64 {
        self.changes.generation()
    }

    pub(crate) fn current_sources_stoppable<S: Stop>(&self, stop: S) -> StopResult<Vec<Source>, S> {
        match self.lock_snapshots(stop) {
            Ok(guard) => guard
                .snapshot(&SelectingSnapshot {
                    core: &self.sdk,
                    select: |_: &BorrowedSource<'_>| Ok(true),
                })
                .map(Ok),
            Err(stopped) => Ok(Err(stopped)),
        }
    }

    /// The runtime this finder was created through.
    #[cfg(feature = "advanced")]
    pub(crate) fn runtime(&self) -> &Ndi {
        self.sdk.ndi()
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
            cursor: SourceCursor::new(self.changes.generation()),
        }
    }

    /// Share one native report with every observer of the earlier generation.
    pub(crate) fn wait_for_sources_since(
        &self,
        generation: u64,
        deadline: Deadline,
        stop: impl Stop,
    ) -> bool {
        // SAFETY: the finder instance is live for this borrow.
        self.changes
            .wait_since(generation, deadline, stop, |timeout_ms| unsafe {
                self.sdk
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
    /// # Errors
    ///
    /// Returns [`Error::UnexpectedSdkResult`] with
    /// [`crate::SdkResultDefect::MalformedDiscoveryData`] for a malformed top-level
    /// descriptor array, [`Error::AllocationFailed`] when result or text storage
    /// cannot be reserved, or [`Error::ResourceLimit`] when a snapshot exceeds
    /// its copy budgets. An isolated malformed descriptor is skipped.
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
    #[doc(alias = "NDIlib_find_get_current_sources")]
    pub fn current_sources(&self) -> Result<Vec<Source>> {
        let Ok(sources) = self.current_sources_stoppable(NeverStop)?;
        Ok(sources)
    }

    /// The lookup's next snapshot, or none when contention exhausts its budget.
    fn current_sources_until<S: Stop>(
        &self,
        deadline: Deadline,
        stop: S,
        select: impl Fn(&BorrowedSource<'_>) -> Result<bool>,
    ) -> StopResult<Option<Vec<Source>>, S> {
        match self.lock_snapshots_until(deadline, stop) {
            Ok(Some(guard)) => guard
                .snapshot(&SelectingSnapshot {
                    core: &self.sdk,
                    select,
                })
                .map(|sources| Ok(Some(sources))),
            Ok(None) => Ok(Ok(None)),
            Err(stopped) => Ok(Err(stopped)),
        }
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
    fn poll_until_deadline<T, S: Stop, A>(
        &self,
        deadline: Deadline,
        stop: S,
        state: &mut A,
        select: impl Fn(&A, &BorrowedSource<'_>) -> Result<bool>,
        mut accept: impl FnMut(&mut A, Vec<Source>) -> Result<Option<T>>,
    ) -> StopResult<Option<T>, S> {
        let mut watch = self.watch();
        loop {
            if let Err(stopped) = stop.check() {
                return Ok(Err(stopped));
            }
            let sources = match watch
                .current_sources_until(deadline, stop, |source| select(state, source))?
            {
                Ok(Some(sources)) => sources,
                Ok(None) => return Ok(Ok(None)),
                Err(stopped) => return Ok(Err(stopped)),
            };
            if let Some(selected) = accept(state, sources)? {
                return Ok(Ok(Some(selected)));
            }
            if deadline.is_expired() {
                return Ok(Ok(None));
            }
            // Block until the source list changes or the window ends; either
            // way the next iteration re-snapshots. The wait is rounded up, so
            // the last one lasts until the deadline rather than polling.
            watch.wait_until(deadline, stop);
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
        select: impl FnMut(&Source) -> bool,
    ) -> Result<Option<Source>> {
        let Ok(source) = self.find_source_until(Deadline::after(timeout)?, NeverStop, select)?;
        Ok(source)
    }

    pub(crate) fn find_source_until<S: Stop>(
        &self,
        deadline: Deadline,
        stop: S,
        mut select: impl FnMut(&Source) -> bool,
    ) -> StopResult<Option<Source>, S> {
        self.poll_until_deadline(
            deadline,
            stop,
            &mut (),
            |_, _| Ok(true),
            |_, sources| Ok(sources.into_iter().find(|source| select(source))),
        )
    }

    /// Wait for an exact, case-sensitive complete NDI source name.
    ///
    /// Names normally look like `"STUDIO (Camera 2)"`. Duplicate names at different
    /// addresses select the first match in the current snapshot; use
    /// [`Self::find_source`] with an address predicate to disambiguate them.
    /// Timeout, concurrency and error behavior match that method. Only matching
    /// descriptors are copied from each bounded snapshot.
    ///
    /// # Errors
    ///
    /// An empty name returns [`Error::InvalidInput`]; a name containing a NUL
    /// returns [`Error::InvalidCString`]. The name is checked before the timeout,
    /// including in async lookups. Other errors match [`Self::find_source`].
    pub fn find_source_by_name(&self, name: &str, timeout: Duration) -> Result<Option<Source>> {
        let (name, deadline) = checked_name_lookup(name, timeout)?;
        let Ok(source) = self.find_source_by_name_until(name.as_str(), deadline, NeverStop)?;
        Ok(source)
    }

    pub(crate) fn find_source_by_name_until<S: Stop>(
        &self,
        name: &str,
        deadline: Deadline,
        stop: S,
    ) -> StopResult<Option<Source>, S> {
        self.poll_until_deadline(
            deadline,
            stop,
            &mut (),
            |_, source| Ok(source.name.equals(name)),
            |_, sources| Ok(sources.into_iter().next()),
        )
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
    /// Identity includes the name and the complete address text;
    /// a changed descriptor is returned as a distinct source. Network discovery
    /// cannot guarantee observation of every source. May be empty if none appear.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] if `timeout` exceeds [`crate::MAX_TIMEOUT`].
    /// Returns [`Error::ResourceLimit`] if a snapshot exceeds the limits of
    /// [`Self::current_sources`], or distinct results retained across the
    /// whole window exceed 16,384 entries or 16 MiB. The retained byte budget
    /// counts shared source strings once, plus the storage of both the result
    /// vector and the deduplication set, including container capacity.
    /// Returns [`Error::UnexpectedSdkResult`] for a malformed top-level snapshot
    /// array, or [`Error::AllocationFailed`] when result or text storage cannot
    /// be reserved. An isolated malformed descriptor is skipped.
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
        let Ok(sources) = self.find_sources_until(Deadline::after(timeout)?, NeverStop)?;
        Ok(sources)
    }

    pub(crate) fn find_sources_until<S: Stop>(
        &self,
        deadline: Deadline,
        stop: S,
    ) -> StopResult<Vec<Source>, S> {
        self.find_selected_sources_until(deadline, stop, None)
    }

    fn find_selected_sources_until<S: Stop>(
        &self,
        deadline: Deadline,
        stop: S,
        query: Option<HostQuery<'_>>,
    ) -> StopResult<Vec<Source>, S> {
        // Preserve first-observed order and deduplicate by complete descriptor.
        let mut state = (Vec::new(), HashSet::new(), 0);
        let outcome = self.poll_until_deadline(
            deadline,
            stop,
            &mut state,
            |(_, seen, _), source| {
                if let Some(query) = query {
                    if !source.matches_query(query)? {
                        return Ok(false);
                    }
                }
                Ok(!source.seen_in(seen)?)
            },
            |(discovered, seen, retained_bytes), sources| {
                accumulate_sources(discovered, seen, retained_bytes, sources)?;
                Ok(None::<()>)
            },
        )?;
        Ok(outcome.map(|_| state.0))
    }
}

impl Drop for Finder {
    fn drop(&mut self) {
        // SAFETY: the instance belongs to this runtime and is destroyed once.
        unsafe { self.sdk.NDIlib_find_destroy(self.instance.0) };
    }
}

const_assert!({
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Finder>();
});

/// An exact, nonempty NDI source name with no interior NUL. Ownership can
/// change without checking the same text again.
#[derive(Debug, Clone)]
pub(crate) struct SourceName<'a>(&'a str);

impl<'a> SourceName<'a> {
    pub(crate) fn checked(name: &'a str) -> Result<Self> {
        if name.is_empty() {
            return Err(Error::InvalidInput(crate::InputDefect::Empty {
                what: "source name",
            }));
        }
        if name.contains('\0') {
            #[expect(
                clippy::unwrap_used,
                reason = "contains proved an interior NUL; NulError has no public constructor"
            )]
            return Err(CString::new(name).unwrap_err().into());
        }
        Ok(Self(name))
    }

    pub(crate) fn as_str(&self) -> &str {
        self.0
    }

    #[cfg(any(feature = "tokio", feature = "smol"))]
    pub(crate) fn into_owned(self) -> OwnedSourceName {
        OwnedSourceName(self.0.to_owned())
    }

    pub(crate) fn into_c_string(self) -> CString {
        // Reserve the terminator as well so converting a borrowed name needs
        // one allocation, just as CString::new does.
        let mut bytes = Vec::with_capacity(self.0.len() + 1);
        bytes.extend_from_slice(self.0.as_bytes());
        // SAFETY: checked excludes every NUL, and ownership changes preserve
        // those exact bytes. CString appends the only, terminating NUL.
        unsafe { CString::from_vec_unchecked(bytes) }
    }
}

/// The checked name crossing async dispatch has no borrowed representation or
/// lifetime parameter. Rust 1.88 can prove its worker future is Send directly.
#[cfg(any(feature = "tokio", feature = "smol"))]
#[derive(Debug, Clone)]
pub(crate) struct OwnedSourceName(String);

#[cfg(any(feature = "tokio", feature = "smol"))]
impl OwnedSourceName {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Check lookup arguments in their public argument order, before any snapshot,
/// cache access or async dispatch.
pub(crate) fn checked_name_lookup(
    name: &str,
    timeout: Duration,
) -> Result<(SourceName<'_>, Deadline)> {
    let name = SourceName::checked(name)?;
    Ok((name, Deadline::after(timeout)?))
}

pub(crate) fn checked_host_lookup(
    host: &str,
    timeout: Duration,
) -> Result<(HostQuery<'_>, Deadline)> {
    let host = HostQuery::checked(host)?;
    Ok((host, Deadline::after(timeout)?))
}

fn accumulate_sources(
    discovered: &mut Vec<Source>,
    seen: &mut HashSet<SeenSource>,
    retained_bytes: &mut usize,
    sources: Vec<Source>,
) -> Result<()> {
    for source in sources {
        if seen.contains(&source as &dyn SourceIdentity) {
            continue;
        }
        if discovered.len() == budget::MAX_SOURCES {
            return Err(budget::limit(
                ResourceKind::FinderDiscoverySources,
                budget::MAX_SOURCES,
            ));
        }
        let text_bytes = source.name.storage_bytes()
            + source
                .address
                .as_ref()
                .map_or(0, |address| address.0.storage_bytes());
        let bytes = retained_bytes
            .checked_add(text_bytes)
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
        seen.insert(SeenSource(source.clone()));
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
pub(crate) enum HostQuery<'a> {
    Ip(IpAddr),
    Host(&'a str),
    Machine(&'a str),
}

impl<'a> HostQuery<'a> {
    pub(crate) fn checked(host: &'a str) -> Result<Self> {
        Self::parse(host).ok_or(Error::InvalidInput(crate::InputDefect::InvalidHostQuery))
    }

    #[cfg(any(feature = "tokio", feature = "smol"))]
    pub(crate) fn into_owned(self) -> OwnedHostQuery {
        match self {
            Self::Ip(ip) => OwnedHostQuery::Ip(ip),
            Self::Host(name) => OwnedHostQuery::Host(name.to_owned()),
            Self::Machine(name) => OwnedHostQuery::Machine(name.to_owned()),
        }
    }

    fn parse(host: &'a str) -> Option<Self> {
        let ip_text = if host.starts_with('[') {
            host.strip_prefix('[')?.strip_suffix(']')?
        } else {
            host
        };
        if let Ok(ip) = ip_text.parse::<IpAddr>() {
            return Some(Self::Ip(ip));
        }
        if host != ip_text {
            return None;
        }
        let name = host.strip_suffix('.').unwrap_or(host);
        if name.is_empty()
            || name.len() > 253
            || name.trim() != name
            || name.chars().any(char::is_control)
            || name.contains([':', '/', '?', '#', '@', '[', ']', '(', ')'])
            || (name.contains('.') && name.bytes().all(|c| c.is_ascii_digit() || c == b'.'))
        {
            return None;
        }
        if name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        }) {
            Some(Self::Host(name))
        } else {
            Some(Self::Machine(name))
        }
    }

    fn matches(self, candidate: &str) -> bool {
        match self {
            Self::Ip(ip) => candidate.parse::<IpAddr>() == Ok(ip),
            Self::Host(name) | Self::Machine(name) => candidate
                .strip_suffix('.')
                .unwrap_or(candidate)
                .eq_ignore_ascii_case(name),
        }
    }
}

fn address_matches_query(address: &str, query: HostQuery<'_>) -> bool {
    !matches!(query, HostQuery::Machine(_))
        && parse_authority(address).is_some_and(|authority| query.matches(authority.host))
}

fn source_matches_query(name: &str, address: Option<&str>, query: HostQuery<'_>) -> bool {
    if address.is_some_and(|address| address_matches_query(address, query)) {
        return true;
    }
    match query {
        HostQuery::Ip(_) => false,
        HostQuery::Host(_) | HostQuery::Machine(_) => name
            .split_once(" (")
            .filter(|(_, source)| source.ends_with(')'))
            .is_some_and(|(machine, _)| query.matches(machine)),
    }
}

/// A parsed query that can cross a blocking-pool dispatch without reparsing
/// text or borrowing the async caller's string.
#[cfg(any(feature = "tokio", feature = "smol"))]
pub(crate) enum OwnedHostQuery {
    Ip(IpAddr),
    Host(String),
    Machine(String),
}

#[cfg(any(feature = "tokio", feature = "smol"))]
impl OwnedHostQuery {
    pub(crate) fn as_query(&self) -> HostQuery<'_> {
        match self {
            Self::Ip(ip) => HostQuery::Ip(*ip),
            Self::Host(name) => HostQuery::Host(name),
            Self::Machine(name) => HostQuery::Machine(name),
        }
    }
}

/// A retained cache identity keeps numeric addresses separate from names.
#[derive(Debug, PartialEq, Eq)]
enum HostKey {
    Ip(IpAddr),
    Name(Box<str>),
}

/// Owned keys and borrowed queries share identity without allocating a key.
pub(crate) trait HostIdentity {
    fn identity(&self) -> HostQuery<'_>;
}

impl HostIdentity for HostQuery<'_> {
    fn identity(&self) -> HostQuery<'_> {
        *self
    }
}

impl HostIdentity for HostKey {
    fn identity(&self) -> HostQuery<'_> {
        match self {
            Self::Ip(ip) => HostQuery::Ip(*ip),
            Self::Name(name) => HostQuery::Host(name),
        }
    }
}

impl Hash for dyn HostIdentity + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self.identity() {
            HostQuery::Ip(ip) => {
                0u8.hash(state);
                ip.hash(state);
            }
            HostQuery::Host(name) | HostQuery::Machine(name) => {
                1u8.hash(state);
                name.len().hash(state);
                for byte in name.bytes() {
                    byte.to_ascii_lowercase().hash(state);
                }
            }
        }
    }
}

impl PartialEq for dyn HostIdentity + '_ {
    fn eq(&self, other: &Self) -> bool {
        match (self.identity(), other.identity()) {
            (HostQuery::Ip(a), HostQuery::Ip(b)) => a == b,
            (
                HostQuery::Host(a) | HostQuery::Machine(a),
                HostQuery::Host(b) | HostQuery::Machine(b),
            ) => a.eq_ignore_ascii_case(b),
            _ => false,
        }
    }
}
impl Eq for dyn HostIdentity + '_ {}

impl Hash for HostKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (self as &dyn HostIdentity).hash(state);
    }
}

impl<'a> Borrow<dyn HostIdentity + 'a> for HostKey {
    fn borrow(&self) -> &(dyn HostIdentity + 'a) {
        self
    }
}

impl TryFrom<HostQuery<'_>> for HostKey {
    type Error = Error;

    fn try_from(query: HostQuery<'_>) -> Result<Self> {
        Ok(match query {
            HostQuery::Ip(ip) => Self::Ip(ip),
            HostQuery::Host(name) | HostQuery::Machine(name) => {
                let mut name = budget::copy_string(name)?;
                name.make_ascii_lowercase();
                Self::Name(name.into_boxed_str())
            }
        })
    }
}

/// Opaque network address text of an NDI source.
///
/// Construction refuses NUL bytes and keeps the native representation ready
/// for connection requests. Equality and hashing compare the exact text the SDK
/// receives. URL classification does not establish the codec or NDI|HX use.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceAddress(SourceText);

/// Validated UTF-8 with exactly one trailing NUL, shared by descriptors and
/// their native borrowers. All SDK pointers lead into this immutable storage.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct SourceText(Arc<CString>);

impl SourceText {
    // Shared CString storage adds its Arc header, the CString and one NUL.
    pub(crate) const STORAGE_OVERHEAD: usize =
        2 * std::mem::size_of::<usize>() + std::mem::size_of::<CString>() + 1;

    /// Copy classified SDK text directly into its final shared native storage.
    pub(crate) fn from_sdk(source: LossyText<'_>, kind: crate::AllocationKind) -> Result<Self> {
        let mut text = String::new();
        let bytes = source.len() + 1;
        text.try_reserve_exact(bytes)
            .map_err(|_| crate::error::allocation_failed(kind, Some(bytes)))?;
        source.push_to(&mut text);
        // SAFETY: LossyText carries the NUL-free representation invariant.
        Ok(unsafe { Self::from_nul_free_string(text) })
    }

    /// Keep generated text whose NUL-free invariant is already established.
    ///
    /// # Safety
    /// `text` must contain no NUL.
    unsafe fn from_nul_free_string(text: String) -> Self {
        let mut bytes = text.into_bytes();
        bytes.push(0);
        // SAFETY: the caller excludes interior NUL and we appended the sole NUL.
        Self(Arc::new(unsafe {
            CString::from_vec_with_nul_unchecked(bytes)
        }))
    }

    pub(crate) fn with_port(&self, port: u16) -> Self {
        let address = self.as_str();
        let endpoint = if address.parse::<std::net::Ipv6Addr>().is_ok() {
            format!("[{address}]:{port}")
        } else {
            format!("{address}:{port}")
        };
        // SAFETY: SourceText contains no interior NUL, and formatting adds
        // only brackets, a colon and decimal digits.
        unsafe { Self::from_nul_free_string(endpoint) }
    }

    pub(crate) fn new(text: String) -> Result<Self> {
        Ok(Self(Arc::new(CString::new(text)?)))
    }

    pub(crate) fn as_str(&self) -> &str {
        // SAFETY: the CString was constructed from a String, and its immutable
        // shared backing cannot change after construction.
        unsafe { std::str::from_utf8_unchecked(self.0.as_bytes()) }
    }

    fn as_ptr(&self) -> *const c_char {
        self.0.as_ptr()
    }

    fn storage_bytes(&self) -> usize {
        self.0.as_bytes().len() + Self::STORAGE_OVERHEAD
    }
}

impl fmt::Debug for SourceText {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl SourceAddress {
    pub(crate) fn from_text(text: SourceText) -> Self {
        Self(text)
    }

    /// Keep the opaque address text unchanged after checking its native form.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] when the address contains a NUL byte.
    pub fn new(text: impl Into<String>) -> Result<Self> {
        SourceText::new(text.into()).map(Self)
    }

    /// The exact address text, whether it names a host, IP address or URL.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Whether the address contains a URL scheme separator (`://`).
    pub fn is_url(&self) -> bool {
        self.as_str().contains("://")
    }

    /// Check whether this address has the exact host or IP.
    ///
    /// Hostnames compare without ASCII case or a final DNS dot. The query
    /// grammar is defined by [`Source::matches_host`]. Machine-name queries
    /// and invalid queries return false because an address has no machine label.
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
    /// let addr = SourceAddress::new("192.168.1.100:5960".to_string()).unwrap();
    /// assert!(addr.matches_host("192.168.1.100"));
    /// assert!(!addr.matches_host("192.168.1"));
    ///
    /// let url = SourceAddress::new("http://camera.local:8080".to_string()).unwrap();
    /// assert!(url.matches_host("camera.local"));
    /// ```
    pub fn matches_host(&self, host: &str) -> bool {
        HostQuery::parse(host).is_some_and(|query| self.matches_query(query))
    }

    fn matches_query(&self, query: HostQuery<'_>) -> bool {
        address_matches_query(self.as_str(), query)
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
    /// let addr = SourceAddress::new("192.168.1.100:5960".to_string()).unwrap();
    /// assert_eq!(addr.port(), Some(5960));
    ///
    /// let no_port = SourceAddress::new("192.168.1.100".to_string()).unwrap();
    /// assert_eq!(no_port.port(), None);
    ///
    /// let url = SourceAddress::new("http://camera.local:8080".to_string()).unwrap();
    /// assert_eq!(url.port(), Some(8080));
    /// ```
    pub fn port(&self) -> Option<u16> {
        self.authority().and_then(|authority| authority.port)
    }

    // SDK addresses are opaque. Only interpret conventional URL authorities and
    // host[:port] forms; never inspect their path, query, or fragment as a port.
    fn authority(&self) -> Option<Authority<'_>> {
        parse_authority(self.as_str())
    }
}

/// An NDI source: its name and, when known, its network address.
///
/// Equality and hashing compare the complete name and address text; they do
/// not imply persistent device identity.
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
/// let source = Source::new("LAPTOP (Camera 1)").unwrap()
///     .with_address(SourceAddress::new("192.168.1.100:5960".to_string()).unwrap());
/// assert_eq!(source.name(), "LAPTOP (Camera 1)");
/// assert_eq!(source.to_string(), "LAPTOP (Camera 1)@192.168.1.100:5960");
/// assert!(Source::new("LAPTOP (Camera 1)").unwrap().address().is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Source {
    name: SourceText,
    address: Option<SourceAddress>,
}

/// Only discovery's private deduplication set needs borrowed textual keys.
/// Keep its custom Borrow and hashing separate from the public source value.
#[repr(transparent)]
#[derive(Clone, PartialEq, Eq)]
struct SeenSource(Source);

const_assert!(std::mem::size_of::<SeenSource>() == std::mem::size_of::<Source>());

/// Owned and borrowed source keys use the same complete textual identity.
/// Trait-object borrowing permits a HashSet lookup without allocating a Source.
trait SourceIdentity {
    fn key(&self) -> (&str, Option<&str>);
}

impl SourceIdentity for Source {
    fn key(&self) -> (&str, Option<&str>) {
        (
            self.name(),
            self.address.as_ref().map(SourceAddress::as_str),
        )
    }
}

impl SourceIdentity for (&str, Option<&str>) {
    fn key(&self) -> (&str, Option<&str>) {
        *self
    }
}

impl SourceIdentity for SeenSource {
    fn key(&self) -> (&str, Option<&str>) {
        self.0.key()
    }
}

impl Hash for SeenSource {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

impl Hash for dyn SourceIdentity + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

impl PartialEq for dyn SourceIdentity + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for dyn SourceIdentity + '_ {}

impl<'a> Borrow<dyn SourceIdentity + 'a> for SeenSource {
    fn borrow(&self) -> &(dyn SourceIdentity + 'a) {
        self
    }
}

#[repr(C)]
pub(crate) struct RawSource {
    _source: Source,
    raw: NDIlib_source_t,
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

impl RawSource {
    /// Lend the descriptor only while its backing C strings remain owned.
    pub(crate) fn as_raw(&self) -> &NDIlib_source_t {
        &self.raw
    }
}

impl Source {
    pub(crate) fn from_text(name: SourceText, address: Option<SourceAddress>) -> Self {
        Self { name, address }
    }

    /// The source named `name`, without an address.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidCString`] when the name contains a NUL byte.
    pub fn new(name: impl Into<String>) -> Result<Self> {
        Ok(Self {
            name: SourceText::new(name.into())?,
            address: None,
        })
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
        self.name.as_str()
    }

    /// The network address for connecting to this source, when known.
    pub fn address(&self) -> Option<&SourceAddress> {
        self.address.as_ref()
    }

    /// Check if this source matches a hostname, IP address or NDI machine name.
    ///
    /// An IP query checks the complete address host numerically; IP literals may
    /// have enclosing square brackets. A DNS hostname checks both the address
    /// host and the machine component of an SDK
    /// `MACHINE (Source Name)` label. Other machine-name queries check only that
    /// machine component, never the label's source name or an address.
    ///
    /// Names are nonempty and at most 253 bytes after an optional final dot is
    /// removed. They have no control characters, leading or trailing whitespace,
    /// endpoint/URI delimiters (`: / ? # @ [ ]`), or label delimiters (`( )`).
    /// Dotted-numeric text must be a complete IP address. DNS hostnames use
    /// ASCII letters, digits and interior hyphens in labels of at most 63 bytes;
    /// other accepted names, including spaces and underscores, are machine names.
    /// Both name cases ignore a final dot and fold only ASCII letters: other
    /// Unicode characters compare exactly. Invalid queries return false.
    ///
    /// # Arguments
    ///
    /// * `host` - The hostname, IP address or machine name to match against
    ///
    /// # Examples
    ///
    /// ```
    /// use grafton_ndi::{Source, SourceAddress};
    ///
    /// let source = Source::new("CAMERA1 (Chan1, 192.168.0.107)").unwrap()
    ///     .with_address(SourceAddress::new("192.168.0.107:5960".to_string()).unwrap());
    ///
    /// assert!(source.matches_host("192.168.0.107"));
    /// assert!(source.matches_host("CAMERA1"));
    /// assert!(!source.matches_host("192.168.1.1"));
    /// ```
    pub fn matches_host(&self, host: &str) -> bool {
        HostQuery::parse(host).is_some_and(|query| self.matches_query(query))
    }

    fn matches_query(&self, query: HostQuery<'_>) -> bool {
        source_matches_query(
            self.name(),
            self.address.as_ref().map(SourceAddress::as_str),
            query,
        )
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
    /// let source = Source::new("CAMERA1").unwrap()
    ///     .with_address(SourceAddress::new("192.168.1.100:5960".to_string()).unwrap());
    ///
    /// assert_eq!(source.host(), Some("192.168.1.100"));
    /// ```
    pub fn host(&self) -> Option<&str> {
        self.address
            .as_ref()?
            .authority()
            .map(|authority| authority.host)
    }

    /// Copy a source descriptor that the SDK call `source` returned
    /// into owned strings after null checks, replacing bytes that are not
    /// UTF-8.
    ///
    /// Null checks cannot validate dangling or otherwise invalid pointers. The
    /// SDK must provide a live descriptor and readable NUL-terminated strings.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NativeOperationFailed`] naming the call's function when the
    /// descriptor is null, or names neither a source nor an address, and
    /// [`Error::UnexpectedSdkResult`] when its name or address is longer than
    /// the 4 MiB one SDK string may be. [`Error::AllocationFailed`] when the
    /// source text storage cannot be reserved.
    ///
    /// # Safety
    ///
    /// The non-null pointer in `source` must point to a live `NDIlib_source_t`. Its
    /// non-null string pointers must remain readable through their NUL
    /// terminators, or for 4 MiB, until copying finishes.
    pub(crate) unsafe fn try_from_raw(
        source: sys::table::Called<*const NDIlib_source_t>,
    ) -> Result<Self> {
        let failed = || source.failed();
        let source_ptr = *source.value();
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
            decode_source(
                name,
                address,
                &mut SingleSourceText(source.map(|_| ())),
                |_| Ok(true),
            )
        }?
        .ok_or_else(failed)
    }

    /// Retain the source's validated storage and lend its native descriptor.
    /// No strings are allocated or validated again.
    pub(crate) fn to_raw(&self) -> RawSource {
        let source = self.clone();
        let raw = NDIlib_source_t {
            p_ndi_name: source.name.as_ptr(),
            __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                p_url_address: source
                    .address
                    .as_ref()
                    .map_or(ptr::null(), |address| address.0.as_ptr()),
            },
        };
        RawSource {
            _source: source,
            raw,
        }
    }
}

impl Display for Source {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match &self.address {
            Some(address) => write!(
                f,
                "{name}@{address}",
                name = self.name(),
                address = address.as_str()
            ),
            None => write!(f, "{name}", name = self.name()),
        }
    }
}

/// A thread-safe cache of complete host result sets in one finder's scope.
///
/// A host can publish several feeds. [`Self::find_sources_by_host`] retains every
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
/// results are not cached. Results that cannot be stored are returned uncached.
/// The accepted grammar is [`Source::matches_host`]'s.
/// Equivalent hostname or machine-name ASCII case/final-dot and numeric IP
/// spellings share an entry. Other spellings, including DNS aliases, remain
/// distinct identities.
///
/// The cache is `Send + Sync`. Discovery, descriptor copies and retired entry
/// destruction run outside its mutex so unrelated lookups and invalidation
/// remain available; SDK snapshots retain their normal
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
/// let finder = Finder::new(&ndi, &FinderOptions::new().groups(["Studio"])
///     .extra_ips(["192.168.0.107"]))?;
/// let cache = SourceCache::new(finder);
/// let feeds = cache.find_sources_by_host("192.168.0.107", Duration::from_secs(5))?;
/// for feed in feeds { println!("{feed}"); }
/// # Ok(())
/// # }
/// ```
pub struct SourceCache {
    finder: Finder,
    entries: CachedSources,
}

impl fmt::Debug for SourceCache {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceCache")
            .field("finder", &self.finder)
            .finish_non_exhaustive()
    }
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
    #[cfg(any(feature = "tokio", feature = "smol"))]
    pub(crate) fn cached_sources(&self, query: HostQuery<'_>) -> Result<Option<Vec<Source>>> {
        self.entries
            .probe(query, copy_sources)
            .map(|(_, sources, _)| sources)
    }

    #[cfg(all(test, any(feature = "tokio", feature = "smol")))]
    pub(crate) fn seed_for_test(&self, host: &str, sources: Vec<Source>) {
        let key = HostKey::try_from(HostQuery::checked(host).unwrap()).unwrap();
        self.entries.publish(Some(0), key, Arc::new(sources));
    }

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
    ///     let options = FinderOptions::new().groups(["Studio"]);
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

    /// All feeds observed for a hostname, IP or NDI machine name within `timeout`.
    ///
    /// A cache hit returns owned descriptor copies without an SDK call. On a
    /// miss, observes the full window using the configured finder; zero makes
    /// one non-waiting snapshot attempt. Results keep first-observed order, and
    /// sources that differ in name or full address text are distinct. Select the
    /// desired feed explicitly.
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
    /// [`Error::InvalidInput`] for a query outside [`Source::matches_host`]'s
    /// grammar or a timeout above [`crate::MAX_TIMEOUT`], even on a cache hit.
    /// The host is checked before the timeout, including in async lookups;
    /// discovery/resource errors propagate without filling an entry. A completed
    /// result the cache cannot store is returned uncached. IP hints must already
    /// be configured on the finder; this method does not change its scope.
    pub fn find_sources_by_host(&self, host: &str, timeout: Duration) -> Result<Vec<Source>> {
        let (query, deadline) = checked_host_lookup(host, timeout)?;
        let Ok(sources) = self.find_sources_by_host_until(query, deadline, NeverStop)?;
        Ok(sources)
    }

    pub(crate) fn find_sources_by_host_until<S: Stop>(
        &self,
        query: HostQuery<'_>,
        deadline: Deadline,
        stop: S,
    ) -> StopResult<Vec<Source>, S> {
        self.entries
            .find_sources_by_host_with::<S>(query, deadline, |query, deadline| {
                self.discover_host_until(query, deadline, stop)
            })
    }

    fn discover_host_until<S: Stop>(
        &self,
        query: HostQuery<'_>,
        deadline: Deadline,
        stop: S,
    ) -> StopResult<Vec<Source>, S> {
        self.finder()
            .find_selected_sources_until(deadline, stop, Some(query))
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

    /// Number of cached host result sets; each holds every feed observed for its host.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no host result sets are cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Fallibly reserve descriptor copies while sharing their immutable, validated
/// source strings. Discovery has already bounded the string storage.
fn copy_sources(sources: &[Source]) -> Result<Vec<Source>> {
    let mut copy = Vec::new();
    copy.try_reserve_exact(sources.len())
        .map_err(|_| budget::allocation_failed())?;
    for source in sources {
        copy.push(source.clone());
    }
    Ok(copy)
}

impl CachedSources {
    /// Clone the map entry under the short lock, then copy descriptors outside it.
    /// The miss generation belongs to the same lookup used for publication.
    fn probe<'a>(
        &self,
        query: HostQuery<'a>,
        copy: impl FnOnce(&[Source]) -> Result<Vec<Source>>,
    ) -> Result<(HostQuery<'a>, Option<Vec<Source>>, Option<u64>)> {
        let (snapshot, generation) = {
            let cache = self
                .cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                cache.entries.get(&query as &dyn HostIdentity).cloned(),
                cache.generation,
            )
        };
        let sources = snapshot.map(|snapshot| copy(&snapshot)).transpose()?;
        Ok((query, sources, generation))
    }

    fn find_sources_by_host_with<S: Stop>(
        &self,
        query: HostQuery<'_>,
        deadline: Deadline,
        discover: impl FnOnce(HostQuery<'_>, Deadline) -> StopResult<Vec<Source>, S>,
    ) -> StopResult<Vec<Source>, S> {
        self.find_sources_by_host_with_copy::<S>(query, deadline, discover, copy_sources)
    }

    fn find_sources_by_host_with_copy<S: Stop>(
        &self,
        query: HostQuery<'_>,
        deadline: Deadline,
        discover: impl FnOnce(HostQuery<'_>, Deadline) -> StopResult<Vec<Source>, S>,
        mut copy: impl FnMut(&[Source]) -> Result<Vec<Source>>,
    ) -> StopResult<Vec<Source>, S> {
        let (key, sources, generation) = self.probe(query, &mut copy)?;
        if let Some(sources) = sources {
            return Ok(Ok(sources));
        }
        // SDK waits and user discovery run outside the cache mutex. Invalidation
        // changes the generation, so an old result cannot repopulate the cache.
        let sources = match discover(query, deadline)? {
            Ok(sources) => sources,
            Err(stopped) => return Ok(Err(stopped)),
        };
        if !sources.is_empty() {
            {
                let cache = self
                    .cache
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if generation.is_none()
                    || cache.generation != generation
                    || cache.entries.contains_key(&key as &dyn HostIdentity)
                {
                    return Ok(Ok(sources));
                }
            }
            // The bounded, fallible descriptor copy happens outside the mutex.
            // Publication must still recheck both generation and key: another
            // discovery or invalidation may have completed during this copy.
            let Ok(snapshot) = copy(&sources) else {
                return Ok(Ok(sources));
            };
            let Ok(key) = HostKey::try_from(key) else {
                return Ok(Ok(sources));
            };
            self.publish(generation, key, Arc::new(snapshot));
        }
        Ok(Ok(sources))
    }

    // Admission cannot change the caller's discovered result. Prepare copies
    // outside the lock, then recheck ownership before reserving and inserting.
    fn publish(&self, generation: Option<u64>, key: HostKey, snapshot: Arc<Vec<Source>>) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if generation.is_some()
            && cache.generation == generation
            && !cache.entries.contains_key(&key)
            && cache.entries.try_reserve(1).is_ok()
        {
            cache.entries.insert(key, snapshot);
        }
    }

    fn invalidate(&self, host: &str) {
        let retired = {
            let mut cache = self
                .cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cache.invalidate_pending();
            HostQuery::parse(host)
                .and_then(|query| cache.entries.remove_entry(&query as &dyn HostIdentity))
        };
        // Transfer the complete entry, including its key, before reclaiming
        // strings or the final snapshot owner. Readers only wait for removal.
        drop(retired);
    }

    fn clear(&self) {
        let retired = {
            let mut cache = self
                .cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            cache.invalidate_pending();
            std::mem::take(&mut cache.entries)
        };
        // Detaching the map is constant-sized and allocation-free. Reclaim its
        // entries and high-water bucket storage after releasing the mutex.
        drop(retired);
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
    // Exercise the cache with the same checked inputs as the synchronous
    // boundary, while letting individual tests supply discovery and copying.
    impl CachedSources {
        fn lookup(
            &self,
            host: &str,
            timeout: Duration,
            discover: impl FnOnce(HostQuery<'_>) -> Result<Vec<Source>>,
        ) -> Result<Vec<Source>> {
            self.lookup_with_copy(host, timeout, discover, copy_sources)
        }

        fn lookup_with_copy(
            &self,
            host: &str,
            timeout: Duration,
            discover: impl FnOnce(HostQuery<'_>) -> Result<Vec<Source>>,
            copy: impl FnMut(&[Source]) -> Result<Vec<Source>>,
        ) -> Result<Vec<Source>> {
            let (query, deadline) = checked_host_lookup(host, timeout)?;
            let Ok(sources) = self.find_sources_by_host_with_copy::<NeverStop>(
                query,
                deadline,
                |query, _| discover(query).map(Ok),
                copy,
            )?;
            Ok(sources)
        }
    }

    unsafe fn source_from_raw(
        source: *const NDIlib_source_t,
        function: &'static std::ffi::CStr,
    ) -> Result<Source> {
        // SAFETY: forwarded to the test caller's live-descriptor obligation.
        unsafe { Source::try_from_raw(sys::table::for_test(source, function)) }
    }

    use super::*;

    fn assert_same_name_error(actual: Error, expected: &Error) {
        match (actual, expected) {
            (Error::InvalidInput(actual), Error::InvalidInput(expected)) => {
                assert_eq!(&actual, expected)
            }
            (Error::InvalidCString(actual), Error::InvalidCString(expected)) => {
                assert_eq!(&actual, expected)
            }
            (actual, expected) => panic!("different errors: {actual:?}, {expected:?}"),
        }
    }

    #[test]
    fn source_names_preserve_checked_text_and_distinguish_empty_from_nul() {
        assert_same_name_error(
            SourceName::checked("").unwrap_err(),
            &Error::InvalidInput(crate::InputDefect::Empty {
                what: "source name",
            }),
        );
        for name in ["bad\0name", "trailing\0"] {
            let expected = Error::from(CString::new(name).unwrap_err());
            assert_same_name_error(SourceName::checked(name).unwrap_err(), &expected);
            #[cfg(feature = "file-io")]
            assert_same_name_error(crate::file::SourceSpec::new(name).unwrap_err(), &expected);
        }
        let longest = "x".repeat(if cfg!(miri) {
            64
        } else {
            crate::sdk_copy::MAX_SDK_STRING_BYTES - 1
        });
        for name in ["STUDIO (Camera 1)", "Caméra (映像)", longest.as_str()] {
            let (checked, allocations) =
                crate::test_alloc::allocation_count(|| SourceName::checked(name).unwrap());
            assert_eq!(allocations, 0);
            assert_eq!(checked.as_str(), name);
            let (native, allocations) =
                crate::test_alloc::allocation_count(|| checked.clone().into_c_string());
            assert_eq!(allocations, 1);
            assert_eq!(native.to_str().unwrap(), name);
            #[cfg(any(feature = "tokio", feature = "smol"))]
            assert_eq!(checked.into_owned().as_str(), name);
            #[cfg(feature = "file-io")]
            assert_eq!(
                crate::file::SourceSpec::new(name).unwrap().name(),
                Some(name)
            );
        }
        // Address-only Source values intentionally do not use this name rule.
        assert_eq!(Source::new("").unwrap().name(), "");
    }

    #[test]
    fn discovery_lookup_checks_inputs_before_timeouts_without_a_runtime() {
        let excessive = crate::MAX_TIMEOUT + Duration::from_nanos(1);
        for timeout in [Duration::ZERO, excessive] {
            for name in ["", "bad\0name", "trailing\0"] {
                assert_same_name_error(
                    checked_name_lookup(name, timeout).err().unwrap(),
                    &SourceName::checked(name).unwrap_err(),
                );
            }
            for host in ["", "bad\0host", "http://camera"] {
                assert_same_name_error(
                    checked_host_lookup(host, timeout).err().unwrap(),
                    &Error::InvalidInput(crate::InputDefect::InvalidHostQuery),
                );
            }
        }
        for result in [
            checked_name_lookup("camera", excessive).err(),
            checked_host_lookup("camera", excessive).err(),
        ] {
            assert!(matches!(
                result,
                Some(Error::InvalidInput(
                    crate::InputDefect::TimeoutTooLong { .. }
                ))
            ));
        }
    }

    #[test]
    fn exact_source_name_entry_points_share_errors_before_native_lookup() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let finder = Finder::new(&ndi, &FinderOptions::new().show_local_sources(false)).unwrap();
        let mut listener = crate::test_support::require_feature(
            ndi.require_feature(crate::RuntimeFeature::ReceiverDiscovery),
        )
        .then(|| {
            crate::discovery_server::RecvListener::new(
                &ndi,
                &crate::discovery_server::ListenerOptions::new().server("127.0.0.1:1"),
            )
            .unwrap()
        });
        let id = crate::discovery_server::ReceiverId::new("not-advertised").unwrap();
        for name in ["", "bad\0name", "trailing\0"] {
            let expected = SourceName::checked(name).unwrap_err();
            assert_same_name_error(
                finder
                    .find_source_by_name(name, Duration::ZERO)
                    .unwrap_err(),
                &expected,
            );
            #[cfg(feature = "file-io")]
            assert_same_name_error(crate::file::SourceSpec::new(name).unwrap_err(), &expected);
            if let Some(listener) = &mut listener {
                // This ID has no membership; invalid text must win before it is read.
                assert_same_name_error(listener.request_connect(&id, name).unwrap_err(), &expected);
            }
        }
    }

    #[test]
    fn source_cursor_commits_only_completed_observations_in_protocol_order() {
        let cursor = SourceCursor::new(1);
        let generation = Cell::new(10);
        let (_, observation) = SourceCursor::observe_snapshot(
            || generation.get(),
            || {
                generation.set(11);
                Ok(())
            },
            |_| true,
        )
        .unwrap();
        assert_eq!(cursor.observed(), 1, "a worker cannot commit its result");
        cursor.commit(observation);
        assert_eq!(cursor.observed(), 10, "snapshot observes before copying");

        let failed = SourceCursor::observe_snapshot(
            || generation.get(),
            || Err::<(), _>(Error::InvalidInput(crate::InputDefect::InvalidHostQuery)),
            |_| true,
        );
        assert!(failed.is_err());
        assert_eq!(
            cursor.observed(),
            10,
            "failure does not acknowledge a report"
        );
        let (_, incomplete) =
            SourceCursor::observe_snapshot(|| generation.get(), || Ok(None::<()>), Option::is_some)
                .unwrap();
        cursor.commit(incomplete);
        assert_eq!(
            cursor.observed(),
            10,
            "an incomplete internal copy is not success"
        );

        let (changed, observation) = SourceCursor::observe_wait(
            || generation.get(),
            || {
                generation.set(12);
                true
            },
        );
        assert!(changed);
        assert_eq!(cursor.observed(), 10);
        cursor.commit(observation);
        assert_eq!(cursor.observed(), 12, "wait observes after its report");
        let (changed, observation) = SourceCursor::observe_wait(
            || panic!("an unchanged wait has no generation to acknowledge"),
            || false,
        );
        assert!(!changed);
        cursor.commit(observation);
        assert_eq!(cursor.observed(), 12);
        let (_, abandoned) = SourceCursor::observe_snapshot(|| 13, || Ok(()), |_| true).unwrap();
        let _ = abandoned; // A dropped async future never calls commit.
        assert_eq!(cursor.observed(), 12);
    }

    #[test]
    fn discovery_window_charges_shared_source_strings_once() {
        let source = Source::new("HOST (Camera)")
            .unwrap()
            .with_address(SourceAddress::new("192.0.2.7:5961").unwrap());
        let expected =
            source.name.storage_bytes() + source.address.as_ref().unwrap().0.storage_bytes();
        let mut discovered = Vec::new();
        let mut seen = HashSet::new();
        let mut retained = 0;
        accumulate_sources(
            &mut discovered,
            &mut seen,
            &mut retained,
            vec![source.clone()],
        )
        .unwrap();
        assert_eq!(retained, expected);
        assert!(Arc::ptr_eq(
            &discovered[0].name.0,
            &seen.get(&source as &dyn SourceIdentity).unwrap().0.name.0
        ));
        accumulate_sources(&mut discovered, &mut seen, &mut retained, vec![source]).unwrap();
        assert_eq!(retained, expected, "duplicates add no storage");
    }

    #[test]
    fn a_typed_stop_does_not_publish_an_incomplete_host_window() {
        use std::sync::atomic::AtomicBool;
        let cache = CachedSources::default();
        let outcome = cache
            .find_sources_by_host_with::<&AtomicBool>(
                HostQuery::checked("camera").unwrap(),
                Deadline::after(Duration::ZERO).unwrap(),
                |_, _| Ok(Err(crate::timeout::Stopped)),
            )
            .unwrap();
        assert!(matches!(outcome, Err(crate::timeout::Stopped)));
        assert!(cache.is_empty());
    }

    #[test]
    fn cache_discovery_retains_the_deadline_that_precedes_the_lookup() {
        let cache = CachedSources::default();
        let deadline = Deadline::starting(
            std::time::Instant::now() - Duration::from_secs(1),
            crate::timeout::Timeout::new(Duration::from_millis(1)).unwrap(),
        )
        .unwrap();
        let sources = cache
            .find_sources_by_host_with::<NeverStop>(
                HostQuery::checked("camera").unwrap(),
                deadline,
                |query, remaining| {
                    assert!(matches!(query, HostQuery::Host("camera")));
                    assert_eq!(remaining.end(), deadline.end());
                    assert!(remaining.is_expired());
                    Ok(Ok(Vec::new()))
                },
            )
            .unwrap()
            .unwrap();
        assert!(sources.is_empty());
    }

    #[cfg(any(feature = "tokio", feature = "smol"))]
    #[test]
    fn owned_host_queries_keep_the_parsed_kind_and_normalized_text() {
        for (host, kind) in [
            ("192.0.2.1", 0),
            ("[2001:db8::1]", 0),
            ("Camera.LOCAL.", 1),
            ("Studio A.", 2),
        ] {
            let text = host.to_owned();
            let parsed = HostQuery::checked(&text).unwrap();
            let expected = HostKey::try_from(parsed).unwrap();
            let owned = parsed.into_owned();
            drop(text);
            let query = owned.as_query();
            assert!(matches!(
                (kind, query),
                (0, HostQuery::Ip(_)) | (1, HostQuery::Host(_)) | (2, HostQuery::Machine(_)),
            ));
            assert_eq!(HostKey::try_from(query).unwrap(), expected);
        }
    }

    #[test]
    fn source_values_refuse_nul_before_any_native_use() {
        assert!(matches!(
            Source::new("Camera\0other"),
            Err(Error::InvalidCString(_))
        ));
        assert!(matches!(
            SourceAddress::new("camera\0other"),
            Err(Error::InvalidCString(_))
        ));
        assert_eq!(Source::new("").unwrap().name(), "");
        assert_eq!(SourceAddress::new("").unwrap().as_str(), "");
    }

    #[test]
    fn native_source_descriptors_share_validated_storage_without_allocating() {
        let source = Source::new("Studio (Camera)")
            .unwrap()
            .with_address(SourceAddress::new("192.0.2.1:5960").unwrap());
        let (raw, allocations) = crate::test_alloc::allocation_count(|| {
            let first = source.to_raw();
            let second = source.to_raw();
            assert_eq!(first.as_raw().p_ndi_name, second.as_raw().p_ndi_name);
            // SAFETY: both descriptors initialized their address pointer.
            unsafe {
                assert_eq!(
                    first.as_raw().__bindgen_anon_1.p_url_address,
                    second.as_raw().__bindgen_anon_1.p_url_address
                );
            }
            first
        });
        assert_eq!(allocations, 0);
        drop(source);
        // SAFETY: RawSource retains the immutable strings after Source is dropped.
        unsafe {
            assert_eq!(
                std::ffi::CStr::from_ptr(raw.as_raw().p_ndi_name),
                c"Studio (Camera)"
            );
            assert_eq!(
                std::ffi::CStr::from_ptr(raw.as_raw().__bindgen_anon_1.p_url_address),
                c"192.0.2.1:5960"
            );
        }
    }

    #[test]
    fn configured_lists_preserve_elements_and_validate_at_owner_creation() {
        let options = FinderOptions::new()
            .groups(["Public", "Studio"])
            .extra_ips(["192.0.2.1", "192.0.2.2"]);
        assert_eq!(options.configured_groups().unwrap(), ["Public", "Studio"]);
        assert_eq!(
            list_cstring(options.configured_groups(), "groups")
                .unwrap()
                .as_deref(),
            Some(c"Public,Studio")
        );
        assert_eq!(list_cstring(None, "groups").unwrap(), None);
        assert!(matches!(
            list_cstring(Some(&[]), "groups"),
            Err(Error::InvalidInput(crate::InputDefect::Empty {
                what: "groups"
            }))
        ));
        for invalid in ["", "Studio,Public", "Studio\0Public"] {
            let options = FinderOptions::new().groups(["valid", invalid]);
            assert_eq!(options.configured_groups().unwrap()[1], invalid);
            assert!(matches!(
                list_cstring(options.configured_groups(), "groups"),
                Err(Error::InvalidInput(crate::InputDefect::ListElement {
                    setting: "groups",
                    index: 1
                }))
            ));
        }
    }

    #[test]
    fn configured_lists_report_the_first_invalid_element_across_checks() {
        for (values, expected) in [
            (["first\0nul", "", "last,comma"], 0),
            (["first,comma", "second\0nul", ""], 0),
            (["", "second\0nul", "last,comma"], 0),
            (["valid", "second\0nul", "last,comma"], 1),
            (["valid", "second,comma", "last\0nul"], 1),
            (["valid", "", "last\0nul"], 1),
            (["non-ASCII café", "second\0nul", "last,comma"], 1),
            (["valid", "second", "last\0nul"], 2),
        ] {
            let values = values.map(str::to_owned);
            assert!(matches!(
                list_cstring(Some(&values), "servers"),
                Err(Error::InvalidInput(crate::InputDefect::ListElement {
                    setting: "servers",
                    index,
                })) if index == expected
            ));
        }
    }

    #[test]
    fn cache_descriptor_copies_release_the_global_mutex_and_recheck_publication() {
        let cache = CachedSources::default();
        let sources = vec![Source::new("camera (Program)").unwrap()];
        for hit in [false, true] {
            let result = cache
                .lookup_with_copy(
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
            .lookup_with_copy(
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
            .lookup_with_copy(
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
    fn publication_failure_returns_discovery_but_hit_failure_is_reported() {
        let cache = CachedSources::default();
        let sources = vec![Source::new("camera (Program)").unwrap()];
        for invalidate in [false, true] {
            let result = cache
                .lookup_with_copy(
                    "camera",
                    Duration::ZERO,
                    |_| Ok(sources.clone()),
                    |_| {
                        if invalidate {
                            cache.invalidate("camera");
                        }
                        Err(budget::allocation_failed())
                    },
                )
                .unwrap();
            assert_eq!(result, sources);
            assert!(cache.is_empty());
        }
        cache
            .lookup("camera", Duration::ZERO, |_| Ok(sources.clone()))
            .unwrap();
        let result = cache.lookup_with_copy(
            "camera",
            Duration::ZERO,
            |_| panic!("a hit must not run discovery"),
            |_| Err(budget::allocation_failed()),
        );
        assert!(matches!(
            result,
            Err(Error::AllocationFailed(crate::AllocationFailure {
                kind: crate::AllocationKind::DiscoveryResults,
                ..
            }))
        ));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn successful_cache_publication_keeps_its_allocation_count() {
        for (host, expected) in [("camera", 4), ("192.0.2.1", 3)] {
            let cache = CachedSources::default();
            // Initialize native mutex storage before measuring publication.
            assert!(cache.is_empty());
            let sources = vec![Source::new("camera (Program)").unwrap()];
            let (result, allocations) = crate::test_alloc::allocation_count(|| {
                cache.lookup(host, Duration::ZERO, |_| Ok(sources))
            });
            assert_eq!(result.unwrap().len(), 1);
            // One vector, its Arc header and map storage; only names need an
            // allocated key. Source strings remain shared with the caller.
            assert_eq!(allocations, expected, "{host}");
            assert_eq!(cache.len(), 1);
        }
    }

    #[cfg(any(feature = "tokio", feature = "smol"))]
    #[test]
    fn cancellation_during_successful_snapshot_lock_acquisition_skips_the_snapshot() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let finder = Finder::new(&ndi, &FinderOptions::new()).unwrap();
        fn check<W: SnapshotWait>(finder: &Finder, wait: W) {
            let stop = AtomicBool::new(false);
            let mut held = Some(finder.sources_lock.lock());
            let mut acquired = false;
            let result = finder.lock_snapshots_with(wait, &stop, |remaining| {
                assert!(!stop.load(Ordering::Acquire));
                assert!(remaining > Duration::ZERO && remaining <= POLL_INTERVAL);
                // Model cancellation arriving while the acquisition waits,
                // followed by the current holder releasing within this slice.
                stop.store(true, Ordering::Release);
                drop(held.take());
                let instance = finder.sources_lock.try_lock_for(remaining);
                acquired = instance.is_some();
                instance
            });
            assert!(
                acquired,
                "the cancelled wait must exercise a successful acquisition"
            );
            assert!(
                matches!(result, Err(crate::timeout::Stopped)),
                "cancellation must withhold the native snapshot guard"
            );
            assert!(
                finder.sources_lock.try_lock().is_some(),
                "the cancelled acquisition must release its lock"
            );
        }
        check(&finder, UnboundedSnapshot);
        check(&finder, Deadline::after(Duration::from_secs(3)).unwrap());
    }

    #[cfg(any(feature = "tokio", feature = "smol"))]
    #[test]
    fn cancelled_snapshot_lock_wait_finishes_without_entering_the_sdk() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        const WATCHDOG: Duration = Duration::from_secs(5);
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let finder = Finder::new(&ndi, &FinderOptions::new()).unwrap();
        let held = finder.sources_lock.lock();
        let stop = AtomicBool::new(false);
        let (ready, waiting) = mpsc::channel();
        let (finished, completion) = mpsc::channel();
        std::thread::scope(|scope| {
            // Keep cleanup inside the scope, so even a failed assertion drops
            // the held lock before scope waits for a blocked worker to finish.
            let held = held;
            let finder = &finder;
            let stop = &stop;
            let worker = scope.spawn(move || {
                let mut ready = Some(ready);
                let stopped = finder
                    .lock_snapshots_with(UnboundedSnapshot, stop, |remaining| {
                        assert!(remaining > Duration::ZERO && remaining <= POLL_INTERVAL);
                        if let Some(ready) = ready.take() {
                            ready.send(()).unwrap();
                        }
                        finder.sources_lock.try_lock_for(remaining)
                    })
                    .is_err();
                let _ = finished.send(stopped);
            });
            waiting.recv_timeout(WATCHDOG).unwrap();
            stop.store(true, Ordering::Release);
            // Completion must precede unlocking; the watchdog permits shared
            // CI scheduling delays, not an unbounded snapshot-lock wait.
            let result = completion.recv_timeout(WATCHDOG);
            drop(held);
            worker.join().unwrap();
            assert!(
                result.expect("cancelled snapshot wait did not finish while the lock was held"),
                "cancellation must withhold the native snapshot permission"
            );
        });
    }

    #[test]
    fn the_first_watch_after_finder_creation_allocates_nothing() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let finder = Finder::new(&ndi, &FinderOptions::new()).unwrap();
        // This exact Finder has had no watch, snapshot or generation query.
        // Native mutex initialization belongs to construction, not this call.
        let (watch, allocations) = crate::test_alloc::allocation_count(|| finder.watch());
        assert_eq!(allocations, 0);
        assert!(std::ptr::eq(watch.finder, &finder));
        assert_eq!(watch.cursor.observed(), 0);
    }

    #[test]
    fn source_cache_moves_its_finder_without_allocating_or_recreating_it() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let finder = Finder::new(&ndi, &FinderOptions::new().groups(["Studio"])).unwrap();
        let instance = finder.instance;
        let (cache, allocations) = crate::test_alloc::allocation_count(|| SourceCache::new(finder));
        assert_eq!(allocations, 0);
        assert_eq!(cache.finder().instance, instance);
        assert_eq!(cache.finder()._groups.as_deref(), Some(c"Studio"));
        assert!(cache.is_empty());
    }

    fn host_key(host: &str) -> HostKey {
        HostKey::try_from(HostQuery::parse(host).unwrap()).unwrap()
    }

    #[test]
    fn canonical_host_keys_obey_identity_and_hash_laws() {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let hash = |key: &HostKey| {
            let mut state = DefaultHasher::new();
            key.hash(&mut state);
            let owned_hash = state.finish();
            let mut borrowed_state = DefaultHasher::new();
            std::borrow::Borrow::<dyn HostIdentity>::borrow(key).hash(&mut borrowed_state);
            assert_eq!(owned_hash, borrowed_state.finish());
            owned_hash
        };
        for variants in [
            vec!["camera.local", "CAMERA.LOCAL", "Camera.Local."],
            vec!["Studio A", "STUDIO A", "studio a."],
            vec!["STUDIO_PC", "studio_pc", "Studio_Pc."],
            vec!["ÉTAGE A", "Étage a", "Étage A."],
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
        for name in [
            "camera",
            "camera-1.local",
            "a.b.c",
            "123",
            "Studio A",
            "studio_pc",
            "Étage A",
            &"a".repeat(63),
        ] {
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
            "studio a",
            "studio-a",
            "studio_a",
            "Étage a",
            "étage a",
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
    fn typed_host_keys_match_borrowed_identity_and_hashes() {
        use std::hash::DefaultHasher;
        fn hash(key: &dyn HostIdentity) -> u64 {
            let mut state = DefaultHasher::new();
            key.hash(&mut state);
            state.finish()
        }
        for (a, b, equal) in [
            ("192.0.2.1", "[::ffff:192.0.2.1]", false),
            ("[2001:db8::1]", "2001:DB8::1", true),
            ("Camera.Local.", "camera.local", true),
            ("MY MACHINE", "my machine", true),
            ("Ä", "ä", false),
            ("camera..", "camera", false),
        ] {
            let a = HostQuery::checked(a).unwrap();
            let b = HostQuery::checked(b).unwrap();
            let key_a = HostKey::try_from(a).unwrap();
            let key_b = HostKey::try_from(b).unwrap();
            assert_eq!(key_a == key_b, equal);
            assert_eq!(hash(&a) == hash(&b), equal);
            assert_eq!(hash(&key_a), hash(&a));
            assert_eq!(hash(&key_b), hash(&b));
            let map = HashMap::from([(key_a, ())]);
            assert_eq!(map.contains_key(&b as &dyn HostIdentity), equal);
        }
        // The type keeps an IP and an identically spelled future name disjoint.
        let ip = HostKey::try_from(HostQuery::Ip("192.0.2.1".parse().unwrap())).unwrap();
        let name = HostKey::try_from(HostQuery::Machine("192.0.2.1")).unwrap();
        assert_ne!(ip, name);
        let map = HashMap::from([(ip, ())]);
        assert!(!map.contains_key(&HostQuery::Machine("192.0.2.1") as &dyn HostIdentity));
    }

    #[test]
    fn borrowed_host_identity_preserves_map_laws_without_allocating_for_edge_spellings() {
        use std::hash::DefaultHasher;
        let spellings = [
            "camera",
            "Camera.",
            "camera..",
            "CAMERA..",
            "camera...",
            "Étage A",
            "ÉTAGE a.",
            "étage a",
            "E\u{301}tage A",
            "İS",
            "İs.",
            "is",
            "K",
            "K",
            "k.",
            "192.0.2.1",
            "[192.0.2.1]",
            "::ffff:192.0.2.1",
            "[::ffff:192.0.2.1]",
            "[2001:db8::1]",
            "2001:DB8::1",
        ];
        let queries: Vec<_> = spellings
            .iter()
            .map(|text| HostQuery::checked(text).unwrap())
            .collect();
        let keys: Vec<_> = queries
            .iter()
            .map(|&query| HostKey::try_from(query).unwrap())
            .collect();
        let hash = |value: &dyn HostIdentity| {
            let mut state = DefaultHasher::new();
            value.hash(&mut state);
            state.finish()
        };
        for (i, &query) in queries.iter().enumerate() {
            let mut map = HashMap::from([(HostKey::try_from(query).unwrap(), ())]);
            let ((), allocations) = crate::test_alloc::allocation_count(|| {
                for (j, other) in queries.iter().enumerate() {
                    let equal = keys[i] == keys[j];
                    assert_eq!(
                        (&query as &dyn HostIdentity) == (other as &dyn HostIdentity),
                        equal
                    );
                    assert_eq!(hash(&keys[j]), hash(other));
                    if equal {
                        assert_eq!(hash(&query), hash(other));
                    }
                    assert_eq!(
                        map.contains_key(other as &dyn HostIdentity),
                        equal,
                        "{} versus {}",
                        spellings[i],
                        spellings[j]
                    );
                    let removed = map.remove_entry(other as &dyn HostIdentity);
                    assert_eq!(removed.is_some(), equal);
                    if let Some((key, value)) = removed {
                        map.insert(key, value);
                    }
                }
            });
            assert_eq!(allocations, 0, "{}", spellings[i]);
        }
    }

    #[test]
    fn equivalent_cache_hits_and_invalidation_share_one_identity() {
        for (a, b) in [
            ("CAMERA.LOCAL", "camera.local"),
            ("camera.local.", "camera.local"),
            ("STUDIO A", "studio a."),
            ("STUDIO_PC.", "studio_pc"),
            ("[2001:0db8:0:0:0:0:0:1]", "2001:db8::1"),
        ] {
            for (first, second) in [(a, b), (b, a)] {
                let cache = CachedSources::default();
                let old = Source::new("camera")
                    .unwrap()
                    .with_address(SourceAddress::new("192.0.2.1:5960").unwrap());
                let fresh = old
                    .clone()
                    .with_address(SourceAddress::new("192.0.2.1:5961").unwrap());
                cache
                    .lookup(first, Duration::ZERO, |_| Ok(vec![old.clone()]))
                    .unwrap();
                let (identical, identical_allocs) = crate::test_alloc::allocation_count(|| {
                    cache
                        .lookup(first, Duration::ZERO, |_| panic!("cache hit"))
                        .unwrap()
                });
                let (equivalent, equivalent_allocs) = crate::test_alloc::allocation_count(|| {
                    cache
                        .lookup(second, Duration::ZERO, |_| panic!("equivalent cache hit"))
                        .unwrap()
                });
                assert_eq!(identical, vec![old.clone()]);
                assert_eq!(equivalent, vec![old]);
                assert_eq!(identical_allocs, 1); // Only the vector; validated strings are shared.
                assert_eq!(equivalent_allocs, identical_allocs);
                assert_eq!(cache.len(), 1);
                assert!(cache
                    .lookup(second, Duration::MAX, |_| panic!("invalid timeout"))
                    .is_err());
                cache.invalidate(second);
                assert!(cache.is_empty());
                assert_eq!(
                    cache
                        .lookup(first, Duration::ZERO, |_| Ok(vec![fresh.clone()]))
                        .unwrap(),
                    vec![fresh]
                );
                assert_eq!(cache.len(), 1);
                cache.invalidate("invalid:host:port");
                assert_eq!(cache.len(), 1);
                cache.clear();
                assert!(cache
                    .lookup(first, Duration::ZERO, |_| Err(Error::InvalidInput(
                        crate::InputDefect::InvalidHostQuery
                    )))
                    .is_err());
                assert!(cache
                    .lookup(second, Duration::ZERO, |_| Ok(Vec::new()))
                    .unwrap()
                    .is_empty());
                assert!(cache.is_empty());
            }
        }
    }

    #[test]
    fn exact_host_matching_ignores_labels_ports_and_url_payloads() {
        let other = Source::new("CAMERA10 (192.168.1.1)")
            .unwrap()
            .with_address(SourceAddress::new("192.168.1.100:5960").unwrap());
        assert!(!other.matches_host("192.168.1.1"));
        assert!(!other.matches_host("5960"));
        assert!(!other.matches_host("CAMERA1"));
        let exact = Source::new("CAMERA1 (Program)").unwrap().with_address(
            SourceAddress::new("http://user:pass@Camera.Local:8080/ndi?next=192.0.2.1").unwrap(),
        );
        assert!(exact.matches_host("camera.local."));
        assert!(exact.matches_host("camera1"));
        assert!(!exact.matches_host("192.0.2.1"));
        assert!(!exact.matches_host("Program"));
        let ipv6 = SourceAddress::new("[2001:0db8:0:0:0:0:0:1]:5960").unwrap();
        assert!(ipv6.matches_host("2001:db8::1"));
        assert!(ipv6.matches_host("[2001:db8::1]"));
        let studio = Source::new("Studio A (Camera)").unwrap();
        assert!(studio.matches_host("studio a"));
        assert!(studio.matches_host("STUDIO A."));
        assert!(!studio.matches_host("Studio"));
        assert!(!studio.matches_host("Camera"));
        assert!(Source::new("STUDIO_PC (Camera)")
            .unwrap()
            .matches_host("studio_pc"));
        let unicode = Source::new("ÉTAGE A (Camera)").unwrap();
        assert!(unicode.matches_host("Étage a"));
        assert!(!unicode.matches_host("étage a"));
        assert!(!SourceAddress::new("http://studio-a:80")
            .unwrap()
            .matches_host("Studio A"));
        // Even opaque address text that contains the same machine name cannot
        // make a machine query match an address rather than the SDK label.
        assert!(!SourceAddress::new("Studio A:5960")
            .unwrap()
            .matches_host("Studio A"));
        let too_long = "x".repeat(254);
        for invalid in [
            "",
            "camera:5960",
            "http://camera.local",
            "camera/local",
            "192.168.1",
            "foo\0bar",
            "[bad]",
            "Studio (A)",
            " Studio",
            "Studio\t",
            "Studio .",
            "\u{2003}Studio",
            "Studio\u{2003}",
            &too_long,
        ] {
            assert!(HostQuery::parse(invalid).is_none(), "{invalid:?}");
        }
        let (_, allocations) = crate::test_alloc::allocation_count(|| {
            for _ in 0..100 {
                assert!(exact.matches_host("CAMERA.LOCAL"));
                assert!(ipv6.matches_host("2001:db8::1"));
                assert!(studio.matches_host("STUDIO A."));
                assert!(unicode.matches_host("Étage a"));
                let query = HostQuery::parse("Studio A").unwrap();
                assert!(
                    (&query as &dyn HostIdentity)
                        == (&HostQuery::Machine("studio a") as &dyn HostIdentity)
                );
            }
        });
        assert_eq!(allocations, 0);
    }

    #[test]
    fn machine_query_grammar_covers_each_printable_ascii_byte() {
        for byte in b' '..=b'~' {
            let delimiter = b":/?#@[]()".contains(&byte);
            let embedded = format!("Studio{}A", char::from(byte));
            assert_eq!(
                HostQuery::parse(&embedded).is_some(),
                !delimiter,
                "embedded byte {byte:#x}: {embedded:?}"
            );
            let single = char::from(byte).to_string();
            assert_eq!(
                HostQuery::parse(&single).is_some(),
                !delimiter && !matches!(byte, b' ' | b'.'),
                "standalone byte {byte:#x}: {single:?}"
            );
        }
        for character in (0..=31).chain(127..=159).map(char::from) {
            let name = format!("Studio{character}A");
            assert!(HostQuery::parse(&name).is_none(), "{name:?}");
        }
        for machine in ["Studio A", "STUDIO_PC", "Étage A", "a".repeat(253).as_str()] {
            assert!(
                matches!(HostQuery::parse(machine), Some(HostQuery::Machine(_))),
                "{machine:?}"
            );
        }
        for host in ["studio-a", "studio.local", "STUDIO"] {
            assert!(
                matches!(HostQuery::parse(host), Some(HostQuery::Host(_))),
                "{host:?}"
            );
        }
    }

    #[test]
    fn cache_selects_exact_host_and_rejects_invalid_queries_before_discovery() {
        let cache = CachedSources::default();
        let wrong = Source::new("CAMERA10 (Program)")
            .unwrap()
            .with_address(SourceAddress::new("192.168.1.100:5960").unwrap());
        let right = Source::new("CAMERA1 (Program)")
            .unwrap()
            .with_address(SourceAddress::new("192.168.1.1:5961").unwrap());
        let timeout = Duration::ZERO;
        let selected = cache
            .lookup("192.168.1.1", timeout, |query| {
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
                .lookup("192.168.1.1", timeout, |_| panic!("cache hit"))
                .unwrap(),
            vec![right]
        );
        let too_long = "x".repeat(254);
        for invalid in [
            "",
            "192.168.1.1:5961",
            "camera?query",
            "foo\0bar",
            "Studio (A)",
            " Studio",
            "Studio\t",
            &too_long,
        ] {
            assert!(matches!(
                cache.lookup(invalid, timeout, |_| panic!(
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
            let old = Source::new("old")
                .unwrap()
                .with_address(SourceAddress::new("192.0.2.1:5960").unwrap());
            let fresh = Source::new("fresh")
                .unwrap()
                .with_address(SourceAddress::new("192.0.2.1:5961").unwrap());
            let pending = Arc::clone(&cache);
            let old_result = old.clone();
            let thread = std::thread::spawn(move || {
                pending
                    .lookup("CAMERA.LOCAL.", Duration::ZERO, |_| {
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
                .lookup("camera.local", Duration::ZERO, |_| Ok(vec![fresh.clone()]))
                .unwrap();
            assert_eq!(found, vec![fresh.clone()]);
            resume_tx.send(()).unwrap();
            assert_eq!(thread.join().unwrap(), vec![old]);
            assert_eq!(
                cache
                    .lookup("Camera.Local.", Duration::ZERO, |_| panic!(
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
                    .lookup("camera.local", Duration::ZERO, |_| {
                        started_tx.send(()).unwrap();
                        resume_rx.recv().unwrap();
                        Ok(vec![Source::new("CAMERA (Program)").unwrap()])
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
        let source = Source::new("camera")
            .unwrap()
            .with_address(SourceAddress::new("192.0.2.1").unwrap());
        assert_eq!(
            cache
                .lookup("192.0.2.1", Duration::ZERO, |_| Ok(vec![source.clone()]))
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
            let source = Source::new("")
                .unwrap()
                .with_address(SourceAddress::new(address).unwrap());
            assert_eq!(source.host(), Some(host), "{address}");
            assert_eq!(source.address().unwrap().port(), port, "{address}");
        }
        assert!(parse_authority("[::1").is_none());
        assert!(parse_authority("camera:invalid").is_none());
        assert!(parse_authority("").is_none());
    }

    #[test]
    fn configured_and_discovered_sources_share_text_identity() {
        use std::hash::{DefaultHasher, Hash, Hasher};

        let hash = |source: &Source| {
            let mut state = DefaultHasher::new();
            source.hash(&mut state);
            state.finish()
        };
        for text in [
            "camera.local:8080",
            "192.0.2.1:5960",
            "[2001:db8::1]:5960",
            "ndi://h:1",
            "opaque text",
        ] {
            let configured = Source::new("Camera")
                .unwrap()
                .with_address(SourceAddress::new(text).unwrap());
            let raw = configured.to_raw();
            // SAFETY: RawSource lends its descriptor while retaining both C
            // strings for this synchronous owned copy.
            let discovered = unsafe { source_from_raw(raw.as_raw(), c"test") }.unwrap();
            assert_eq!(configured, discovered);
            assert_eq!(hash(&configured), hash(&discovered));
            let address = discovered.address().unwrap();
            assert_eq!(address.as_str(), text);
            assert_eq!(address.is_url(), text.contains("://"));
            // Both native union member names denote the same address pointer.
            // SAFETY: RawSource initialized the pointer member of this union.
            unsafe {
                assert_eq!(
                    raw.as_raw().__bindgen_anon_1.p_url_address,
                    raw.as_raw().__bindgen_anon_1.p_ip_address
                );
                assert_eq!(
                    std::ffi::CStr::from_ptr(raw.as_raw().__bindgen_anon_1.p_url_address)
                        .to_str()
                        .unwrap(),
                    text
                );
            }
        }
    }

    #[test]
    fn discovery_accumulates_distinct_name_and_address_text_in_order() {
        let first = Source::new("Camera")
            .unwrap()
            .with_address(SourceAddress::new("192.0.2.1").unwrap());
        let changed = first
            .clone()
            .with_address(SourceAddress::new("192.0.2.2").unwrap());
        let same_address = first
            .clone()
            .with_address(SourceAddress::new("192.0.2.1").unwrap());
        let mut seen = HashSet::new();
        let mut discovered = Vec::new();
        let mut bytes = 0;
        accumulate_sources(&mut discovered, &mut seen, &mut bytes, vec![first.clone()]).unwrap();
        accumulate_sources(&mut discovered, &mut seen, &mut bytes, Vec::new()).unwrap();
        accumulate_sources(
            &mut discovered,
            &mut seen,
            &mut bytes,
            vec![first.clone(), changed.clone(), same_address.clone()],
        )
        .unwrap();
        assert_eq!(same_address, first);
        assert_eq!(discovered, vec![first, changed]);
    }

    #[test]
    fn cache_validates_timeout_before_returning_cached_source() {
        let cache = CachedSources::default();
        cache.cache.lock().unwrap().entries.insert(
            HostKey::try_from(HostQuery::parse("camera").unwrap()).unwrap(),
            Arc::new(vec![Source::new("camera").unwrap()]),
        );
        for host in ["camera", "not_cached"] {
            assert!(matches!(
                cache.lookup(host, Duration::MAX, |_| panic!("invalid timeout")),
                Err(Error::InvalidInput(_))
            ));
        }
        // A hit is answered without reaching discovery, the only step that
        // uses the runtime.
        assert_eq!(
            cache
                .lookup("camera", Duration::ZERO, |_| panic!("cache hit"))
                .unwrap(),
            vec![Source::new("camera").unwrap()]
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
                let owned = unsafe {
                    {
                        let (name, address) = source_pointers(source);
                        decode_source(name, address, budget, |_| Ok(true))
                    }
                };
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
                    assert!(matches!(sources[0].address(), Some(address) if address.as_str() == format!("192.0.2.{index}:5960")));
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
        let result = unsafe { source_from_raw(ptr::null(), c"NDIlib_send_get_source_name") };
        assert!(matches!(
            result,
            Err(Error::NativeOperationFailed {
                operation}) if operation.name() == c"NDIlib_send_get_source_name"
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
        let unterminated = vec![b'x'; crate::sdk_copy::MAX_SDK_STRING_BYTES];
        let malformed = unterminated.as_ptr().cast();
        let cases = [
            (
                "name and IP",
                name,
                ip,
                Expected::Source(
                    Source::new("Camera")
                        .unwrap()
                        .with_address(SourceAddress::new("192.0.2.1:5960").unwrap()),
                ),
            ),
            (
                "name and URL",
                name,
                url,
                Expected::Source(
                    Source::new("Camera")
                        .unwrap()
                        .with_address(SourceAddress::new("ndi://camera.local:5960").unwrap()),
                ),
            ),
            (
                "address only",
                ptr::null(),
                ip,
                Expected::Source(
                    Source::new("")
                        .unwrap()
                        .with_address(SourceAddress::new("192.0.2.1:5960").unwrap()),
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
                Expected::Source(Source::new("Cam\u{fffd}").unwrap()),
            ),
            ("unterminated name", malformed, ip, Expected::Malformed),
            (
                "name only",
                name,
                ptr::null(),
                Expected::Source(Source::new("Camera").unwrap()),
            ),
            (
                "empty name",
                empty,
                ptr::null(),
                Expected::Source(Source::new("").unwrap()),
            ),
            (
                "empty address only",
                ptr::null(),
                empty,
                Expected::Source(
                    Source::new("")
                        .unwrap()
                        .with_address(SourceAddress::new("").unwrap()),
                ),
            ),
            (
                "empty address",
                name,
                empty,
                Expected::Source(
                    Source::new("Camera")
                        .unwrap()
                        .with_address(SourceAddress::new("").unwrap()),
                ),
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
            let mut accepted_budget = SnapshotBudget::new();
            let mut rejected_budget = SnapshotBudget::new();
            let ((snapshot, accepted_allocations), (rejected, rejected_allocations), single) = unsafe {
                (
                    crate::test_alloc::allocation_count(|| {
                        decode_source(name, address, &mut accepted_budget, |_| Ok(true))
                    }),
                    crate::test_alloc::allocation_count(|| {
                        decode_source(name, address, &mut rejected_budget, |_| Ok(false))
                    }),
                    source_from_raw(&raw, c"test source query"),
                )
            };
            assert!(rejected.unwrap().is_none(), "{label}");
            assert_eq!(rejected_allocations, 0, "{label}");
            assert_eq!(accepted_budget.spent(), rejected_budget.spent(), "{label}");
            let strings = usize::from(!address.is_null()) + 1;
            assert_eq!(
                accepted_allocations,
                if snapshot.as_ref().is_ok_and(Option::is_some) {
                    2 * strings
                } else {
                    0
                },
                "{label}"
            );
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
                                operation}) if operation.name() == c"test source query"
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
    fn descriptor_selector_errors_preserve_text_charges_without_building_storage() {
        for (name, address) in [
            (c"Camera".as_ptr(), c"192.0.2.1:5960".as_ptr()),
            (c"Cam\xff".as_ptr(), c"ndi://host\xfe:5960".as_ptr()),
            (ptr::null(), c"[2001:db8::1]:5960".as_ptr()),
            (c"".as_ptr(), ptr::null()),
        ] {
            let mut rejected_budget = SnapshotBudget::new();
            let mut failed_budget = SnapshotBudget::new();
            // SAFETY: the null pointers and complete C literals stay live for
            // both decodes; neither selector retains its borrowed source.
            let rejected =
                unsafe { decode_source(name, address, &mut rejected_budget, |_| Ok(false)) };
            assert!(rejected.unwrap().is_none());
            let (failed, allocations) = crate::test_alloc::allocation_count(|| {
                // SAFETY: the same descriptor and lifetime obligations as above.
                unsafe {
                    decode_source(name, address, &mut failed_budget, |_| {
                        Err(Error::InvalidInput(crate::InputDefect::InvalidHostQuery))
                    })
                }
            });
            assert!(matches!(
                failed,
                Err(Error::InvalidInput(crate::InputDefect::InvalidHostQuery))
            ));
            assert_eq!(allocations, 0);
            assert_eq!(failed_budget.spent(), rejected_budget.spent());
        }
    }

    #[test]
    fn source_descriptor_and_snapshot_allocations_name_their_storage() {
        let name = CString::new(vec![b'x'; 1 << 20]).unwrap();
        let raw = NDIlib_source_t {
            p_ndi_name: name.as_ptr(),
            ..Default::default()
        };
        let single = crate::test_alloc::fail_allocations_at_least(1 << 20, || {
            // SAFETY: the descriptor and its complete terminated name stay live.
            unsafe { source_from_raw(&raw, c"test source query") }
        });
        assert!(matches!(
            single,
            Err(Error::AllocationFailed(crate::AllocationFailure {
                kind: crate::AllocationKind::SourceDescriptor,
                bytes: Some(1_048_577),
            }))
        ));
        let snapshot = crate::test_alloc::fail_allocations_at_least(1 << 20, || {
            // SAFETY: the descriptor and its complete terminated name stay live.
            unsafe {
                decode_source(
                    name.as_ptr(),
                    ptr::null(),
                    &mut SnapshotBudget::new(),
                    |_| Ok(true),
                )
            }
        });
        assert!(matches!(
            snapshot,
            Err(Error::AllocationFailed(crate::AllocationFailure {
                kind: crate::AllocationKind::DiscoveryResults,
                bytes: Some(1_048_577),
            }))
        ));
    }

    #[test]
    fn source_strings_are_bounded_by_the_sdk_string_limit() {
        use crate::sdk_copy::MAX_SDK_STRING_BYTES;
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
            let fits = source_from_raw(&descriptor(&longest, ptr::null()), c"test").unwrap();
            assert_eq!(fits.name().len(), MAX_SDK_STRING_BYTES - 1);
            for raw in [
                descriptor(&too_long, address.as_ptr()),
                descriptor(c"Camera".to_bytes_with_nul(), too_long.as_ptr().cast()),
            ] {
                let Err(Error::UnexpectedSdkResult(reason)) =
                    source_from_raw(&raw, c"NDIlib_send_get_source_name")
                else {
                    panic!("a string longer than the limit must be refused");
                };
                assert_eq!(
                    reason,
                    crate::SdkResultDefect::UnterminatedText {
                        operation: sys::table::for_test((), c"NDIlib_send_get_source_name")
                            .function()
                    }
                );
            }
        }
    }

    #[test]
    fn a_snapshot_leaves_out_only_the_malformed_entries() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let finder = Finder::new(&ndi, &FinderOptions::default()).unwrap();
        let mut too_long = vec![b'x'; crate::sdk_copy::MAX_SDK_STRING_BYTES + 1];
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
                unsafe {
                    {
                        let (name, address) = source_pointers(source);
                        decode_source(name, address, budget, |_| Ok(true))
                    }
                }
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

        let result = unsafe { source_from_raw(&source, c"NDIlib_find_get_current_sources") };
        assert!(matches!(
            result,
            Err(Error::NativeOperationFailed {
                operation}) if operation.name() == c"NDIlib_find_get_current_sources"
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
        let source = unsafe { source_from_raw(&raw, c"test") }.unwrap();
        assert!(source.name().is_empty());
        assert!(
            matches!(source.address(), Some(address) if address.is_url() && address.as_str() == "ndi://127.0.0.1:5960")
        );
        let roundtrip = source.to_raw();
        assert!(!roundtrip.as_raw().p_ndi_name.is_null());
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

        let result = unsafe { source_from_raw(&source, c"test") };
        assert!(result.is_ok());

        let source = result.unwrap();
        assert_eq!(source.name(), "Test Source");
        let address = source.address().expect("an address");
        assert_eq!(address.as_str(), "192.168.1.100:5960");
        assert!(!address.is_url());
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

        let result = unsafe { source_from_raw(&source, c"test") };
        assert!(result.is_ok());

        let source = result.unwrap();
        assert_eq!(source.name(), "HX Source");
        let address = source.address().expect("an address");
        assert_eq!(address.as_str(), "http://camera.local:8080/ndi");
        assert!(address.is_url());
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

        let result = unsafe { source_from_raw(&source, c"test") };
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
