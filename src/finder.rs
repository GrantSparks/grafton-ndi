//! NDI source discovery and network browsing.

use std::{
    collections::{HashMap, HashSet},
    ffi::CString,
    fmt::{self, Display, Formatter},
    net::IpAddr,
    ptr,
    sync::Mutex,
    time::Duration,
};

use crate::{
    frames::copy_sdk_string,
    sys::{self, bindings::*},
    timeout::Deadline,
    to_ms_checked, Error, Ndi, Result,
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

/// Discovers NDI sources on the network.
///
/// `Finder` provides methods to discover and monitor NDI sources. It maintains
/// a background thread that continuously updates the list of available sources.
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
/// if finder.wait_for_sources(Duration::from_secs(5))? {
///     let sources = finder.current_sources()?;
///     for source in sources {
///         println!("Found: {}", source);
///     }
/// }
/// # Ok(())
/// # }
/// ```
pub struct Finder {
    instance: NDIlib_find_instance_t,
    // SDK snapshots and their strings expire on the next snapshot call.
    sources_lock: Mutex<()>,
    _groups: Option<CString>,    // Hold ownership of CStrings
    _extra_ips: Option<CString>, // to ensure they outlive SDK usage
    #[cfg(feature = "advanced")]
    _config: Option<crate::advanced::AdvancedConfig>,
    _ndi: Ndi,
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
            instance,
            sources_lock: Mutex::new(()),
            _groups: groups,
            _extra_ips: extra_ips,
            #[cfg(feature = "advanced")]
            _config: config,
            _ndi: ndi.clone(),
        })
    }

    /// Run `read` on the native finder while no other snapshot can be taken.
    ///
    /// The SDK owns a snapshot and its strings until the next snapshot call on
    /// the same finder, so `read` must finish copying before it returns.
    #[cfg(feature = "advanced")]
    pub(crate) fn snapshot<T>(&self, read: impl FnOnce(NDIlib_find_instance_t) -> T) -> T {
        let _snapshot = self
            .sources_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        read(self.instance)
    }

    /// The runtime this finder was created through.
    #[cfg(feature = "advanced")]
    pub(crate) fn runtime(&self) -> &Ndi {
        &self._ndi
    }

    /// Waits for the source list to change.
    ///
    /// This method blocks until the list of discovered sources changes or the
    /// timeout expires. Use this to efficiently monitor for source changes.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum time to wait ([`Duration::ZERO`] = no wait).
    ///   Must not exceed [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Returns
    ///
    /// `true` if the source list changed, `false` if the timeout expired.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] if `timeout` exceeds [`crate::MAX_TIMEOUT`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{Ndi, FinderOptions, Finder};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = Ndi::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// // Wait up to 5 seconds for changes
    /// if finder.wait_for_sources(Duration::from_secs(5))? {
    ///     println!("Source list changed!");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn wait_for_sources(&self, timeout: Duration) -> Result<bool> {
        Ok(self.wait_for_sources_ms(to_ms_checked(timeout)?))
    }

    /// [`Self::wait_for_sources`] with a timeout already in the SDK's
    /// milliseconds.
    fn wait_for_sources_ms(&self, timeout_ms: u32) -> bool {
        // SAFETY: the finder instance is live for this borrow.
        unsafe {
            self._ndi
                .api()
                .core
                .NDIlib_find_wait_for_sources(self.instance, timeout_ms)
        }
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
        // SAFETY: the SDK returns a live snapshot guarded by sources_lock until
        // current_sources_with completes its deep copy.
        unsafe {
            self.current_sources_with(
                |num_sources| {
                    self._ndi
                        .api()
                        .core
                        .NDIlib_find_get_current_sources(self.instance, num_sources)
                },
                |source| Source::try_from_raw(source, "NDIlib_find_get_current_sources"),
            )
        }
    }

    // Keep acquisition and conversion in one critical section: the SDK owns both
    // the array and its strings, so locking only the SDK call is insufficient.
    /// # Safety
    /// `fetch` must return a live array of the reported count, with readable
    /// source descriptors and C strings until all copies finish under the lock.
    unsafe fn current_sources_with(
        &self,
        fetch: impl FnOnce(&mut u32) -> *const NDIlib_source_t,
        mut copy: impl FnMut(*const NDIlib_source_t) -> Result<Source>,
    ) -> Result<Vec<Source>> {
        let _snapshot = self
            .sources_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut num_sources = 0;
        let sources_ptr = fetch(&mut num_sources);
        if sources_ptr.is_null() {
            return Ok(vec![]);
        }

        // A descriptor the SDK should not produce (null, naming neither a
        // source nor an address, or with a string longer than the limit) is
        // left out, so that one malformed entry does not hide the others.
        let mut sources = Vec::with_capacity(num_sources as usize);
        for i in 0..num_sources as usize {
            // SAFETY: the SDK reported `num_sources` entries in this live array.
            if let Ok(source) = copy(unsafe { sources_ptr.add(i) }) {
                sources.push(source);
            }
        }
        Ok(sources)
    }

    /// Poll discovery until `accept` selects a value or `timeout` elapses.
    ///
    /// After each source-list snapshot, `accept` is handed the current sources
    /// and may return `Some` to stop immediately; otherwise this blocks on the
    /// next source-list change and retries until the deadline. A single
    /// [`wait_for_sources`](Self::wait_for_sources) returns on the *first*
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
        mut accept: impl FnMut(Vec<Source>) -> Option<T>,
    ) -> Result<Option<T>> {
        // Validate up front so an over-long timeout is rejected even when the
        // first snapshot already satisfies `accept`.
        let deadline = Deadline::after(timeout)?;
        loop {
            if let Some(selected) = accept(self.current_sources()?) {
                return Ok(Some(selected));
            }
            if deadline.is_expired() {
                return Ok(None);
            }
            // Block until the source list changes or the window ends; either
            // way the next iteration re-snapshots. The wait is rounded up, so
            // the last one lasts until the deadline rather than polling.
            self.wait_for_sources_ms(deadline.remaining_ms());
        }
    }

    /// Discovers sources for up to `timeout`, returning the complete set.
    ///
    /// This honors the full `timeout` window and returns every source observed
    /// within it. Unlike a single [`wait_for_sources`](Self::wait_for_sources)
    /// followed by a snapshot — which returns on the *first* source-list change
    /// and so can miss responders that announce late on staggered or unicast
    /// (IP-hint-only) networks — enumeration here retains each source observed
    /// during the window.
    ///
    /// For an immediate, non-blocking snapshot of already-known sources use
    /// [`current_sources`](Self::current_sources); to resolve a single specific
    /// host use [`SourceCache::find_by_host`].
    ///
    /// # Arguments
    ///
    /// * `timeout` - Discovery window. [`Duration::ZERO`] returns an immediate
    ///   snapshot. Must not exceed [`crate::MAX_TIMEOUT`] (~49.7 days).
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
        self.poll_until_deadline(timeout, |sources| {
            accumulate_sources(&mut discovered, &mut seen, sources);
            None::<()>
        })?;
        Ok(discovered)
    }
}

impl Drop for Finder {
    fn drop(&mut self) {
        // SAFETY: the instance was created through this runtime and is
        // destroyed exactly once, before the runtime handle is released.
        unsafe { self._ndi.api().core.NDIlib_find_destroy(self.instance) };
    }
}

/// # Safety
///
/// The SDK finder handle may be moved between threads. Its runtime and creation
/// strings remain alive with it, and destruction requires exclusive ownership.
unsafe impl std::marker::Send for Finder {}

/// # Safety
///
/// The SDK forbids concurrent `NDIlib_find_get_current_sources` calls on one
/// instance and invalidates its returned buffers on the next call. `sources_lock`
/// serializes those calls through the complete deep copy into owned `Source`s.
/// Waiting for source changes does not invalidate those buffers; destruction
/// requires exclusive access through `Drop`.
unsafe impl std::marker::Sync for Finder {}

fn accumulate_sources(
    discovered: &mut Vec<Source>,
    seen: &mut HashSet<Source>,
    sources: Vec<Source>,
) {
    for source in sources {
        if seen.insert(source.clone()) {
            discovered.push(source);
        }
    }
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
/// NDI sources can be addressed via URL (for NDI HX sources) or IP address
/// (for standard NDI sources).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceAddress {
    /// URL address (typically for NDI HX sources).
    Url(String),
    /// IP address (for standard NDI sources).
    Ip(String),
}

impl SourceAddress {
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
    /// assert!(addr.contains_host("192.168.1.100"));
    /// assert!(!addr.contains_host("192.168.1"));
    ///
    /// let url = SourceAddress::Url("http://camera.local:8080".to_string());
    /// assert!(url.contains_host("camera.local"));
    /// ```
    pub fn contains_host(&self, host: &str) -> bool {
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

impl Source {
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
        // The two address fields share one pointer; a URL is told apart from
        // an IP address by its scheme.
        // SAFETY: both union fields are pointers of the same type.
        let address = unsafe { ndi_source.__bindgen_anon_1.p_url_address };

        let name = if ndi_source.p_ndi_name.is_null() {
            // The SDK accepts an address-only descriptor with a null name.
            if address.is_null() {
                return Err(failed());
            }
            String::new()
        } else {
            // SAFETY: the caller keeps the name readable through its terminator.
            unsafe { copy_sdk_string(ndi_source.p_ndi_name, operation) }?
        };

        let address = if address.is_null() {
            None
        } else {
            // SAFETY: the caller keeps the address readable through its terminator.
            let address = unsafe { copy_sdk_string(address, operation) }?;
            Some(if address.contains("://") {
                SourceAddress::Url(address)
            } else {
                SourceAddress::Ip(address)
            })
        };

        Ok(Source { name, address })
    }

    /// Convert to raw format for FFI use
    ///
    /// # Safety
    ///
    /// The returned RawSource struct uses #[repr(C)] to guarantee C-compatible layout
    /// for safe FFI interop with the NDI SDK.
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

/// A thread-safe cache of host-to-source lookups.
///
/// Resolving a hostname or IP address to the NDI [`Source`] it advertises
/// takes a network discovery, which can last seconds. A `SourceCache`
/// remembers each answer: [`find_by_host`](Self::find_by_host) discovers a
/// host the first time it is asked for, and afterwards returns the remembered
/// source without calling the NDI runtime at all.
///
/// The cache stores only owned source descriptors. It holds no [`Ndi`] handle,
/// so it does not keep NDI started, it can be created before a runtime is
/// selected, and it can outlive every handle. A lookup that has to search the
/// network uses the handle it is given.
///
/// An entry stays until [`invalidate`](Self::invalidate) or
/// [`clear`](Self::clear) removes it: the cache does not notice a source going
/// offline or changing address.
/// Equivalent hostname case/final-dot and numeric IP spellings share one entry.
/// Hostnames, IP addresses, and DNS aliases otherwise remain distinct identities.
///
/// # Thread Safety
///
/// `SourceCache` is thread-safe and can be shared across threads using `Arc<SourceCache>`.
/// Lookups of different hosts do not wait for one another's discovery.
///
/// # Examples
///
/// ```no_run
/// use grafton_ndi::{SourceCache, Ndi};
/// use std::time::Duration;
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = Ndi::new()?;
/// let cache = SourceCache::new();
///
/// // The first lookup of a host searches the network.
/// let source = cache.find_by_host(&ndi, "192.168.0.107", Duration::from_secs(5))?;
/// println!("Found source: {}", source);
///
/// // Later lookups of that host are answered from the cache.
/// let same_source = cache.find_by_host(&ndi, "192.168.0.107", Duration::from_secs(5))?;
/// assert_eq!(same_source, source);
/// # Ok(())
/// # }
/// ```
pub struct SourceCache {
    cache: Mutex<CacheState>,
}

struct CacheState {
    entries: HashMap<HostKey, Source>,
    generation: Option<u64>,
}

impl CacheState {
    fn invalidate_pending(&mut self) {
        self.generation = self.generation.and_then(|value| value.checked_add(1));
    }
}

impl SourceCache {
    /// Create an empty cache. This needs no NDI runtime.
    ///
    /// # Examples
    ///
    /// ```
    /// use grafton_ndi::SourceCache;
    ///
    /// let cache = SourceCache::new();
    /// assert!(cache.is_empty());
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Find the source advertised by a host, by IP address or hostname.
    ///
    /// A host found before is answered from the cache, without calling the
    /// NDI runtime. Otherwise a finder is created from `ndi` for the duration
    /// of the lookup, local sources included and `host` given to it as an
    /// extra address to query, and the first matching source is cached and
    /// returned.
    ///
    /// # Arguments
    ///
    /// * `ndi` - The runtime handle a discovery is run through. A cache hit
    ///   does not use it.
    /// * `host` - The hostname or IP address to search for
    /// * `timeout` - Maximum time to wait for source discovery.
    ///   Must not exceed [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Errors
    ///
    /// - [`Error::NoSourcesFound`] if no source matching the host is discovered
    ///   before the timeout expires
    /// - [`Error::InvalidInput`] if `host` is not a bare hostname or IP,
    ///   or `timeout` exceeds [`crate::MAX_TIMEOUT`]; both are checked before
    ///   the cache is consulted
    /// - [`Error::NativeOperationFailed`] if the finder cannot be created
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use grafton_ndi::{SourceCache, Ndi};
    /// use std::time::Duration;
    ///
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = Ndi::new()?;
    /// let cache = SourceCache::new();
    ///
    /// // Find by IP address
    /// let source = cache.find_by_host(&ndi, "192.168.0.107", Duration::from_secs(5))?;
    ///
    /// // Find by the machine component of an SDK source name
    /// let source = cache.find_by_host(&ndi, "CAMERA1", Duration::from_secs(5))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn find_by_host(&self, ndi: &Ndi, host: &str, timeout: Duration) -> Result<Source> {
        self.find_by_host_with(host, timeout, |query| {
            let hint = host
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
                .unwrap_or(host);
            let options = FinderOptions::new().extra_ips(hint);
            let finder = Finder::new(ndi, &options)?;
            finder.poll_until_deadline(timeout, |sources| {
                sources
                    .into_iter()
                    .find(|source| source.matches_query(query))
            })
        })
    }

    /// The cache around a discovery: validate the request, answer from the
    /// cache when possible, and otherwise run `discover` and remember what it
    /// finds.
    fn find_by_host_with(
        &self,
        host: &str,
        timeout: Duration,
        discover: impl FnOnce(HostQuery<'_>) -> Result<Option<Source>>,
    ) -> Result<Source> {
        let query = HostQuery::parse(host).ok_or_else(|| {
            Error::InvalidInput("Host must be a bare hostname or IP address".into())
        })?;
        to_ms_checked(timeout)?;
        let key = HostKeyBuffer::new(query);
        let generation = {
            let cache = self
                .cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cached) = cache.entries.get(key.as_str()) {
                return Ok(cached.clone());
            }
            cache.generation
        };

        // Discovery may wait for the SDK. Leave the cache mutex available to
        // invalidation and unrelated cache hits while this lookup is pending.
        let source = discover(query)?.ok_or_else(|| Error::NoSourcesFound {
            criteria: format!("host: {host}"),
        })?;
        {
            let mut cache = self
                .cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if generation.is_some() && cache.generation == generation {
                cache
                    .entries
                    .entry(key.into_owned())
                    .or_insert_with(|| source.clone());
            }
        }
        Ok(source)
    }

    /// Invalidate the cache entry for a specific host.
    ///
    /// This is useful when a source goes offline or when you want to force
    /// a fresh discovery on the next lookup. Discovery started before this
    /// call may return to its caller, but cannot repopulate the cache afterward.
    /// Any accepted equivalent spelling removes the same host identity.
    /// Malformed input removes no entry but still invalidates pending discovery.
    ///
    /// # Arguments
    ///
    /// * `host` - The hostname or IP address to remove from the cache
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use grafton_ndi::{SourceCache, Ndi};
    /// use std::time::Duration;
    ///
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = Ndi::new()?;
    /// let cache = SourceCache::new();
    /// let source = cache.find_by_host(&ndi, "192.168.0.107", Duration::from_secs(5))?;
    ///
    /// // Later, if the source goes offline
    /// cache.invalidate("192.168.0.107");
    ///
    /// // Next lookup will perform fresh discovery
    /// # Ok(())
    /// # }
    /// ```
    pub fn invalidate(&self, host: &str) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.invalidate_pending();
        if let Some(query) = HostQuery::parse(host) {
            cache.entries.remove(HostKeyBuffer::new(query).as_str());
        }
    }

    /// Clear all cached sources.
    ///
    /// This removes all entries from the cache, forcing fresh discovery
    /// for all subsequent lookups. Pending discovery cannot repopulate the
    /// cache after this call returns.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use grafton_ndi::{SourceCache, Ndi};
    /// use std::time::Duration;
    ///
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = Ndi::new()?;
    /// let cache = SourceCache::new();
    /// cache.find_by_host(&ndi, "192.168.0.107", Duration::from_secs(5))?;
    /// cache.find_by_host(&ndi, "192.168.0.108", Duration::from_secs(5))?;
    ///
    /// // Clear all cached sources
    /// cache.clear();
    /// # Ok(())
    /// # }
    /// ```
    pub fn clear(&self) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.invalidate_pending();
        cache.entries.clear();
    }

    /// Get the number of distinct cached host identities.
    ///
    /// This can be useful for monitoring cache usage and debugging.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use grafton_ndi::{SourceCache, Ndi};
    /// use std::time::Duration;
    ///
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = Ndi::new()?;
    /// let cache = SourceCache::new();
    /// assert_eq!(cache.len(), 0);
    ///
    /// cache.find_by_host(&ndi, "192.168.0.107", Duration::from_secs(5))?;
    /// assert_eq!(cache.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn len(&self) -> usize {
        let cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.entries.len()
    }

    /// Check if the cache is empty.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use grafton_ndi::{SourceCache, Ndi};
    /// use std::time::Duration;
    ///
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = Ndi::new()?;
    /// let cache = SourceCache::new();
    /// assert!(cache.is_empty());
    ///
    /// cache.find_by_host(&ndi, "192.168.0.107", Duration::from_secs(5))?;
    /// assert!(!cache.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    pub fn is_empty(&self) -> bool {
        let cache = self
            .cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.entries.is_empty()
    }
}

impl Default for SourceCache {
    fn default() -> Self {
        Self {
            cache: Mutex::new(CacheState {
                entries: HashMap::new(),
                generation: Some(0),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                let cache = SourceCache::new();
                let old =
                    Source::new("camera").with_address(SourceAddress::Ip("192.0.2.1:5960".into()));
                let fresh = old
                    .clone()
                    .with_address(SourceAddress::Ip("192.0.2.1:5961".into()));
                cache
                    .find_by_host_with(first, Duration::ZERO, |_| Ok(Some(old.clone())))
                    .unwrap();
                let (identical, identical_allocs) = crate::test_alloc::allocation_count(|| {
                    cache
                        .find_by_host_with(first, Duration::ZERO, |_| panic!("cache hit"))
                        .unwrap()
                });
                let (equivalent, equivalent_allocs) = crate::test_alloc::allocation_count(|| {
                    cache
                        .find_by_host_with(second, Duration::ZERO, |_| {
                            panic!("equivalent cache hit")
                        })
                        .unwrap()
                });
                assert_eq!(identical, old);
                assert_eq!(equivalent, old);
                assert_eq!(identical_allocs, 2); // The two owned descriptor strings.
                assert_eq!(equivalent_allocs, identical_allocs);
                assert_eq!(cache.len(), 1);
                assert!(cache
                    .find_by_host_with(second, Duration::MAX, |_| panic!("invalid timeout"))
                    .is_err());
                cache.invalidate(second);
                assert!(cache.is_empty());
                assert_eq!(
                    cache
                        .find_by_host_with(first, Duration::ZERO, |_| Ok(Some(fresh.clone())))
                        .unwrap(),
                    fresh
                );
                assert_eq!(cache.len(), 1);
                cache.invalidate("invalid:host:port");
                assert_eq!(cache.len(), 1);
                cache.clear();
                assert!(cache
                    .find_by_host_with(first, Duration::ZERO, |_| Err(Error::InvalidInput(
                        "discovery failed".into()
                    )))
                    .is_err());
                assert!(cache
                    .find_by_host_with(second, Duration::ZERO, |_| Ok(None))
                    .is_err());
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
        assert!(ipv6.contains_host("2001:db8::1"));
        assert!(ipv6.contains_host("[2001:db8::1]"));
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
                assert!(ipv6.contains_host("2001:db8::1"));
            }
        });
        assert_eq!(allocations, 0);
    }

    #[test]
    fn cache_selects_exact_host_and_rejects_invalid_queries_before_discovery() {
        let cache = SourceCache::new();
        let wrong = Source::new("CAMERA10 (Program)")
            .with_address(SourceAddress::Ip("192.168.1.100:5960".into()));
        let right = Source::new("CAMERA1 (Program)")
            .with_address(SourceAddress::Ip("192.168.1.1:5961".into()));
        let timeout = Duration::ZERO;
        let selected = cache
            .find_by_host_with("192.168.1.1", timeout, |query| {
                Ok([wrong.clone(), right.clone()]
                    .into_iter()
                    .find(|s| s.matches_query(query)))
            })
            .unwrap();
        assert_eq!(selected, right);
        assert_eq!(cache.len(), 1);
        assert_eq!(
            cache
                .find_by_host_with("192.168.1.1", timeout, |_| panic!("cache hit"))
                .unwrap(),
            right
        );
        for invalid in ["", "192.168.1.1:5961", "camera?query", "foo\0bar"] {
            assert!(matches!(
                cache.find_by_host_with(invalid, timeout, |_| panic!(
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
            let cache = Arc::new(SourceCache::new());
            let (started_tx, started_rx) = mpsc::channel();
            let (resume_tx, resume_rx) = mpsc::channel();
            let old = Source::new("old").with_address(SourceAddress::Ip("192.0.2.1:5960".into()));
            let fresh =
                Source::new("fresh").with_address(SourceAddress::Ip("192.0.2.1:5961".into()));
            let pending = Arc::clone(&cache);
            let old_result = old.clone();
            let thread = std::thread::spawn(move || {
                pending
                    .find_by_host_with("CAMERA.LOCAL.", Duration::ZERO, |_| {
                        started_tx.send(()).unwrap();
                        resume_rx.recv().unwrap();
                        Ok(Some(old_result))
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
                .find_by_host_with("camera.local", Duration::ZERO, |_| Ok(Some(fresh.clone())))
                .unwrap();
            assert_eq!(found, fresh);
            resume_tx.send(()).unwrap();
            assert_eq!(thread.join().unwrap(), old);
            assert_eq!(
                cache
                    .find_by_host_with("Camera.Local.", Duration::ZERO, |_| panic!(
                        "fresh entry was overwritten"
                    ))
                    .unwrap(),
                fresh
            );
        }
    }

    #[test]
    fn absent_invalidation_and_empty_clear_suppress_pending_fills() {
        use std::sync::{mpsc, Arc};
        for clear in [false, true] {
            let cache = Arc::new(SourceCache::new());
            let (started_tx, started_rx) = mpsc::channel();
            let (resume_tx, resume_rx) = mpsc::channel();
            let pending = Arc::clone(&cache);
            let thread = std::thread::spawn(move || {
                pending
                    .find_by_host_with("camera.local", Duration::ZERO, |_| {
                        started_tx.send(()).unwrap();
                        resume_rx.recv().unwrap();
                        Ok(Some(Source::new("CAMERA (Program)")))
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
        let cache = SourceCache::new();
        cache.cache.lock().unwrap().generation = Some(u64::MAX);
        cache.invalidate("absent");
        let source = Source::new("camera").with_address(SourceAddress::Ip("192.0.2.1".into()));
        assert_eq!(
            cache
                .find_by_host_with("192.0.2.1", Duration::ZERO, |_| Ok(Some(source.clone())))
                .unwrap(),
            source
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
        accumulate_sources(&mut discovered, &mut seen, vec![first.clone()]);
        accumulate_sources(&mut discovered, &mut seen, Vec::new());
        accumulate_sources(
            &mut discovered,
            &mut seen,
            vec![first.clone(), changed.clone(), distinct_variant.clone()],
        );
        assert_eq!(discovered, vec![first, changed, distinct_variant]);
    }

    #[test]
    fn cache_validates_timeout_before_returning_cached_source() {
        let cache = SourceCache::new();
        cache.cache.lock().unwrap().entries.insert(
            HostKeyBuffer::new(HostQuery::parse("camera").unwrap()).into_owned(),
            Source::new("camera"),
        );
        for host in ["camera", "not_cached"] {
            assert!(matches!(
                cache.find_by_host_with(host, Duration::MAX, |_| panic!("invalid timeout")),
                Err(Error::InvalidInput(_))
            ));
        }
        // A hit is answered without reaching discovery, the only step that
        // uses the runtime.
        assert_eq!(
            cache
                .find_by_host_with("camera", Duration::ZERO, |_| panic!("cache hit"))
                .unwrap(),
            Source::new("camera")
        );
    }

    #[test]
    fn concurrent_snapshots_keep_sdk_buffers_exclusive_until_copied() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Barrier, TryLockError,
        };

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
                    // SAFETY: raw and both C strings stay live through copying;
                    // this test's snapshot lock serializes their use.
                    let sources = unsafe { finder.current_sources_with(
                        |count| {
                            // Model an SDK snapshot whose buffers remain borrowed
                            // until conversion finishes, not just until fetch returns.
                            assert_eq!(active_snapshot.fetch_add(1, Ordering::SeqCst), 0);
                            assert!(matches!(finder.sources_lock.try_lock(), Err(TryLockError::WouldBlock)));
                            *count = 1;
                            std::thread::yield_now();
                            &raw
                        },
                        |source| {
                            // Deterministically catches releasing the lock before
                            // copying, even if competing threads are not scheduled.
                            assert!(matches!(finder.sources_lock.try_lock(), Err(TryLockError::WouldBlock)));
                            std::thread::yield_now();
                            let owned = Source::try_from_raw(source, "test snapshot");
                            assert_eq!(active_snapshot.fetch_sub(1, Ordering::SeqCst), 1);
                            owned
                        },
                    ) }.unwrap();
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
                    let _guard = finder.sources_lock.lock().unwrap();
                    panic!("interrupted snapshot");
                })
                .join()
                .is_err());
        });
        assert!(unsafe {
            finder.current_sources_with(|_| ptr::null(), |_| panic!("empty snapshot must not copy"))
        }
        .unwrap()
        .is_empty());
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
        // SAFETY: the array and its strings stay live through the copy.
        let sources = unsafe {
            finder.current_sources_with(
                |count| {
                    *count = snapshot.len() as u32;
                    snapshot.as_ptr()
                },
                |source| Source::try_from_raw(source, "test snapshot"),
            )
        }
        .unwrap();
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

        let cache = Arc::new(SourceCache::default());
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
