//! NDI source discovery and network browsing.

use std::{
    collections::HashMap,
    ffi::{CStr, CString},
    fmt::{self, Display, Formatter},
    marker::PhantomData,
    ptr,
    sync::{Arc, Mutex},
    time::Duration,
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
/// // Find sources on specific network segments
/// let finder = FinderOptions::builder()
///     .extra_ips("192.168.1.0/24,10.0.0.0/24")
///     .build();
/// ```
#[derive(Debug, Default)]
pub struct FinderOptions {
    /// Whether to include local sources in discovery.
    pub show_local_sources: bool,
    /// Comma-separated list of groups to search (e.g., "Public,Private").
    pub groups: Option<String>,
    /// Additional IP addresses or ranges to search.
    pub extra_ips: Option<String>,
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
    /// - `groups`: `None` (search all groups)
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
///     let sources = finder.sources(Duration::ZERO)?;
///     for source in sources {
///         println!("Found: {}", source);
///     }
/// }
/// # Ok(())
/// # }
/// ```
pub struct Finder<'a> {
    instance: NDIlib_find_instance_t,
    _groups: Option<CString>,    // Hold ownership of CStrings
    _extra_ips: Option<CString>, // to ensure they outlive SDK usage
    ndi: PhantomData<&'a NDI>,
}

impl<'a> Finder<'a> {
    /// Creates a new source finder with the specified settings.
    ///
    /// # Arguments
    ///
    /// * `ndi` - The NDI instance (must outlive this `Finder`)
    /// * `settings` - Configuration for source discovery
    ///
    /// # Errors
    ///
    /// Returns an error if the finder cannot be created, typically due to
    /// invalid settings or network issues.
    pub fn new(_ndi: &'a NDI, settings: &FinderOptions) -> Result<Self> {
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
            _groups: groups_cstr,
            _extra_ips: extra_ips_cstr,
            ndi: PhantomData,
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
        let mut num_sources = 0;
        let sources_ptr =
            unsafe { NDIlib_find_get_current_sources(self.instance, &mut num_sources) };
        if sources_ptr.is_null() {
            return Ok(vec![]);
        }

        // Convert each source, skipping any that fail null checks
        let mut sources = Vec::with_capacity(num_sources as usize);
        for i in 0..num_sources {
            let source_ptr = unsafe { sources_ptr.add(i as usize) };
            match Source::try_from_raw(source_ptr) {
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

    /// Returns the current list of discovered sources.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Time to wait for sources ([`Duration::ZERO`] = immediate).
    ///   Must not exceed [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Returns
    ///
    /// A vector of discovered sources. May be empty if no sources are found.
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
    /// // Get sources immediately
    /// let sources = finder.sources(Duration::ZERO)?;
    ///
    /// // Get sources with 1 second timeout
    /// let sources = finder.sources(Duration::from_secs(1))?;
    ///
    /// for source in sources {
    ///     println!("{}", source);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn sources(&self, timeout: Duration) -> Result<Vec<Source>> {
        let timeout_ms = to_ms_checked(timeout)?;
        let mut num_sources = 0;
        let sources_ptr =
            unsafe { NDIlib_find_get_sources(self.instance, &mut num_sources, timeout_ms) };
        if sources_ptr.is_null() {
            return Ok(vec![]);
        }

        // Convert each source, skipping any that fail null checks
        let mut sources = Vec::with_capacity(num_sources as usize);
        for i in 0..num_sources {
            let source_ptr = unsafe { sources_ptr.add(i as usize) };
            match Source::try_from_raw(source_ptr) {
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

    /// Waits for sources and then returns the current list.
    ///
    /// This is a convenience method that combines [`wait_for_sources`](Self::wait_for_sources)
    /// followed by [`sources`](Self::sources) with a zero timeout. Even if no "change" is signaled,
    /// cached sources may exist and will be returned.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Maximum time to wait for source changes.
    ///   Must not exceed [`crate::MAX_TIMEOUT`] (~49.7 days).
    ///
    /// # Returns
    ///
    /// A vector of discovered sources. May be empty if no sources are found.
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
    /// // Wait up to 5 seconds and get sources
    /// let sources = finder.find_sources(Duration::from_secs(5))?;
    ///
    /// for source in sources {
    ///     println!("Found: {}", source);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn find_sources(&self, timeout: Duration) -> Result<Vec<Source>> {
        let _changed = self.wait_for_sources(timeout)?; // intentionally ignored
        self.sources(Duration::ZERO)
    }
}

impl Drop for Finder<'_> {
    fn drop(&mut self) {
        unsafe { NDIlib_find_destroy(self.instance) };
    }
}

/// # Safety
///
/// The NDI SDK documentation states that find operations are thread-safe.
/// `NDIlib_find_create_v2`, `NDIlib_find_wait_for_sources`, and `NDIlib_find_get_sources`
/// can be called from multiple threads. The Finder struct only holds an opaque pointer
/// returned by the SDK and does not perform any mutations that could cause data races.
unsafe impl std::marker::Send for Finder<'_> {}

/// # Safety
///
/// The NDI SDK documentation guarantees thread-safety for find operations.
/// Multiple threads can safely call methods on a shared Finder instance as the
/// SDK handles all necessary synchronization internally.
unsafe impl std::marker::Sync for Finder<'_> {}

/// Network address of an NDI source.
///
/// NDI sources can be addressed via URL (for NDI HX sources) or IP address
/// (for standard NDI sources).
#[derive(Debug, Default, Clone)]
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
    /// Check if this address contains the given host or IP.
    ///
    /// This performs a substring match against the address string, useful for
    /// finding sources by hostname or IP address.
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
    /// assert!(addr.contains_host("192.168.1"));
    ///
    /// let url = SourceAddress::Url("http://camera.local:8080".to_string());
    /// assert!(url.contains_host("camera.local"));
    /// ```
    pub fn contains_host(&self, host: &str) -> bool {
        match self {
            SourceAddress::Ip(ip) => ip.contains(host),
            SourceAddress::Url(url) => url.contains(host),
            SourceAddress::None => false,
        }
    }

    /// Extract the port number from this address if present.
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
        let addr_str = match self {
            SourceAddress::Ip(ip) => ip.as_str(),
            SourceAddress::Url(url) => url.as_str(),
            SourceAddress::None => return None,
        };

        if let SourceAddress::Url(_) = self {
            // Try to parse as URL to extract port
            // Format might be http://host:port or similar
            if let Some(port_start) = addr_str.rfind(':') {
                // Make sure this isn't the :// in the scheme
                let before_colon = &addr_str[..port_start];
                if !before_colon.ends_with('/') {
                    // Try to parse what comes after the colon
                    let port_str = &addr_str[port_start + 1..];
                    // Remove any trailing path
                    let port_str = port_str.split('/').next().unwrap_or(port_str);
                    return port_str.parse::<u16>().ok();
                }
            }
        } else if let Some(colon_pos) = addr_str.rfind(':') {
            let port_str = &addr_str[colon_pos + 1..];
            return port_str.parse::<u16>().ok();
        }

        None
    }
}

/// Represents an NDI source discovered on the network.
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
#[derive(Debug, Default, Clone)]
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
    /// This method checks both the source name and address for a match,
    /// making it easy to find sources by hostname or IP.
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
        self.name.contains(host) || self.address.contains_host(host)
    }

    /// Extract the IP address from this source if available.
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
        match &self.address {
            SourceAddress::Ip(ip) => Some(ip.split(':').next().unwrap_or(ip)),
            SourceAddress::Url(url) => {
                let without_scheme = if let Some(idx) = url.find("://") {
                    &url[idx + 3..]
                } else {
                    url.as_str()
                };
                let host = without_scheme
                    .split(':')
                    .next()
                    .unwrap_or(without_scheme)
                    .split('/')
                    .next()
                    .unwrap_or(without_scheme);
                if host.is_empty() {
                    None
                } else {
                    Some(host)
                }
            }
            SourceAddress::None => None,
        }
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

    /// Safely convert from raw NDI source pointer with null checks.
    ///
    /// This performs defensive checks at the FFI boundary to prevent undefined behavior
    /// from null or invalid pointers returned by the NDI SDK.
    ///
    /// # Errors
    ///
    /// Returns `Error::NullPointer` if:
    /// - The source pointer itself is null
    /// - The `p_ndi_name` field is null
    ///
    /// # Safety
    ///
    /// The caller must ensure that if `source_ptr` is non-null, it points to a valid
    /// `NDIlib_source_t` with proper lifetime.
    pub(crate) fn try_from_raw(source_ptr: *const NDIlib_source_t) -> Result<Self> {
        // Check top-level pointer
        if source_ptr.is_null() {
            return Err(Error::NullPointer("NDIlib_source_t pointer".into()));
        }

        let ndi_source = unsafe { &*source_ptr };

        // Check p_ndi_name field
        if ndi_source.p_ndi_name.is_null() {
            return Err(Error::NullPointer("NDIlib_source_t::p_ndi_name".into()));
        }

        let name = unsafe {
            CStr::from_ptr(ndi_source.p_ndi_name)
                .to_string_lossy()
                .into_owned()
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

/// Cached NDI source with associated NDI runtime instance.
///
/// The `_ndi` field keeps the NDI runtime alive for as long as the source is cached,
/// ensuring the runtime doesn't get destroyed while sources are still in use.
#[derive(Clone)]
struct CachedSource {
    _ndi: Arc<NDI>,
    source: Source,
}

/// Thread-safe cache for NDI source discovery.
///
/// `SourceCache` eliminates the need for applications to manually cache NDI instances
/// and discovered sources. It handles expensive NDI initialization and source discovery
/// operations internally with built-in caching.
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
/// let cache = SourceCache::new()?;
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
    cache: Mutex<HashMap<String, CachedSource>>,
}

impl SourceCache {
    /// Create a new source cache.
    ///
    /// # Errors
    ///
    /// Returns an error if the NDI runtime cannot be initialized.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use grafton_ndi::SourceCache;
    ///
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let cache = SourceCache::new()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn new() -> Result<Self> {
        Ok(Self {
            cache: Mutex::new(HashMap::new()),
        })
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
    /// - [`Error::InvalidConfiguration`] if `timeout` exceeds [`crate::MAX_TIMEOUT`]
    /// - Other errors if NDI initialization or discovery fails
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use grafton_ndi::SourceCache;
    /// use std::time::Duration;
    ///
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let cache = SourceCache::new()?;
    ///
    /// // Find by IP address
    /// let source = cache.find_by_host("192.168.0.107", Duration::from_secs(5))?;
    ///
    /// // Find by partial IP
    /// let source = cache.find_by_host("192.168.0", Duration::from_secs(5))?;
    ///
    /// // Find by name
    /// let source = cache.find_by_host("CAMERA1", Duration::from_secs(5))?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn find_by_host(&self, host: &str, timeout: Duration) -> Result<Source> {
        {
            let cache = self.cache.lock().unwrap();
            if let Some(cached) = cache.get(host) {
                return Ok(cached.source.clone());
            }
        }

        let ndi = Arc::new(NDI::new()?);
        // Use extra_ips to hint NDI to look at the specific host IP/network segment
        // This significantly improves discovery speed and reliability
        let options = FinderOptions::builder()
            .show_local_sources(true)
            .extra_ips(host)
            .build();
        let finder = Finder::new(&ndi, &options)?;

        finder.wait_for_sources(timeout)?;
        let sources = finder.sources(Duration::ZERO)?;

        let source = sources
            .into_iter()
            .find(|s| s.matches_host(host))
            .ok_or_else(|| Error::NoSourcesFound {
                criteria: format!("host: {host}"),
            })?;

        {
            let mut cache = self.cache.lock().unwrap();
            cache.insert(
                host.to_string(),
                CachedSource {
                    _ndi: ndi.clone(),
                    source: source.clone(),
                },
            );
        }

        Ok(source)
    }

    /// Invalidate the cache entry for a specific host.
    ///
    /// This is useful when a source goes offline or when you want to force
    /// a fresh discovery on the next lookup.
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
    /// let cache = SourceCache::new()?;
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
        let mut cache = self.cache.lock().unwrap();
        cache.remove(host);
    }

    /// Clear all cached sources.
    ///
    /// This removes all entries from the cache, forcing fresh discovery
    /// for all subsequent lookups.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use grafton_ndi::SourceCache;
    /// use std::time::Duration;
    ///
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let cache = SourceCache::new()?;
    /// cache.find_by_host("192.168.0.107", Duration::from_secs(5))?;
    /// cache.find_by_host("192.168.0.108", Duration::from_secs(5))?;
    ///
    /// // Clear all cached sources
    /// cache.clear();
    /// # Ok(())
    /// # }
    /// ```
    pub fn clear(&self) {
        let mut cache = self.cache.lock().unwrap();
        cache.clear();
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
    /// let cache = SourceCache::new()?;
    /// assert_eq!(cache.len(), 0);
    ///
    /// cache.find_by_host("192.168.0.107", Duration::from_secs(5))?;
    /// assert_eq!(cache.len(), 1);
    /// # Ok(())
    /// # }
    /// ```
    pub fn len(&self) -> usize {
        let cache = self.cache.lock().unwrap();
        cache.len()
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
    /// let cache = SourceCache::new()?;
    /// assert!(cache.is_empty());
    ///
    /// cache.find_by_host("192.168.0.107", Duration::from_secs(5))?;
    /// assert!(!cache.is_empty());
    /// # Ok(())
    /// # }
    /// ```
    pub fn is_empty(&self) -> bool {
        let cache = self.cache.lock().unwrap();
        cache.is_empty()
    }
}

impl Default for SourceCache {
    fn default() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    #[test]
    fn test_try_from_raw_null_pointer() {
        // Test that null pointer is rejected
        let result = Source::try_from_raw(ptr::null());
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

        let result = Source::try_from_raw(&source as *const _);
        assert!(result.is_err());
        match result {
            Err(Error::NullPointer(msg)) => {
                assert!(msg.contains("p_ndi_name"));
            }
            _ => panic!("Expected NullPointer error for null name"),
        }
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

        let result = Source::try_from_raw(&source as *const _);
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

        let result = Source::try_from_raw(&source as *const _);
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

        let result = Source::try_from_raw(&source as *const _);
        assert!(result.is_ok());

        let source = result.unwrap();
        assert_eq!(source.name, "Source No Addr");
        match source.address {
            SourceAddress::None => {}
            _ => panic!("Expected None address"),
        }
    }
}
