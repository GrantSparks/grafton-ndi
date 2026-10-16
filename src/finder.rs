//! NDI source discovery and network browsing.

use std::{
    collections::{HashMap, HashSet},
    ffi::{CStr, CString},
    fmt::{self, Display, Formatter},
    net::IpAddr,
    ptr,
    sync::Mutex,
    time::{Duration, Instant},
};

use crate::{ndi_lib::*, to_ms_checked, Error, Result, NDI};

/// Configuration for NDI source discovery.
///
/// Use the builder pattern to create instances with specific settings.
///
/// # Examples
///
/// ```
/// use grafton_ndi::FinderOptions;
///
/// // Find all sources including local ones
/// let finder = FinderOptions::builder()
///     .show_local_sources(true)
///     .build();
///
/// // Find sources in specific groups
/// let finder = FinderOptions::builder()
///     .groups("Public,Studio")
///     .build();
///
/// // Find sources on specific hosts
/// let finder = FinderOptions::builder()
///     .extra_ips("192.168.1.100,10.0.0.100")
///     .build();
/// ```
#[derive(Debug, Clone)]
pub struct FinderOptions {
    /// Whether to include local sources in discovery.
    pub show_local_sources: bool,
    /// Comma-separated list of groups to search (e.g., "Public,Private").
    pub groups: Option<String>,
    /// Additional comma-separated IP addresses to query.
    pub extra_ips: Option<String>,
}

impl Default for FinderOptions {
    fn default() -> Self {
        Self::builder().build()
    }
}

impl FinderOptions {
    /// Create a builder for configuring find options
    pub fn builder() -> FinderOptionsBuilder {
        FinderOptionsBuilder::new()
    }
}

/// Builder for configuring FinderOptions with ergonomic method chaining
#[derive(Debug, Clone)]
pub struct FinderOptionsBuilder {
    show_local_sources: Option<bool>,
    groups: Option<String>,
    extra_ips: Option<String>,
}

impl FinderOptionsBuilder {
    /// Creates a new builder with default settings.
    ///
    /// Default settings:
    /// - `show_local_sources`: `true`
    /// - `groups`: `None` (use SDK-configured default groups)
    /// - `extra_ips`: `None` (no additional IPs)
    pub fn new() -> Self {
        Self {
            show_local_sources: None,
            groups: None,
            extra_ips: None,
        }
    }

    /// Configure whether to show local sources
    #[must_use]
    pub fn show_local_sources(mut self, show: bool) -> Self {
        self.show_local_sources = Some(show);
        self
    }

    /// Set the groups to search
    #[must_use]
    pub fn groups<S: Into<String>>(mut self, groups: S) -> Self {
        self.groups = Some(groups.into());
        self
    }

    /// Set extra IPs to search
    #[must_use]
    pub fn extra_ips<S: Into<String>>(mut self, ips: S) -> Self {
        self.extra_ips = Some(ips.into());
        self
    }

    /// Build the FinderOptions
    #[must_use]
    pub fn build(self) -> FinderOptions {
        FinderOptions {
            show_local_sources: self.show_local_sources.unwrap_or(true),
            groups: self.groups,
            extra_ips: self.extra_ips,
        }
    }
}

impl Default for FinderOptionsBuilder {
    fn default() -> Self {
        Self::new()
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
/// # use grafton_ndi::{NDI, FinderOptions, Finder};
/// # use std::time::Duration;
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = NDI::new()?;
/// let options = FinderOptions::builder().show_local_sources(true).build();
/// let finder = Finder::new(&ndi, &options)?;
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
    _ndi: NDI,
}

impl Finder {
    /// Creates a new source finder with the specified settings.
    ///
    /// # Arguments
    ///
    /// * `ndi` - The NDI instance (cloned internally to keep the runtime alive)
    /// * `settings` - Configuration for source discovery
    ///
    /// # Errors
    ///
    /// Returns an error if the finder cannot be created, typically due to
    /// invalid settings or network issues.
    pub fn new(ndi: &NDI, settings: &FinderOptions) -> Result<Self> {
        let groups_cstr = settings
            .groups
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(Error::InvalidCString)?;
        let extra_ips_cstr = settings
            .extra_ips
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(Error::InvalidCString)?;

        let create_settings = NDIlib_find_create_t {
            show_local_sources: settings.show_local_sources,
            p_groups: groups_cstr.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
            p_extra_ips: extra_ips_cstr.as_ref().map_or(ptr::null(), |s| s.as_ptr()),
        };

        let instance = unsafe { NDIlib_find_create_v2(&create_settings) };
        if instance.is_null() {
            return Err(Error::InitializationFailed(
                "NDIlib_find_create_v2 failed".into(),
            ));
        }
        Ok(Self {
            instance,
            sources_lock: Mutex::new(()),
            _groups: groups_cstr,
            _extra_ips: extra_ips_cstr,
            _ndi: ndi.clone(),
        })
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
    /// Returns [`Error::InvalidConfiguration`] if `timeout` exceeds [`crate::MAX_TIMEOUT`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, FinderOptions, Finder};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// // Wait up to 5 seconds for changes
    /// if finder.wait_for_sources(Duration::from_secs(5))? {
    ///     println!("Source list changed!");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn wait_for_sources(&self, timeout: Duration) -> Result<bool> {
        let timeout_ms = to_ms_checked(timeout)?;
        Ok(unsafe { NDIlib_find_wait_for_sources(self.instance, timeout_ms) })
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
    /// # use grafton_ndi::{NDI, FinderOptions, Finder};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
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
                |num_sources| NDIlib_find_get_current_sources(self.instance, num_sources),
                |source| Source::try_from_raw(source),
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

        // Convert each source, skipping any that fail null checks
        let mut sources = Vec::with_capacity(num_sources as usize);
        for i in 0..num_sources {
            let source_ptr = unsafe { sources_ptr.add(i as usize) };
            match copy(source_ptr) {
                Ok(source) => sources.push(source),
                Err(_e) => {
                    // Skip invalid sources (null pointers from SDK)
                    // This is a defensive measure - the SDK should not return null entries,
                    // but we handle it gracefully if it does
                    #[cfg(debug_assertions)]
                    eprintln!("Warning: Skipping invalid source at index {i}: {_e}");
                }
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
        to_ms_checked(timeout)?;
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(selected) = accept(self.current_sources()?) {
                return Ok(Some(selected));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            // Block until the source list changes or the remaining window
            // expires; either way the next iteration re-snapshots and the
            // zero-`remaining` check then terminates the loop.
            self.wait_for_sources(remaining)?;
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
    /// Returns [`Error::InvalidConfiguration`] if `timeout` exceeds [`crate::MAX_TIMEOUT`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, FinderOptions, Finder};
    /// # use std::time::Duration;
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
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
        unsafe { NDIlib_find_destroy(self.instance) };
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

/// Network address of an NDI source.
///
/// NDI sources can be addressed via URL (for NDI HX sources) or IP address
/// (for standard NDI sources).
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub enum SourceAddress {
    /// No address available.
    #[default]
    None,
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
            Self::None => None,
            Self::Ip(address) | Self::Url(address) => parse_authority(address),
        }
    }
}

/// Represents an NDI source discovered on the network.
///
/// Equality and hashing compare the complete name and address, including the
/// address variant; they do not imply persistent device identity.
///
/// Sources contain a human-readable name and network address. The name
/// typically includes the machine name and source name (e.g., "MACHINE (Source)").
///
/// # Examples
///
/// ```
/// use grafton_ndi::{Source, SourceAddress};
///
/// let source = Source {
///     name: "LAPTOP (Camera 1)".to_string(),
///     address: SourceAddress::Ip("192.168.1.100:5960".to_string()),
/// };
///
/// println!("Source: {}", source); // Displays: LAPTOP (Camera 1)@192.168.1.100:5960
/// ```
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub struct Source {
    /// The NDI source name (e.g., "MACHINE (Source Name)").
    pub name: String,
    /// The network address for connecting to this source.
    pub address: SourceAddress,
}

#[repr(C)]
pub(crate) struct RawSource {
    _name: CString,
    _url_address: Option<CString>,
    _ip_address: Option<CString>,
    pub raw: NDIlib_source_t,
}

impl Source {
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
    /// let source = Source {
    ///     name: "CAMERA1 (Chan1, 192.168.0.107)".to_string(),
    ///     address: SourceAddress::Ip("192.168.0.107:5960".to_string()),
    /// };
    ///
    /// assert!(source.matches_host("192.168.0.107"));
    /// assert!(source.matches_host("CAMERA1"));
    /// assert!(!source.matches_host("192.168.1.1"));
    /// ```
    pub fn matches_host(&self, host: &str) -> bool {
        HostQuery::parse(host).is_some_and(|query| self.matches_query(query))
    }

    fn matches_query(&self, query: HostQuery<'_>) -> bool {
        if self.address.matches_query(query) {
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
    /// For URL-based sources, this extracts the hostname portion.
    ///
    /// # Returns
    ///
    /// `Some(ip)` if an IP or hostname is found, `None` otherwise.
    ///
    /// # Examples
    ///
    /// ```
    /// use grafton_ndi::{Source, SourceAddress};
    ///
    /// let source = Source {
    ///     name: "CAMERA1".to_string(),
    ///     address: SourceAddress::Ip("192.168.1.100:5960".to_string()),
    /// };
    ///
    /// assert_eq!(source.ip_address(), Some("192.168.1.100"));
    /// ```
    pub fn ip_address(&self) -> Option<&str> {
        self.address.authority().map(|authority| authority.host)
    }

    /// Extract the hostname or IP without port.
    ///
    /// This is an alias for `ip_address()` for better API discoverability.
    ///
    /// # Examples
    ///
    /// ```
    /// use grafton_ndi::{Source, SourceAddress};
    ///
    /// let source = Source {
    ///     name: "CAMERA1".to_string(),
    ///     address: SourceAddress::Ip("192.168.1.100:5960".to_string()),
    /// };
    ///
    /// assert_eq!(source.host(), Some("192.168.1.100"));
    /// ```
    pub fn host(&self) -> Option<&str> {
        self.ip_address()
    }

    /// Copy an SDK source descriptor into owned strings after null checks.
    ///
    /// Null checks cannot validate dangling or otherwise invalid pointers. The
    /// SDK must provide a live descriptor and readable NUL-terminated strings.
    ///
    /// # Errors
    ///
    /// Returns `Error::NullPointer` if:
    /// - The source pointer itself is null
    /// - Both the `p_ndi_name` and source address fields are null
    ///
    /// # Safety
    ///
    /// A non-null `source_ptr` must point to a live `NDIlib_source_t`. Its non-null
    /// string pointers must remain readable through their NUL terminators until
    /// copying finishes.
    pub(crate) unsafe fn try_from_raw(source_ptr: *const NDIlib_source_t) -> Result<Self> {
        // Check top-level pointer
        if source_ptr.is_null() {
            return Err(Error::NullPointer("NDIlib_source_t pointer".into()));
        }

        let ndi_source = unsafe { &*source_ptr };

        let name = if ndi_source.p_ndi_name.is_null() {
            // The SDK accepts an address-only descriptor with a null name.
            if unsafe { ndi_source.__bindgen_anon_1.p_url_address }.is_null() {
                return Err(Error::NullPointer("NDIlib_source_t::p_ndi_name".into()));
            }
            String::new()
        } else {
            unsafe {
                CStr::from_ptr(ndi_source.p_ndi_name)
                    .to_string_lossy()
                    .into_owned()
            }
        };

        // For unions, we need to determine which field is active.
        // NDI SDK convention: URL addresses are used for NDI HX sources,
        // IP addresses for regular sources. We check URL first as it's
        // typically used for newer/HX sources.
        let address = unsafe {
            if !ndi_source.__bindgen_anon_1.p_url_address.is_null() {
                let url_str = CStr::from_ptr(ndi_source.__bindgen_anon_1.p_url_address)
                    .to_string_lossy()
                    .into_owned();
                if url_str.contains("://") {
                    SourceAddress::Url(url_str)
                } else {
                    SourceAddress::Ip(url_str)
                }
            } else {
                SourceAddress::None
            }
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
            SourceAddress::Url(url) => {
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
            SourceAddress::Ip(ip) => {
                let ip_cstr = CString::new(ip.clone()).map_err(Error::InvalidCString)?;
                let p_ip = ip_cstr.as_ptr();
                (
                    None,
                    Some(ip_cstr),
                    NDIlib_source_t__bindgen_ty_1 { p_ip_address: p_ip },
                )
            }
            SourceAddress::None => (
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
            SourceAddress::Url(url) => write!(f, "{name}@{url}", name = self.name),
            SourceAddress::Ip(ip) => write!(f, "{name}@{ip}", name = self.name),
            SourceAddress::None => write!(f, "{name}", name = self.name),
        }
    }
}

/// Thread-safe cache for NDI source discovery.
///
/// `SourceCache` eliminates the need for applications to manually cache NDI instances
/// and discovered sources. Discovery initializes the runtime on a cache miss.
/// Cached sources own their strings and need no live runtime. Entries remain
/// cached until explicitly invalidated or cleared.
///
/// # Thread Safety
///
/// `SourceCache` is thread-safe and can be shared across threads using `Arc<SourceCache>`.
/// Interior mutability is handled internally with proper synchronization.
///
/// # Examples
///
/// ```no_run
/// use grafton_ndi::SourceCache;
/// use std::time::Duration;
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// // Create a cache instance
/// let cache = SourceCache::new();
///
/// // Find a source by hostname or IP with automatic caching
/// let source = cache.find_by_host("192.168.0.107", Duration::from_secs(5))?;
/// println!("Found source: {}", source);
///
/// // Subsequent lookups use the cache
/// let same_source = cache.find_by_host("192.168.0.107", Duration::from_secs(5))?;
///
/// # Ok(())
/// # }
/// ```
pub struct SourceCache {
    cache: Mutex<CacheState>,
}

struct CacheState {
    entries: HashMap<String, Source>,
    generation: Option<u64>,
}

impl CacheState {
    fn invalidate_pending(&mut self) {
        self.generation = self.generation.and_then(|value| value.checked_add(1));
    }
}

impl SourceCache {
    /// Create a new source cache.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use grafton_ndi::SourceCache;
    ///
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let cache = SourceCache::new();
    /// # Ok(())
    /// # }
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Find a source by IP address or hostname with built-in caching.
    ///
    /// This method handles NDI initialization and source discovery internally.
    /// If a source matching the host has been previously found, it returns the
    /// cached result. Otherwise, it performs NDI discovery and caches the result.
    ///
    /// # Arguments
    ///
    /// * `host` - The hostname or IP address to search for
    /// * `timeout` - Maximum time to wait for source discovery.
    ///   Must not exceed [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Returns
    ///
    /// The discovered source, or an error if no matching source is found or
    /// the timeout expires.
    ///
    /// # Errors
    ///
    /// - [`Error::NoSourcesFound`] if no source matching the host is discovered
    /// - [`Error::InvalidConfiguration`] if `host` is not a bare hostname or IP,
    ///   or `timeout` exceeds [`crate::MAX_TIMEOUT`]
    /// - Other errors if NDI initialization or discovery fails
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use grafton_ndi::SourceCache;
    /// use std::time::Duration;
    ///
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let cache = SourceCache::new();
    ///
    /// // Find by IP address
    /// let source = cache.find_by_host("192.168.0.107", Duration::from_secs(5))?;
    ///
    /// // Find by the machine component of an SDK source name
    /// let source = cache.find_by_host("CAMERA1", Duration::from_secs(5))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn find_by_host(&self, host: &str, timeout: Duration) -> Result<Source> {
        self.find_by_host_with(host, timeout, |query| {
            let ndi = NDI::new()?;
            let hint = host
                .strip_prefix('[')
                .and_then(|value| value.strip_suffix(']'))
                .unwrap_or(host);
            let options = FinderOptions::builder()
                .show_local_sources(true)
                .extra_ips(hint)
                .build();
            let finder = Finder::new(&ndi, &options)?;
            finder.poll_until_deadline(timeout, |sources| {
                sources
                    .into_iter()
                    .find(|source| source.matches_query(query))
            })
        })
    }

    fn find_by_host_with(
        &self,
        host: &str,
        timeout: Duration,
        discover: impl FnOnce(HostQuery<'_>) -> Result<Option<Source>>,
    ) -> Result<Source> {
        let query = HostQuery::parse(host).ok_or_else(|| {
            Error::InvalidConfiguration("Host must be a bare hostname or IP address".into())
        })?;
        to_ms_checked(timeout)?;
        let generation = {
            let cache = self
                .cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cached) = cache.entries.get(host) {
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
                    .entry(host.to_string())
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
    ///
    /// # Arguments
    ///
    /// * `host` - The hostname or IP address to remove from the cache
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use grafton_ndi::SourceCache;
    /// use std::time::Duration;
    ///
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let cache = SourceCache::new();
    /// let source = cache.find_by_host("192.168.0.107", Duration::from_secs(5))?;
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
        cache.entries.remove(host);
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
    /// use grafton_ndi::SourceCache;
    /// use std::time::Duration;
    ///
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let cache = SourceCache::new();
    /// cache.find_by_host("192.168.0.107", Duration::from_secs(5))?;
    /// cache.find_by_host("192.168.0.108", Duration::from_secs(5))?;
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

    /// Get the number of cached sources.
    ///
    /// This can be useful for monitoring cache usage and debugging.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use grafton_ndi::SourceCache;
    /// use std::time::Duration;
    ///
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let cache = SourceCache::new();
    /// assert_eq!(cache.len(), 0);
    ///
    /// cache.find_by_host("192.168.0.107", Duration::from_secs(5))?;
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
    /// use grafton_ndi::SourceCache;
    /// use std::time::Duration;
    ///
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let cache = SourceCache::new();
    /// assert!(cache.is_empty());
    ///
    /// cache.find_by_host("192.168.0.107", Duration::from_secs(5))?;
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

    #[test]
    fn exact_host_matching_ignores_labels_ports_and_url_payloads() {
        let other = Source {
            name: "CAMERA10 (192.168.1.1)".into(),
            address: SourceAddress::Ip("192.168.1.100:5960".into()),
        };
        assert!(!other.matches_host("192.168.1.1"));
        assert!(!other.matches_host("5960"));
        assert!(!other.matches_host("CAMERA1"));
        let exact = Source {
            name: "CAMERA1 (Program)".into(),
            address: SourceAddress::Url(
                "http://user:pass@Camera.Local:8080/ndi?next=192.0.2.1".into(),
            ),
        };
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
        let wrong = Source {
            name: "CAMERA10 (Program)".into(),
            address: SourceAddress::Ip("192.168.1.100:5960".into()),
        };
        let right = Source {
            name: "CAMERA1 (Program)".into(),
            address: SourceAddress::Ip("192.168.1.1:5961".into()),
        };
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
                Err(Error::InvalidConfiguration(_))
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
            let old = Source {
                name: "old".into(),
                address: SourceAddress::Ip("192.0.2.1:5960".into()),
            };
            let fresh = Source {
                name: "fresh".into(),
                address: SourceAddress::Ip("192.0.2.1:5961".into()),
            };
            let pending = Arc::clone(&cache);
            let old_result = old.clone();
            let thread = std::thread::spawn(move || {
                pending
                    .find_by_host_with("192.0.2.1", Duration::ZERO, |_| {
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
                cache.invalidate("192.0.2.1");
            }
            assert_eq!(cache.len(), 0);
            let found = cache
                .find_by_host_with("192.0.2.1", Duration::ZERO, |_| Ok(Some(fresh.clone())))
                .unwrap();
            assert_eq!(found, fresh);
            resume_tx.send(()).unwrap();
            assert_eq!(thread.join().unwrap(), old);
            assert_eq!(
                cache
                    .find_by_host_with("192.0.2.1", Duration::ZERO, |_| panic!(
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
                        Ok(Some(Source {
                            name: "CAMERA (Program)".into(),
                            address: SourceAddress::None,
                        }))
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
        let source = Source {
            name: "camera".into(),
            address: SourceAddress::Ip("192.0.2.1".into()),
        };
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
        assert!(FinderOptions::default().show_local_sources);
        assert_eq!(
            FinderOptions::default().show_local_sources,
            FinderOptions::builder().build().show_local_sources
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
            let source = Source {
                name: String::new(),
                address: SourceAddress::Url(address.into()),
            };
            assert_eq!(source.host(), Some(host), "{address}");
            assert_eq!(source.address.port(), port, "{address}");
        }
        assert!(parse_authority("[::1").is_none());
        assert!(parse_authority("camera:invalid").is_none());
        assert!(parse_authority("").is_none());
    }

    #[test]
    fn discovery_accumulates_structurally_distinct_sources_in_order() {
        let first = Source {
            name: "Camera".into(),
            address: SourceAddress::Ip("192.0.2.1".into()),
        };
        let mut changed = first.clone();
        changed.address = SourceAddress::Ip("192.0.2.2".into());
        let mut distinct_variant = first.clone();
        distinct_variant.address = SourceAddress::Url("192.0.2.1".into());
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
        cache
            .cache
            .lock()
            .unwrap()
            .entries
            .insert("camera".into(), Source::default());
        for host in ["camera", "not_cached"] {
            assert!(matches!(
                cache.find_by_host(host, Duration::MAX),
                Err(Error::InvalidConfiguration(_))
            ));
        }
        assert_eq!(
            cache.find_by_host("camera", Duration::ZERO).unwrap(),
            Source::default()
        );
    }

    #[test]
    fn concurrent_snapshots_keep_sdk_buffers_exclusive_until_copied() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Barrier, TryLockError,
        };

        let ndi = NDI::new().unwrap();
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
                            let owned = Source::try_from_raw(source);
                            assert_eq!(active_snapshot.fetch_sub(1, Ordering::SeqCst), 1);
                            owned
                        },
                    ) }.unwrap();
                    drop(name);
                    drop(address);
                    assert_eq!(sources[0].name, format!("Camera {index}"));
                    assert!(matches!(&sources[0].address, SourceAddress::Ip(ip) if ip == &format!("192.0.2.{index}:5960")));
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
        // Test that null pointer is rejected
        let result = unsafe { Source::try_from_raw(ptr::null()) };
        assert!(result.is_err());
        match result {
            Err(Error::NullPointer(msg)) => {
                assert!(msg.contains("NDIlib_source_t pointer"));
            }
            _ => panic!("Expected NullPointer error"),
        }
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

        let result = unsafe { Source::try_from_raw(&source as *const _) };
        assert!(result.is_err());
        match result {
            Err(Error::NullPointer(msg)) => {
                assert!(msg.contains("p_ndi_name"));
            }
            _ => panic!("Expected NullPointer error for null name"),
        }
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
        let source = unsafe { Source::try_from_raw(&raw) }.unwrap();
        assert!(source.name.is_empty());
        assert!(
            matches!(source.address, SourceAddress::Url(ref address) if address == "ndi://127.0.0.1:5960")
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

        let result = unsafe { Source::try_from_raw(&source as *const _) };
        assert!(result.is_ok());

        let source = result.unwrap();
        assert_eq!(source.name, "Test Source");
        match source.address {
            SourceAddress::Ip(ip_str) => {
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

        let result = unsafe { Source::try_from_raw(&source as *const _) };
        assert!(result.is_ok());

        let source = result.unwrap();
        assert_eq!(source.name, "HX Source");
        match source.address {
            SourceAddress::Url(url_str) => {
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

        let result = unsafe { Source::try_from_raw(&source as *const _) };
        assert!(result.is_ok());

        let source = result.unwrap();
        assert_eq!(source.name, "Source No Addr");
        match source.address {
            SourceAddress::None => {}
            _ => panic!("Expected None address"),
        }
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
