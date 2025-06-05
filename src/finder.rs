//! NDI source discovery and network browsing.

use std::{
    ffi::{CStr, CString},
    fmt::{self, Display, Formatter},
    marker::PhantomData,
    ptr,
};

use crate::{ndi_lib::*, Error, Result, NDI};

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
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = NDI::new()?;
/// let options = FinderOptions::builder().show_local_sources(true).build();
/// let finder = Finder::new(&ndi, &options)?;
///
/// // Wait for initial discovery
/// if finder.wait_for_sources(5000) {
///     let sources = finder.get_sources(0)?;
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
    /// * `timeout` - Maximum time to wait in milliseconds (0 = no wait)
    ///
    /// # Returns
    ///
    /// `true` if the source list changed, `false` if the timeout expired.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, FinderOptions, Finder};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// // Wait up to 5 seconds for changes
    /// if finder.wait_for_sources(5000) {
    ///     println!("Source list changed!");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn wait_for_sources(&self, timeout: u32) -> bool {
        unsafe { NDIlib_find_wait_for_sources(self.instance, timeout) }
    }

    /// Gets the current list of discovered sources.
    ///
    /// # Arguments
    ///
    /// * `timeout` - Time to wait for sources in milliseconds (0 = immediate)
    ///
    /// # Returns
    ///
    /// A vector of discovered sources. May be empty if no sources are found.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use grafton_ndi::{NDI, FinderOptions, Finder};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let finder = Finder::new(&ndi, &FinderOptions::default())?;
    /// // Get sources immediately
    /// let sources = finder.get_sources(0)?;
    ///
    /// // Get sources with 1 second timeout
    /// let sources = finder.get_sources(1000)?;
    ///
    /// for source in sources {
    ///     println!("{}", source);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_sources(&self, timeout: u32) -> Result<Vec<Source>> {
        let mut num_sources = 0;
        let sources_ptr =
            unsafe { NDIlib_find_get_sources(self.instance, &mut num_sources, timeout) };
        if sources_ptr.is_null() {
            return Ok(vec![]);
        }
        let sources = unsafe {
            (0..num_sources)
                .map(|i| {
                    let source = &*sources_ptr.add(i as usize);
                    Source::from_raw(source)
                })
                .collect()
        };
        Ok(sources)
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

// This struct holds the CStrings to ensure they live as long as needed
#[repr(C)]
pub(crate) struct RawSource {
    _name: CString,
    _url_address: Option<CString>,
    _ip_address: Option<CString>,
    pub raw: NDIlib_source_t,
}

impl Source {
    pub(crate) fn from_raw(ndi_source: &NDIlib_source_t) -> Self {
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
            // Try URL address first
            if !ndi_source.__bindgen_anon_1.p_url_address.is_null() {
                let url_str = CStr::from_ptr(ndi_source.__bindgen_anon_1.p_url_address)
                    .to_string_lossy()
                    .into_owned();
                // Validate it looks like a URL (contains ://)
                if url_str.contains("://") {
                    SourceAddress::Url(url_str)
                } else {
                    // If it doesn't look like a URL, treat as IP
                    SourceAddress::Ip(url_str)
                }
            } else {
                SourceAddress::None
            }
        };

        Source { name, address }
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
            SourceAddress::Url(url) => write!(f, "{}@{}", self.name, url),
            SourceAddress::Ip(ip) => write!(f, "{}@{}", self.name, ip),
            SourceAddress::None => write!(f, "{}", self.name),
        }
    }
}
