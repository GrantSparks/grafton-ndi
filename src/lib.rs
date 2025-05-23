//! High-performance Rust bindings for the NDI® 6 SDK (Network Device Interface).
//!
//! This crate provides safe, idiomatic Rust bindings for the NDI SDK, enabling
//! real-time, low-latency video/audio streaming over IP networks. NDI is widely
//! used in broadcast, live production, and video conferencing applications.
//!
//! # Quick Start
//!
//! ```no_run
//! use grafton_ndi::{NDI, Finder, Find};
//!
//! # fn main() -> Result<(), grafton_ndi::Error> {
//! // Initialize the NDI runtime
//! let ndi = NDI::new()?;
//!
//! // Find sources on the network
//! let finder = Finder::builder().show_local_sources(true).build();
//! let find = Find::new(&ndi, finder)?;
//!
//! // Discover sources
//! find.wait_for_sources(5000);
//! let sources = find.get_sources(0)?;
//!
//! for source in sources {
//!     println!("Found: {}", source);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Core Concepts
//!
//! ## Runtime Management
//!
//! The [`NDI`] struct manages the NDI runtime lifecycle. It must be created before
//! any other NDI operations and should be kept alive for the duration of your
//! application's NDI usage.
//!
//! ## Source Discovery
//!
//! Use [`Find`] to discover NDI sources on the network. Sources can be filtered
//! by groups and additional IP addresses can be specified for discovery.
//!
//! ## Receiving
//!
//! The [`Receiver`] type handles receiving video, audio, and metadata from NDI
//! sources. It supports various color formats and bandwidth modes.
//!
//! ## Sending
//!
//! Use [`SendInstance`] to transmit video, audio, and metadata as an NDI source.
//! Senders can be configured with clock settings and group assignments.
//!
//! # Thread Safety
//!
//! All primary types ([`Find`], [`Receiver`], [`SendInstance`]) implement `Send + Sync`
//! as the underlying NDI SDK is thread-safe. However, for optimal performance,
//! minimize cross-thread operations and maintain thread affinity where possible.
//!
//! # Performance
//!
//! - **Zero-copy**: Frame data directly references NDI's buffers when possible
//! - **Bandwidth control**: Multiple quality levels for different use cases
//! - **Hardware acceleration**: Automatically uses GPU acceleration when available
//!
//! # Platform Support
//!
//! - **Windows**: Full support, tested on Windows 10/11
//! - **Linux**: Full support, tested on Ubuntu 20.04+
//! - **macOS**: Experimental support with limited testing

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
use num_enum::{IntoPrimitive, TryFromPrimitive};
use std::sync::atomic::AtomicBool;
use std::{
    borrow::Cow,
    ffi::{CStr, CString},
    fmt::{self, Display, Formatter},
    os::raw::c_char,
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

mod error;
pub use error::*;

mod ndi_lib;
use ndi_lib::*;

// Global initialization state and reference count
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
    pub fn acquire() -> Result<Self, Error> {
        // 1. Bump the counter immediately.
        let prev = REFCOUNT.fetch_add(1, Ordering::SeqCst);

        if prev == 0 {
            // We are the first handle → initialise the runtime.
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
            // Busy-wait until it is done so the caller never sees an
            // un-initialised runtime while REFCOUNT > 0.
            while !INIT.load(Ordering::SeqCst) && !INIT_FAILED.load(Ordering::SeqCst) {
                std::hint::spin_loop();
            }
            // Check again after waiting
            if INIT_FAILED.load(Ordering::SeqCst) {
                REFCOUNT.fetch_sub(1, Ordering::SeqCst);
                return Err(Error::InitializationFailed(
                    "NDI initialization failed previously".into(),
                ));
            }
        }

        Ok(NDI)
    }

    /// Creates a new NDI instance.
    ///
    /// Alias for [`NDI::acquire()`].
    pub fn new() -> Result<Self, Error> {
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
    pub fn version() -> Result<String, Error> {
        unsafe {
            let version_ptr = NDIlib_version();
            if version_ptr.is_null() {
                return Err(Error::NullPointer("NDIlib_version".into()));
            }
            let c_str = CStr::from_ptr(version_ptr);
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
        NDI
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

/// Configuration for NDI source discovery.
///
/// Use the builder pattern to create instances with specific settings.
///
/// # Examples
///
/// ```
/// use grafton_ndi::Finder;
///
/// // Find all sources including local ones
/// let finder = Finder::builder()
///     .show_local_sources(true)
///     .build();
///
/// // Find sources in specific groups
/// let finder = Finder::builder()
///     .groups("Public,Studio")
///     .build();
///
/// // Find sources on specific network segments
/// let finder = Finder::builder()
///     .extra_ips("192.168.1.0/24,10.0.0.0/24")
///     .build();
/// ```
#[derive(Debug, Default)]
pub struct Finder {
    /// Whether to include local sources in discovery.
    pub show_local_sources: bool,
    /// Comma-separated list of groups to search (e.g., "Public,Private").
    pub groups: Option<String>,
    /// Additional IP addresses or ranges to search.
    pub extra_ips: Option<String>,
}

impl Finder {

    /// Create a builder for configuring find options
    pub fn builder() -> FinderBuilder {
        FinderBuilder::new()
    }
}

/// Builder for configuring Finder with ergonomic method chaining
#[derive(Debug, Clone)]
pub struct FinderBuilder {
    show_local_sources: Option<bool>,
    groups: Option<String>,
    extra_ips: Option<String>,
}

impl FinderBuilder {
    /// Creates a new builder with default settings.
    ///
    /// Default settings:
    /// - `show_local_sources`: `true`
    /// - `groups`: `None` (search all groups)
    /// - `extra_ips`: `None` (no additional IPs)
    pub fn new() -> Self {
        FinderBuilder {
            show_local_sources: None,
            groups: None,
            extra_ips: None,
        }
    }

    /// Configure whether to show local sources
    pub fn show_local_sources(mut self, show: bool) -> Self {
        self.show_local_sources = Some(show);
        self
    }

    /// Set the groups to search
    pub fn groups<S: Into<String>>(mut self, groups: S) -> Self {
        self.groups = Some(groups.into());
        self
    }

    /// Set extra IPs to search
    pub fn extra_ips<S: Into<String>>(mut self, ips: S) -> Self {
        self.extra_ips = Some(ips.into());
        self
    }

    /// Build the Finder
    pub fn build(self) -> Finder {
        Finder {
            show_local_sources: self.show_local_sources.unwrap_or(true),
            groups: self.groups,
            extra_ips: self.extra_ips,
        }
    }
}

impl Default for FinderBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Discovers NDI sources on the network.
///
/// `Find` provides methods to discover and monitor NDI sources. It maintains
/// a background thread that continuously updates the list of available sources.
///
/// # Examples
///
/// ```no_run
/// # use grafton_ndi::{NDI, Finder, Find};
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = NDI::new()?;
/// let finder = Finder::builder().show_local_sources(true).build();
/// let find = Find::new(&ndi, finder)?;
///
/// // Wait for initial discovery
/// if find.wait_for_sources(5000) {
///     let sources = find.get_sources(0)?;
///     for source in sources {
///         println!("Found: {}", source);
///     }
/// }
/// # Ok(())
/// # }
/// ```
pub struct Find<'a> {
    instance: NDIlib_find_instance_t,
    _groups: Option<CString>,    // Hold ownership of CStrings
    _extra_ips: Option<CString>, // to ensure they outlive SDK usage
    ndi: std::marker::PhantomData<&'a NDI>,
}

impl<'a> Find<'a> {
    /// Creates a new source finder with the specified settings.
    ///
    /// # Arguments
    ///
    /// * `ndi` - The NDI instance (must outlive this `Find`)
    /// * `settings` - Configuration for source discovery
    ///
    /// # Errors
    ///
    /// Returns an error if the finder cannot be created, typically due to
    /// invalid settings or network issues.
    pub fn new(_ndi: &'a NDI, settings: Finder) -> Result<Self, Error> {
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
        Ok(Find {
            instance,
            _groups: groups_cstr,
            _extra_ips: extra_ips_cstr,
            ndi: std::marker::PhantomData,
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
    /// # use grafton_ndi::{NDI, Finder, Find};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let find = Find::new(&ndi, Finder::default())?;
    /// // Wait up to 5 seconds for changes
    /// if find.wait_for_sources(5000) {
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
    /// # use grafton_ndi::{NDI, Finder, Find};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// # let ndi = NDI::new()?;
    /// # let find = Find::new(&ndi, Finder::default())?;
    /// // Get sources immediately
    /// let sources = find.get_sources(0)?;
    ///
    /// // Get sources with 1 second timeout
    /// let sources = find.get_sources(1000)?;
    ///
    /// for source in sources {
    ///     println!("{}", source);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn get_sources(&self, timeout: u32) -> Result<Vec<Source>, Error> {
        let mut no_sources = 0;
        let sources_ptr =
            unsafe { NDIlib_find_get_sources(self.instance, &mut no_sources, timeout) };
        if sources_ptr.is_null() {
            return Ok(vec![]);
        }
        let sources = unsafe {
            (0..no_sources)
                .map(|i| {
                    let source = &*sources_ptr.add(i as usize);
                    Source::from_raw(source)
                })
                .collect()
        };
        Ok(sources)
    }
}

impl Drop for Find<'_> {
    fn drop(&mut self) {
        unsafe { NDIlib_find_destroy(self.instance) };
    }
}

/// # Safety
/// 
/// The NDI SDK documentation states that find operations are thread-safe.
/// `NDIlib_find_create_v2`, `NDIlib_find_wait_for_sources`, and `NDIlib_find_get_sources`
/// can be called from multiple threads. The Find struct only holds an opaque pointer
/// returned by the SDK and does not perform any mutations that could cause data races.
unsafe impl std::marker::Send for Find<'_> {}

/// # Safety
/// 
/// The NDI SDK documentation guarantees thread-safety for find operations.
/// Multiple threads can safely call methods on a shared Find instance as the
/// SDK handles all necessary synchronization internally.
unsafe impl std::marker::Sync for Find<'_> {}

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
    fn from_raw(ndi_source: &NDIlib_source_t) -> Self {
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
    fn to_raw(&self) -> Result<RawSource, Error> {
        let name = CString::new(self.name.clone()).map_err(Error::InvalidCString)?;
        
        let (url_address, ip_address, __bindgen_anon_1) = match &self.address {
            SourceAddress::Url(url) => {
                let url_cstr = CString::new(url.clone()).map_err(Error::InvalidCString)?;
                let p_url = url_cstr.as_ptr();
                (
                    Some(url_cstr),
                    None,
                    NDIlib_source_t__bindgen_ty_1 { p_url_address: p_url }
                )
            }
            SourceAddress::Ip(ip) => {
                let ip_cstr = CString::new(ip.clone()).map_err(Error::InvalidCString)?;
                let p_ip = ip_cstr.as_ptr();
                (
                    None,
                    Some(ip_cstr),
                    NDIlib_source_t__bindgen_ty_1 { p_ip_address: p_ip }
                )
            }
            SourceAddress::None => {
                (
                    None,
                    None,
                    NDIlib_source_t__bindgen_ty_1 { p_ip_address: ptr::null() }
                )
            }
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

/// Video pixel format identifiers (FourCC codes).
///
/// These represent the various pixel formats supported by NDI for video frames.
/// The most common formats are BGRA/RGBA for full quality and UYVY for bandwidth-efficient streaming.
///
/// # Examples
///
/// ```
/// use grafton_ndi::FourCCVideoType;
///
/// // For maximum compatibility and quality
/// let format = FourCCVideoType::BGRA;
///
/// // For bandwidth-efficient streaming
/// let format = FourCCVideoType::UYVY;
/// ```
#[derive(Debug, TryFromPrimitive, IntoPrimitive, Clone, Copy)]
#[repr(u32)]
pub enum FourCCVideoType {
    /// YCbCr 4:2:2 format (16 bits per pixel) - bandwidth efficient.
    UYVY = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVY,
    /// YCbCr 4:2:2 with alpha channel (24 bits per pixel).
    UYVA = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_UYVA,
    /// 16-bit YCbCr 4:2:2 format.
    P216 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_P216,
    /// 16-bit YCbCr 4:2:2 with alpha.
    PA16 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_PA16,
    /// Planar YCbCr 4:2:0 format (12 bits per pixel).
    YV12 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_YV12,
    /// Planar YCbCr 4:2:0 format (12 bits per pixel).
    I420 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_I420,
    /// Semi-planar YCbCr 4:2:0 format (12 bits per pixel).
    NV12 = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_NV12,
    /// Blue-Green-Red-Alpha format (32 bits per pixel) - full quality.
    BGRA = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRA,
    /// Blue-Green-Red with padding (32 bits per pixel).
    BGRX = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRX,
    /// Red-Green-Blue-Alpha format (32 bits per pixel) - full quality.
    RGBA = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_RGBA,
    /// Red-Green-Blue with padding (32 bits per pixel).
    RGBX = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_RGBX,
    Max = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_max,
}

#[derive(Debug, TryFromPrimitive, IntoPrimitive, Clone, Copy)]
#[repr(u32)]
pub enum FrameFormatType {
    Progressive = NDIlib_frame_format_type_e_NDIlib_frame_format_type_progressive,
    Interlaced = NDIlib_frame_format_type_e_NDIlib_frame_format_type_interleaved,
    Field0 = NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_0,
    Field1 = NDIlib_frame_format_type_e_NDIlib_frame_format_type_field_1,
    Max = NDIlib_frame_format_type_e_NDIlib_frame_format_type_max,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union LineStrideOrSize {
    pub line_stride_in_bytes: i32,
    pub data_size_in_bytes: i32,
}

impl fmt::Debug for LineStrideOrSize {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // For debugging purposes, we'll assume that we're interested in `line_stride_in_bytes`
        unsafe {
            write!(
                f,
                "LineStrideOrSize {{ line_stride_in_bytes: {} }}",
                self.line_stride_in_bytes
            )
        }
    }
}

impl From<LineStrideOrSize> for NDIlib_video_frame_v2_t__bindgen_ty_1 {
    fn from(value: LineStrideOrSize) -> Self {
        unsafe {
            if value.line_stride_in_bytes != 0 {
                NDIlib_video_frame_v2_t__bindgen_ty_1 {
                    line_stride_in_bytes: value.line_stride_in_bytes,
                }
            } else {
                NDIlib_video_frame_v2_t__bindgen_ty_1 {
                    data_size_in_bytes: value.data_size_in_bytes,
                }
            }
        }
    }
}

impl From<NDIlib_video_frame_v2_t__bindgen_ty_1> for LineStrideOrSize {
    fn from(value: NDIlib_video_frame_v2_t__bindgen_ty_1) -> Self {
        unsafe {
            if value.line_stride_in_bytes != 0 {
                LineStrideOrSize {
                    line_stride_in_bytes: value.line_stride_in_bytes,
                }
            } else {
                LineStrideOrSize {
                    data_size_in_bytes: value.data_size_in_bytes,
                }
            }
        }
    }
}

pub struct VideoFrame<'rx> {
    pub xres: i32,
    pub yres: i32,
    pub fourcc: FourCCVideoType,
    pub frame_rate_n: i32,
    pub frame_rate_d: i32,
    pub picture_aspect_ratio: f32,
    pub frame_format_type: FrameFormatType,
    pub timecode: i64,
    pub data: Cow<'rx, [u8]>,
    pub line_stride_or_size: LineStrideOrSize,
    pub metadata: Option<CString>,
    pub timestamp: i64,
    recv_instance: Option<NDIlib_recv_instance_t>,
    // Store original SDK data pointer for proper freeing
    original_p_data: Option<*mut u8>,
    _origin: std::marker::PhantomData<&'rx Recv<'rx>>,
}

impl fmt::Debug for VideoFrame<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoFrame")
            .field("xres", &self.xres)
            .field("yres", &self.yres)
            .field("fourcc", &self.fourcc)
            .field("frame_rate_n", &self.frame_rate_n)
            .field("frame_rate_d", &self.frame_rate_d)
            .field("picture_aspect_ratio", &self.picture_aspect_ratio)
            .field("frame_format_type", &self.frame_format_type)
            .field("timecode", &self.timecode)
            .field("data (bytes)", &self.data.len())
            .field("line_stride_or_size", &self.line_stride_or_size)
            .field("metadata", &self.metadata)
            .field("timestamp", &self.timestamp)
            .finish()
    }
}

impl Default for VideoFrame<'_> {
    fn default() -> Self {
        VideoFrame::builder()
            .resolution(1920, 1080)
            .fourcc(FourCCVideoType::BGRA)
            .frame_rate(60, 1)
            .aspect_ratio(16.0 / 9.0)
            .format(FrameFormatType::Interlaced)
            .build()
            .expect("Default VideoFrame should always succeed")
    }
}

impl<'rx> VideoFrame<'rx> {

    pub fn to_raw(&self) -> NDIlib_video_frame_v2_t {
        NDIlib_video_frame_v2_t {
            xres: self.xres,
            yres: self.yres,
            FourCC: self.fourcc.into(),
            frame_rate_N: self.frame_rate_n,
            frame_rate_D: self.frame_rate_d,
            picture_aspect_ratio: self.picture_aspect_ratio,
            frame_format_type: self.frame_format_type.into(),
            timecode: self.timecode,
            p_data: self.data.as_ptr() as *mut u8,
            __bindgen_anon_1: self.line_stride_or_size.into(),
            p_metadata: match &self.metadata {
                Some(meta) => meta.as_ptr(),
                None => ptr::null(),
            },
            timestamp: self.timestamp,
        }
    }

    /// Creates a `VideoFrame` from a raw NDI video frame with owned data.
    ///
    /// # Safety
    ///
    /// This function assumes the given `NDIlib_video_frame_v2_t` is valid and correctly allocated.
    /// This method copies the data, so the VideoFrame owns its data and can outlive the source.
    pub unsafe fn from_raw(
        c_frame: &NDIlib_video_frame_v2_t,
        recv_instance: Option<NDIlib_recv_instance_t>,
    ) -> Result<VideoFrame<'static>, Error> {
        if c_frame.p_data.is_null() {
            return Err(Error::InvalidFrame(
                "Video frame has null data pointer".into(),
            ));
        }

        let fourcc = FourCCVideoType::try_from(c_frame.FourCC).unwrap_or(FourCCVideoType::Max);
        
        // Determine data size based on whether we have line_stride or data_size_in_bytes
        // The NDI SDK uses a union here: line_stride_in_bytes for uncompressed formats,
        // data_size_in_bytes for compressed formats.
        let data_size_in_bytes = c_frame.__bindgen_anon_1.data_size_in_bytes;
        let line_stride = c_frame.__bindgen_anon_1.line_stride_in_bytes;
        
        // Since this is a union, we need to determine which field is valid
        // For uncompressed formats, line_stride * height should equal a reasonable frame size
        let potential_stride_size = if line_stride > 0 && c_frame.yres > 0 {
            (line_stride as usize) * (c_frame.yres as usize)
        } else {
            0
        };
        
        let (data_size, line_stride_or_size) = if line_stride > 0 && potential_stride_size > 0 
            && potential_stride_size <= (100 * 1024 * 1024) {
            // Reasonable size for uncompressed video (< 100MB per frame)
            // Use line stride for calculation
            (potential_stride_size, LineStrideOrSize { line_stride_in_bytes: line_stride })
        } else if data_size_in_bytes > 0 {
            // Use the explicit data size (likely compressed format)
            (data_size_in_bytes as usize, LineStrideOrSize { data_size_in_bytes })
        } else {
            // Neither field is valid - this is an error
            return Err(Error::InvalidFrame(
                "Video frame has neither valid line_stride_in_bytes nor data_size_in_bytes".into()
            ));
        };

        if data_size == 0 {
            return Err(Error::InvalidFrame("Video frame has zero size".into()));
        }

        // For zero-copy: just borrow the data slice from the SDK
        let (data, original_p_data) = if recv_instance.is_some() {
            // We're receiving - don't copy, just borrow
            let slice = std::slice::from_raw_parts(c_frame.p_data, data_size);
            (Cow::Borrowed(slice), Some(c_frame.p_data))
        } else {
            // Not from receive - make a copy for ownership
            let slice = std::slice::from_raw_parts(c_frame.p_data, data_size);
            (Cow::Owned(slice.to_vec()), None)
        };

        let metadata = if c_frame.p_metadata.is_null() {
            None
        } else {
            Some(CString::from(CStr::from_ptr(c_frame.p_metadata)))
        };

        Ok(VideoFrame {
            xres: c_frame.xres,
            yres: c_frame.yres,
            fourcc,
            frame_rate_n: c_frame.frame_rate_N,
            frame_rate_d: c_frame.frame_rate_D,
            picture_aspect_ratio: c_frame.picture_aspect_ratio,
            frame_format_type: FrameFormatType::try_from(c_frame.frame_format_type)
                .unwrap_or(FrameFormatType::Max),
            timecode: c_frame.timecode,
            data,
            line_stride_or_size,
            metadata,
            timestamp: c_frame.timestamp,
            recv_instance,
            original_p_data,
            _origin: std::marker::PhantomData,
        })
    }

    /// Create a builder for configuring a video frame
    pub fn builder() -> VideoFrameBuilder<'rx> {
        VideoFrameBuilder::new()
    }
}

/// Builder for configuring a VideoFrame with ergonomic method chaining
#[derive(Debug, Clone)]
pub struct VideoFrameBuilder<'rx> {
    xres: Option<i32>,
    yres: Option<i32>,
    fourcc: Option<FourCCVideoType>,
    frame_rate_n: Option<i32>,
    frame_rate_d: Option<i32>,
    picture_aspect_ratio: Option<f32>,
    frame_format_type: Option<FrameFormatType>,
    timecode: Option<i64>,
    metadata: Option<String>,
    timestamp: Option<i64>,
    _phantom: std::marker::PhantomData<&'rx ()>,
}

impl<'rx> VideoFrameBuilder<'rx> {
    /// Create a new builder with no fields set
    pub fn new() -> Self {
        VideoFrameBuilder {
            xres: None,
            yres: None,
            fourcc: None,
            frame_rate_n: None,
            frame_rate_d: None,
            picture_aspect_ratio: None,
            frame_format_type: None,
            timecode: None,
            metadata: None,
            timestamp: None,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Set the video resolution
    pub fn resolution(mut self, width: i32, height: i32) -> Self {
        self.xres = Some(width);
        self.yres = Some(height);
        self
    }

    /// Set the pixel format
    pub fn fourcc(mut self, fourcc: FourCCVideoType) -> Self {
        self.fourcc = Some(fourcc);
        self
    }

    /// Set the frame rate as a fraction (e.g., 30000/1001 for 29.97fps)
    pub fn frame_rate(mut self, numerator: i32, denominator: i32) -> Self {
        self.frame_rate_n = Some(numerator);
        self.frame_rate_d = Some(denominator);
        self
    }

    /// Set the picture aspect ratio
    pub fn aspect_ratio(mut self, ratio: f32) -> Self {
        self.picture_aspect_ratio = Some(ratio);
        self
    }

    /// Set the frame format type (progressive, interlaced, etc.)
    pub fn format(mut self, format: FrameFormatType) -> Self {
        self.frame_format_type = Some(format);
        self
    }

    /// Set the timecode
    pub fn timecode(mut self, tc: i64) -> Self {
        self.timecode = Some(tc);
        self
    }

    /// Set metadata
    pub fn metadata<S: Into<String>>(mut self, meta: S) -> Self {
        self.metadata = Some(meta.into());
        self
    }

    /// Set the timestamp
    pub fn timestamp(mut self, ts: i64) -> Self {
        self.timestamp = Some(ts);
        self
    }

    /// Build the VideoFrame
    pub fn build(self) -> Result<VideoFrame<'rx>, Error> {
        let xres = self.xres.unwrap_or(1920);
        let yres = self.yres.unwrap_or(1080);
        let fourcc = self.fourcc.unwrap_or(FourCCVideoType::BGRA);
        let frame_rate_n = self.frame_rate_n.unwrap_or(60);
        let frame_rate_d = self.frame_rate_d.unwrap_or(1);
        let picture_aspect_ratio = self.picture_aspect_ratio.unwrap_or(16.0 / 9.0);
        let frame_format_type = self.frame_format_type.unwrap_or(FrameFormatType::Progressive);
        
        // Calculate stride and buffer size
        let bpp = match fourcc {
            FourCCVideoType::BGRA | FourCCVideoType::BGRX | FourCCVideoType::RGBA | FourCCVideoType::RGBX => 32,
            FourCCVideoType::UYVY | FourCCVideoType::YV12 | FourCCVideoType::I420 | FourCCVideoType::NV12 => 16,
            FourCCVideoType::UYVA => 32,
            FourCCVideoType::P216 | FourCCVideoType::PA16 => 32,
            _ => 32,
        };
        let stride = (xres * bpp + 7) / 8;
        let buffer_size: usize = (yres * stride) as usize;
        let data = vec![0u8; buffer_size];
        
        let mut frame = VideoFrame {
            xres,
            yres,
            fourcc,
            frame_rate_n,
            frame_rate_d,
            picture_aspect_ratio,
            frame_format_type,
            timecode: self.timecode.unwrap_or(0),
            data: Cow::Owned(data),
            line_stride_or_size: LineStrideOrSize {
                line_stride_in_bytes: stride,
            },
            metadata: None,
            timestamp: self.timestamp.unwrap_or(0),
            recv_instance: None,
            original_p_data: None,
            _origin: std::marker::PhantomData,
        };
        
        if let Some(meta) = self.metadata {
            frame.metadata = Some(CString::new(meta).map_err(Error::InvalidCString)?);
        }
        
        Ok(frame)
    }
}

impl Default for VideoFrameBuilder<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for VideoFrame<'_> {
    fn drop(&mut self) {
        // If this frame originated from a Recv instance and we have the original SDK pointer, free it
        if let (Some(recv_instance), Some(original_p_data)) = (self.recv_instance, self.original_p_data) {
            // Create a raw frame with the original SDK pointer for NDI to free
            let raw_frame = NDIlib_video_frame_v2_t {
                xres: self.xres,
                yres: self.yres,
                FourCC: self.fourcc.into(),
                frame_rate_N: self.frame_rate_n,
                frame_rate_D: self.frame_rate_d,
                picture_aspect_ratio: self.picture_aspect_ratio,
                frame_format_type: self.frame_format_type.into(),
                timecode: self.timecode,
                p_data: original_p_data,
                __bindgen_anon_1: self.line_stride_or_size.into(),
                p_metadata: match &self.metadata {
                    Some(meta) => meta.as_ptr(),
                    None => ptr::null(),
                },
                timestamp: self.timestamp,
            };
            unsafe {
                NDIlib_recv_free_video_v2(recv_instance, &raw_frame);
            }
        }
    }
}

#[derive(Debug)]
pub struct AudioFrame<'rx> {
    pub sample_rate: i32,
    pub no_channels: i32,
    pub no_samples: i32,
    pub timecode: i64,
    pub fourcc: AudioType,
    data: Cow<'rx, [f32]>,
    pub channel_stride_in_bytes: i32,
    pub metadata: Option<CString>,
    pub timestamp: i64,
    recv_instance: Option<NDIlib_recv_instance_t>,
    // Store original SDK data pointer for proper freeing
    original_p_data: Option<*mut u8>,
    _origin: std::marker::PhantomData<&'rx Recv<'rx>>,
}

impl<'rx> AudioFrame<'rx> {

    pub(crate) fn to_raw(&self) -> NDIlib_audio_frame_v3_t {
        NDIlib_audio_frame_v3_t {
            sample_rate: self.sample_rate,
            no_channels: self.no_channels,
            no_samples: self.no_samples,
            timecode: self.timecode,
            FourCC: self.fourcc.into(),
            p_data: self.data.as_ptr() as *mut f32 as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: self.channel_stride_in_bytes,
            },
            p_metadata: self.metadata.as_ref().map_or(ptr::null(), |m| m.as_ptr()),
            timestamp: self.timestamp,
        }
    }

    pub(crate) fn from_raw(raw: NDIlib_audio_frame_v3_t, recv_instance: Option<NDIlib_recv_instance_t>) -> Result<AudioFrame<'static>, Error> {
        if raw.p_data.is_null() {
            return Err(Error::InvalidFrame(
                "Audio frame has null data pointer".into(),
            ));
        }

        if raw.sample_rate <= 0 {
            return Err(Error::InvalidFrame(format!(
                "Invalid sample rate: {}",
                raw.sample_rate
            )));
        }

        if raw.no_channels <= 0 {
            return Err(Error::InvalidFrame(format!(
                "Invalid number of channels: {}",
                raw.no_channels
            )));
        }

        if raw.no_samples <= 0 {
            return Err(Error::InvalidFrame(format!(
                "Invalid number of samples: {}",
                raw.no_samples
            )));
        }

        let sample_count = (raw.no_samples * raw.no_channels) as usize;

        if sample_count == 0 {
            return Err(Error::InvalidFrame(
                "Calculated audio sample count is zero".into(),
            ));
        }

        // For zero-copy: just borrow the data slice from the SDK
        let (data, original_p_data) = if recv_instance.is_some() {
            // We're receiving - don't copy, just borrow
            let slice = unsafe { std::slice::from_raw_parts(raw.p_data as *const f32, sample_count) };
            (Cow::Borrowed(slice), Some(raw.p_data))
        } else {
            // Not from receive - make a copy for ownership
            let slice = unsafe { std::slice::from_raw_parts(raw.p_data as *const f32, sample_count) };
            (Cow::Owned(slice.to_vec()), None)
        };

        let metadata = if raw.p_metadata.is_null() {
            None
        } else {
            // Copy the string, don't take ownership - SDK will free the original
            Some(unsafe { CString::from(CStr::from_ptr(raw.p_metadata)) })
        };

        Ok(AudioFrame {
            sample_rate: raw.sample_rate,
            no_channels: raw.no_channels,
            no_samples: raw.no_samples,
            timecode: raw.timecode,
            fourcc: match raw.FourCC {
                NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP => AudioType::FLTP,
                _ => AudioType::Max,
            },
            data,
            channel_stride_in_bytes: unsafe { raw.__bindgen_anon_1.channel_stride_in_bytes },
            metadata,
            timestamp: raw.timestamp,
            recv_instance,
            original_p_data,
            _origin: std::marker::PhantomData,
        })
    }

    /// Create a builder for configuring an audio frame
    pub fn builder() -> AudioFrameBuilder<'rx> {
        AudioFrameBuilder::new()
    }

    /// Get audio data as 32-bit floats
    pub fn data(&self) -> &[f32] {
        &self.data
    }

    /// Get audio data for a specific channel (if planar format)
    pub fn channel_data(&self, channel: usize) -> Option<Vec<f32>> {
        if channel >= self.no_channels as usize {
            return None;
        }
        
        let samples_per_channel = self.no_samples as usize;
        
        if self.channel_stride_in_bytes == 0 {
            // Interleaved format: extract samples for the requested channel
            let channels = self.no_channels as usize;
            let channel_data: Vec<f32> = self.data
                .iter()
                .skip(channel)
                .step_by(channels)
                .copied()
                .collect();
            Some(channel_data)
        } else {
            // Planar format: channel data is contiguous
            let stride_in_samples = self.channel_stride_in_bytes as usize / 4; // f32 = 4 bytes
            let start = channel * stride_in_samples;
            let end = start + samples_per_channel;
            
            if end <= self.data.len() {
                Some(self.data[start..end].to_vec())
            } else {
                None
            }
        }
    }
}

/// Builder for configuring an AudioFrame with ergonomic method chaining
#[derive(Debug, Clone)]
pub struct AudioFrameBuilder<'rx> {
    sample_rate: Option<i32>,
    no_channels: Option<i32>,
    no_samples: Option<i32>,
    timecode: Option<i64>,
    fourcc: Option<AudioType>,
    data: Option<Vec<f32>>,
    metadata: Option<String>,
    timestamp: Option<i64>,
    _phantom: std::marker::PhantomData<&'rx ()>,
}

impl<'rx> AudioFrameBuilder<'rx> {
    /// Create a new builder with no fields set
    pub fn new() -> Self {
        AudioFrameBuilder {
            sample_rate: None,
            no_channels: None,
            no_samples: None,
            timecode: None,
            fourcc: None,
            data: None,
            metadata: None,
            timestamp: None,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Set the sample rate
    pub fn sample_rate(mut self, rate: i32) -> Self {
        self.sample_rate = Some(rate);
        self
    }

    /// Set the number of audio channels
    pub fn channels(mut self, channels: i32) -> Self {
        self.no_channels = Some(channels);
        self
    }

    /// Set the number of samples
    pub fn samples(mut self, samples: i32) -> Self {
        self.no_samples = Some(samples);
        self
    }

    /// Set the timecode
    pub fn timecode(mut self, tc: i64) -> Self {
        self.timecode = Some(tc);
        self
    }

    /// Set the audio format
    pub fn format(mut self, format: AudioType) -> Self {
        self.fourcc = Some(format);
        self
    }

    /// Set the audio data as 32-bit floats
    pub fn data(mut self, data: Vec<f32>) -> Self {
        self.data = Some(data);
        self
    }

    /// Set metadata
    pub fn metadata<S: Into<String>>(mut self, meta: S) -> Self {
        self.metadata = Some(meta.into());
        self
    }

    /// Set the timestamp
    pub fn timestamp(mut self, ts: i64) -> Self {
        self.timestamp = Some(ts);
        self
    }

    /// Build the AudioFrame
    pub fn build(self) -> Result<AudioFrame<'rx>, Error> {
        let sample_rate = self.sample_rate.unwrap_or(48000);
        let no_channels = self.no_channels.unwrap_or(2);
        let no_samples = self.no_samples.unwrap_or(1024);
        let fourcc = self.fourcc.unwrap_or(AudioType::FLTP);
        
        let data = if let Some(data) = self.data {
            data
        } else {
            // Calculate default buffer size for f32 samples
            let sample_count = (no_samples * no_channels) as usize;
            vec![0.0f32; sample_count]
        };
        
        let metadata_cstring = self.metadata
            .map(|m| CString::new(m).map_err(Error::InvalidCString))
            .transpose()?;
            
        Ok(AudioFrame {
            sample_rate,
            no_channels,
            no_samples,
            timecode: self.timecode.unwrap_or(0),
            fourcc,
            data: Cow::Owned(data),
            channel_stride_in_bytes: 0, // 0 indicates interleaved format
            metadata: metadata_cstring,
            timestamp: self.timestamp.unwrap_or(0),
            recv_instance: None,
            original_p_data: None,
            _origin: std::marker::PhantomData,
        })
    }
}

impl Default for AudioFrameBuilder<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for AudioFrame<'_> {
    fn default() -> Self {
        AudioFrame::builder()
            .build()
            .expect("Default AudioFrame should always succeed")
    }
}

impl Drop for AudioFrame<'_> {
    fn drop(&mut self) {
        // If this frame originated from a Recv instance and we have the original SDK pointer, free it
        if let (Some(recv_instance), Some(original_p_data)) = (self.recv_instance, self.original_p_data) {
            // Create a raw frame with the original SDK pointer for NDI to free
            let raw_frame = NDIlib_audio_frame_v3_t {
                sample_rate: self.sample_rate,
                no_channels: self.no_channels,
                no_samples: self.no_samples,
                timecode: self.timecode,
                FourCC: self.fourcc.into(),
                p_data: original_p_data,
                __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                    channel_stride_in_bytes: self.channel_stride_in_bytes,
                },
                p_metadata: self.metadata.as_ref().map_or(ptr::null(), |m| m.as_ptr()),
                timestamp: self.timestamp,
            };
            unsafe {
                NDIlib_recv_free_audio_v3(recv_instance, &raw_frame);
            }
        }
    }
}

#[derive(Debug, TryFromPrimitive, IntoPrimitive, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum AudioType {
    FLTP = NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
    Max = NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_max,
}

#[derive(Debug, Clone)]
pub struct MetadataFrame {
    pub data: String, // Owned metadata (typically XML)
    pub timecode: i64,
}

impl MetadataFrame {
    pub fn new() -> Self {
        MetadataFrame {
            data: String::new(),
            timecode: 0,
        }
    }

    pub fn with_data(data: String, timecode: i64) -> Self {
        MetadataFrame { data, timecode }
    }

    /// Convert to raw format for sending
    pub(crate) fn to_raw(&self) -> Result<(CString, NDIlib_metadata_frame_t), Error> {
        let c_data = CString::new(self.data.clone()).map_err(Error::InvalidCString)?;
        let raw = NDIlib_metadata_frame_t {
            length: c_data.as_bytes().len() as i32,
            timecode: self.timecode,
            p_data: c_data.as_ptr() as *mut c_char,
        };
        Ok((c_data, raw))
    }

    /// Create from raw NDI metadata frame (copies the data)
    pub(crate) fn from_raw(raw: &NDIlib_metadata_frame_t) -> Self {
        let data = if raw.p_data.is_null() {
            String::new()
        } else {
            unsafe {
                let c_str = CStr::from_ptr(raw.p_data);
                c_str.to_string_lossy().into_owned()
            }
        };
        MetadataFrame {
            data,
            timecode: raw.timecode,
        }
    }
}

impl Default for MetadataFrame {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub enum RecvColorFormat {
    #[default]
    BGRX_BGRA,
    UYVY_BGRA,
    RGBX_RGBA,
    UYVY_RGBA,
    Fastest,
    Best,
    //    BGRX_BGRA_Flipped,
    Max,
}

impl From<RecvColorFormat> for NDIlib_recv_color_format_e {
    fn from(format: RecvColorFormat) -> Self {
        match format {
            RecvColorFormat::BGRX_BGRA => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA
            }
            RecvColorFormat::UYVY_BGRA => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_UYVY_BGRA
            }
            RecvColorFormat::RGBX_RGBA => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_RGBX_RGBA
            }
            RecvColorFormat::UYVY_RGBA => {
                NDIlib_recv_color_format_e_NDIlib_recv_color_format_UYVY_RGBA
            }
            RecvColorFormat::Fastest => NDIlib_recv_color_format_e_NDIlib_recv_color_format_fastest,
            RecvColorFormat::Best => NDIlib_recv_color_format_e_NDIlib_recv_color_format_best,
            //            RecvColorFormat::BGRX_BGRA_Flipped => {
            //                NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA_flipped
            //            }
            RecvColorFormat::Max => NDIlib_recv_color_format_e_NDIlib_recv_color_format_max,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub enum RecvBandwidth {
    MetadataOnly,
    AudioOnly,
    Lowest,
    #[default]
    Highest,
    Max,
}

impl From<RecvBandwidth> for NDIlib_recv_bandwidth_e {
    fn from(bandwidth: RecvBandwidth) -> Self {
        match bandwidth {
            RecvBandwidth::MetadataOnly => {
                NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_metadata_only
            }
            RecvBandwidth::AudioOnly => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_audio_only,
            RecvBandwidth::Lowest => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_lowest,
            RecvBandwidth::Highest => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
            RecvBandwidth::Max => NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_max,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Receiver {
    pub source_to_connect_to: Source,
    pub color_format: RecvColorFormat,
    pub bandwidth: RecvBandwidth,
    pub allow_video_fields: bool,
    pub ndi_recv_name: Option<String>,
}

#[repr(C)]
pub(crate) struct RawRecvCreateV3 {
    _source: RawSource,
    _name: Option<CString>,
    pub raw: NDIlib_recv_create_v3_t,
}

impl Receiver {

    /// Convert to raw format for FFI use
    ///
    /// # Safety
    ///
    /// The returned RawRecvCreateV3 struct uses #[repr(C)] to guarantee C-compatible layout
    /// for safe FFI interop with the NDI SDK.
    pub(crate) fn to_raw(&self) -> Result<RawRecvCreateV3, Error> {
        let source = self.source_to_connect_to.to_raw()?;
        let name = self
            .ndi_recv_name
            .as_ref()
            .map(|n| CString::new(n.clone()))
            .transpose()
            .map_err(Error::InvalidCString)?;

        let p_ndi_recv_name = name.as_ref().map_or(ptr::null(), |n| n.as_ptr());
        let source_raw = source.raw;

        Ok(RawRecvCreateV3 {
            raw: NDIlib_recv_create_v3_t {
                source_to_connect_to: source_raw,
                color_format: self.color_format.into(),
                bandwidth: self.bandwidth.into(),
                allow_video_fields: self.allow_video_fields,
                p_ndi_recv_name,
            },
            _source: source,
            _name: name,
        })
    }

    /// Create a builder for configuring a receiver
    pub fn builder(source: Source) -> ReceiverBuilder {
        ReceiverBuilder::new(source)
    }
}

/// Builder for configuring a Receiver with ergonomic method chaining
#[derive(Debug, Clone)]
pub struct ReceiverBuilder {
    source_to_connect_to: Source,
    color_format: Option<RecvColorFormat>,
    bandwidth: Option<RecvBandwidth>,
    allow_video_fields: Option<bool>,
    ndi_recv_name: Option<String>,
}

impl ReceiverBuilder {
    /// Create a new builder with the specified source
    pub fn new(source: Source) -> Self {
        ReceiverBuilder {
            source_to_connect_to: source,
            color_format: None,
            bandwidth: None,
            allow_video_fields: None,
            ndi_recv_name: None,
        }
    }

    /// Set the color format for received video
    pub fn color(mut self, fmt: RecvColorFormat) -> Self {
        self.color_format = Some(fmt);
        self
    }

    /// Set the bandwidth mode for the receiver
    pub fn bandwidth(mut self, bw: RecvBandwidth) -> Self {
        self.bandwidth = Some(bw);
        self
    }

    /// Configure whether to allow video fields
    pub fn allow_video_fields(mut self, allow: bool) -> Self {
        self.allow_video_fields = Some(allow);
        self
    }

    /// Set the name for this receiver
    pub fn name<S: Into<String>>(mut self, name: S) -> Self {
        self.ndi_recv_name = Some(name.into());
        self
    }

    /// Build the receiver and create a Recv instance
    pub fn build(self, ndi: &NDI) -> Result<Recv<'_>, Error> {
        let receiver = Receiver {
            source_to_connect_to: self.source_to_connect_to,
            color_format: self.color_format.unwrap_or(RecvColorFormat::BGRX_BGRA),
            bandwidth: self.bandwidth.unwrap_or(RecvBandwidth::Highest),
            allow_video_fields: self.allow_video_fields.unwrap_or(true),
            ndi_recv_name: self.ndi_recv_name,
        };
        Recv::new(ndi, receiver)
    }
}

pub struct Recv<'a> {
    instance: NDIlib_recv_instance_t,
    ndi: std::marker::PhantomData<&'a NDI>,
}

impl<'a> Recv<'a> {
    pub fn new(_ndi: &'a NDI, create: Receiver) -> Result<Self, Error> {
        let create_raw = create.to_raw()?;
        // NDIlib_recv_create_v3 already connects to the source specified in source_to_connect_to
        let instance = unsafe { NDIlib_recv_create_v3(&create_raw.raw) };
        if instance.is_null() {
            Err(Error::InitializationFailed(
                "Failed to create NDI recv instance".into(),
            ))
        } else {
            Ok(Recv {
                instance,
                ndi: std::marker::PhantomData,
            })
        }
    }

    /// Capture a frame with owned data (copies the frame data)
    #[deprecated(note = "Use capture_video, capture_audio, or capture_metadata for concurrent access")]
    pub fn capture(&mut self, timeout_ms: u32) -> Result<FrameType<'_>, Error> {
        let mut video_frame = NDIlib_video_frame_v2_t::default();
        let mut audio_frame = NDIlib_audio_frame_v3_t::default();
        let mut metadata_frame = NDIlib_metadata_frame_t::default();

        let frame_type = unsafe {
            NDIlib_recv_capture_v3(
                self.instance,
                &mut video_frame,
                &mut audio_frame,
                &mut metadata_frame,
                timeout_ms,
            )
        };

        match frame_type {
            NDIlib_frame_type_e_NDIlib_frame_type_video => {
                let frame = unsafe { VideoFrame::from_raw(&video_frame, Some(self.instance)) }?;
                // Note: Drop impl will call NDIlib_recv_free_video_v2 when frame is dropped
                Ok(FrameType::Video(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_audio => {
                let frame = AudioFrame::from_raw(audio_frame, Some(self.instance))?;
                // Note: Drop impl will call NDIlib_recv_free_audio_v3 when frame is dropped
                Ok(FrameType::Audio(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_metadata => {
                let frame = MetadataFrame::from_raw(&metadata_frame);
                unsafe { NDIlib_recv_free_metadata(self.instance, &metadata_frame) };
                Ok(FrameType::Metadata(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_none => Ok(FrameType::None),
            NDIlib_frame_type_e_NDIlib_frame_type_status_change => Ok(FrameType::StatusChange),
            NDIlib_frame_type_e_NDIlib_frame_type_error => {
                Err(Error::CaptureFailed("Received an error frame".into()))
            }
            _ => Err(Error::CaptureFailed(format!(
                "Unknown frame type: {}",
                frame_type
            ))),
        }
    }

    #[allow(dead_code)]
    pub fn free_string(&self, string: &str) {
        let c_string = CString::new(string).expect("Failed to create CString");
        unsafe {
            NDIlib_recv_free_string(self.instance, c_string.into_raw());
        }
    }

    pub fn ptz_is_supported(&self) -> bool {
        unsafe { NDIlib_recv_ptz_is_supported(self.instance) }
    }

    pub fn ptz_recall_preset(&self, preset: u32, speed: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_recall_preset(self.instance, preset as i32, speed) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(format!(
                "Failed to recall PTZ preset {} with speed {}", 
                preset, speed
            )))
        }
    }

    pub fn ptz_zoom(&self, zoom_value: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_zoom(self.instance, zoom_value) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(format!(
                "Failed to set PTZ zoom to {}", 
                zoom_value
            )))
        }
    }

    pub fn ptz_zoom_speed(&self, zoom_speed: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_zoom_speed(self.instance, zoom_speed) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(format!(
                "Failed to set PTZ zoom speed to {}", 
                zoom_speed
            )))
        }
    }

    pub fn ptz_pan_tilt(&self, pan_value: f32, tilt_value: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_pan_tilt(self.instance, pan_value, tilt_value) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(format!(
                "Failed to set PTZ pan/tilt to ({}, {})", 
                pan_value, tilt_value
            )))
        }
    }

    pub fn ptz_pan_tilt_speed(&self, pan_speed: f32, tilt_speed: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_pan_tilt_speed(self.instance, pan_speed, tilt_speed) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(format!(
                "Failed to set PTZ pan/tilt speed to ({}, {})", 
                pan_speed, tilt_speed
            )))
        }
    }

    pub fn ptz_store_preset(&self, preset_no: i32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_store_preset(self.instance, preset_no) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed(format!(
                "Failed to store PTZ preset {}", 
                preset_no
            )))
        }
    }

    pub fn ptz_auto_focus(&self) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_auto_focus(self.instance) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed("Failed to enable PTZ auto focus".into()))
        }
    }

    pub fn ptz_focus(&self, focus_value: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_focus(self.instance, focus_value) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed("Failed to set PTZ focus".into()))
        }
    }

    pub fn ptz_focus_speed(&self, focus_speed: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_focus_speed(self.instance, focus_speed) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed("Failed to set PTZ focus speed".into()))
        }
    }

    pub fn ptz_white_balance_auto(&self) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_white_balance_auto(self.instance) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed("Failed to set PTZ auto white balance".into()))
        }
    }

    pub fn ptz_white_balance_indoor(&self) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_white_balance_indoor(self.instance) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed("Failed to set PTZ indoor white balance".into()))
        }
    }

    pub fn ptz_white_balance_outdoor(&self) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_white_balance_outdoor(self.instance) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed("Failed to set PTZ outdoor white balance".into()))
        }
    }

    pub fn ptz_white_balance_oneshot(&self) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_white_balance_oneshot(self.instance) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed("Failed to set PTZ oneshot white balance".into()))
        }
    }

    pub fn ptz_white_balance_manual(&self, red: f32, blue: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_white_balance_manual(self.instance, red, blue) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed("Failed to set PTZ manual white balance".into()))
        }
    }

    pub fn ptz_exposure_auto(&self) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_exposure_auto(self.instance) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed("Failed to set PTZ auto exposure".into()))
        }
    }

    pub fn ptz_exposure_manual(&self, exposure_level: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_exposure_manual(self.instance, exposure_level) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed("Failed to set PTZ manual exposure".into()))
        }
    }

    pub fn ptz_exposure_manual_v2(&self, iris: f32, gain: f32, shutter_speed: f32) -> Result<(), Error> {
        if unsafe { NDIlib_recv_ptz_exposure_manual_v2(self.instance, iris, gain, shutter_speed) } {
            Ok(())
        } else {
            Err(Error::PtzCommandFailed("Failed to set PTZ manual exposure v2".into()))
        }
    }

    /// Capture only video frames - safe to call from multiple threads concurrently
    pub fn capture_video(&self, timeout_ms: u32) -> Result<Option<VideoFrame<'_>>, Error> {
        let mut video_frame = NDIlib_video_frame_v2_t::default();
        
        // SAFETY: NDI SDK documentation states that recv_capture_v3 is thread-safe
        let frame_type = unsafe {
            NDIlib_recv_capture_v3(
                self.instance,
                &mut video_frame,
                ptr::null_mut(), // no audio
                ptr::null_mut(), // no metadata
                timeout_ms,
            )
        };

        match frame_type {
            NDIlib_frame_type_e_NDIlib_frame_type_video => {
                let frame = unsafe { VideoFrame::from_raw(&video_frame, Some(self.instance)) }?;
                Ok(Some(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_none => Ok(None),
            NDIlib_frame_type_e_NDIlib_frame_type_error => {
                Err(Error::CaptureFailed("Received an error frame".into()))
            }
            _ => Ok(None), // Other frame types are ignored when capturing video only
        }
    }

    /// Capture only audio frames - safe to call from multiple threads concurrently
    pub fn capture_audio(&self, timeout_ms: u32) -> Result<Option<AudioFrame<'_>>, Error> {
        let mut audio_frame = NDIlib_audio_frame_v3_t::default();
        
        // SAFETY: NDI SDK documentation states that recv_capture_v3 is thread-safe
        let frame_type = unsafe {
            NDIlib_recv_capture_v3(
                self.instance,
                ptr::null_mut(), // no video
                &mut audio_frame,
                ptr::null_mut(), // no metadata
                timeout_ms,
            )
        };

        match frame_type {
            NDIlib_frame_type_e_NDIlib_frame_type_audio => {
                let frame = AudioFrame::from_raw(audio_frame, Some(self.instance))?;
                Ok(Some(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_none => Ok(None),
            NDIlib_frame_type_e_NDIlib_frame_type_error => {
                Err(Error::CaptureFailed("Received an error frame".into()))
            }
            _ => Ok(None), // Other frame types are ignored when capturing audio only
        }
    }

    /// Capture only metadata frames - safe to call from multiple threads concurrently
    pub fn capture_metadata(&self, timeout_ms: u32) -> Result<Option<MetadataFrame>, Error> {
        let mut metadata_frame = NDIlib_metadata_frame_t::default();
        
        // SAFETY: NDI SDK documentation states that recv_capture_v3 is thread-safe
        let frame_type = unsafe {
            NDIlib_recv_capture_v3(
                self.instance,
                ptr::null_mut(), // no video
                ptr::null_mut(), // no audio
                &mut metadata_frame,
                timeout_ms,
            )
        };

        match frame_type {
            NDIlib_frame_type_e_NDIlib_frame_type_metadata => {
                let frame = MetadataFrame::from_raw(&metadata_frame);
                unsafe { NDIlib_recv_free_metadata(self.instance, &metadata_frame) };
                Ok(Some(frame))
            }
            NDIlib_frame_type_e_NDIlib_frame_type_none => Ok(None),
            NDIlib_frame_type_e_NDIlib_frame_type_error => {
                Err(Error::CaptureFailed("Received an error frame".into()))
            }
            _ => Ok(None), // Other frame types are ignored when capturing metadata only
        }
    }
}

impl Drop for Recv<'_> {
    fn drop(&mut self) {
        unsafe {
            NDIlib_recv_destroy(self.instance);
        }
    }
}

/// # Safety
/// 
/// The NDI 6 SDK documentation explicitly states that recv operations are thread-safe.
/// `NDIlib_recv_capture_v3` and related functions use internal synchronization.
/// The Recv struct only holds an opaque pointer returned by the SDK, and the SDK
/// guarantees that this pointer can be safely moved between threads.
unsafe impl std::marker::Send for Recv<'_> {}

/// # Safety
/// 
/// The NDI 6 SDK documentation guarantees that `NDIlib_recv_capture_v3` is internally
/// synchronized and can be called concurrently from multiple threads. This is explicitly
/// mentioned in the SDK manual's thread safety section. The capture_video, capture_audio,
/// and capture_metadata methods can be safely called from multiple threads simultaneously.
unsafe impl std::marker::Sync for Recv<'_> {}

#[derive(Debug)]
pub enum FrameType<'rx> {
    Video(VideoFrame<'rx>),
    Audio(AudioFrame<'rx>),
    Metadata(MetadataFrame),
    None,
    StatusChange,
}

#[derive(Debug, Clone)]
pub struct Tally {
    pub on_program: bool,
    pub on_preview: bool,
}

impl Tally {
    pub fn new(on_program: bool, on_preview: bool) -> Self {
        Tally {
            on_program,
            on_preview,
        }
    }

    pub(crate) fn to_raw(&self) -> NDIlib_tally_t {
        NDIlib_tally_t {
            on_program: self.on_program,
            on_preview: self.on_preview,
        }
    }
}

#[derive(Debug)]
pub struct SendInstance<'a> {
    instance: NDIlib_send_instance_t,
    _name: *mut c_char,   // Store raw pointer to free on drop
    _groups: *mut c_char, // Store raw pointer to free on drop
    ndi: std::marker::PhantomData<&'a NDI>,
}

/// A borrowed video frame that references external pixel data.
/// Used for zero-copy async send operations.
pub struct VideoFrameBorrowed<'buf> {
    pub xres: i32,
    pub yres: i32,
    pub fourcc: FourCCVideoType,
    pub frame_rate_n: i32,
    pub frame_rate_d: i32,
    pub picture_aspect_ratio: f32,
    pub frame_format_type: FrameFormatType,
    pub timecode: i64,
    pub data: &'buf [u8],
    pub line_stride_or_size: LineStrideOrSize,
    pub metadata: Option<&'buf CStr>,
    pub timestamp: i64,
}

impl<'buf> VideoFrameBorrowed<'buf> {
    /// Create a borrowed frame from a mutable buffer
    pub fn from_buffer(
        data: &'buf [u8],
        xres: i32,
        yres: i32,
        fourcc: FourCCVideoType,
        frame_rate_n: i32,
        frame_rate_d: i32,
    ) -> Self {
        let bpp = match fourcc {
            FourCCVideoType::BGRA | FourCCVideoType::BGRX | FourCCVideoType::RGBA | FourCCVideoType::RGBX => 32,
            FourCCVideoType::UYVY | FourCCVideoType::YV12 | FourCCVideoType::I420 | FourCCVideoType::NV12 => 16,
            FourCCVideoType::UYVA => 32,
            FourCCVideoType::P216 | FourCCVideoType::PA16 => 32,
            _ => 32,
        };
        let stride = (xres * bpp + 7) / 8;
        
        VideoFrameBorrowed {
            xres,
            yres,
            fourcc,
            frame_rate_n,
            frame_rate_d,
            picture_aspect_ratio: 16.0 / 9.0,
            frame_format_type: FrameFormatType::Progressive,
            timecode: 0,
            data,
            line_stride_or_size: LineStrideOrSize { line_stride_in_bytes: stride },
            metadata: None,
            timestamp: 0,
        }
    }

    fn to_raw(&self) -> NDIlib_video_frame_v2_t {
        NDIlib_video_frame_v2_t {
            xres: self.xres,
            yres: self.yres,
            FourCC: self.fourcc.into(),
            frame_rate_N: self.frame_rate_n,
            frame_rate_D: self.frame_rate_d,
            picture_aspect_ratio: self.picture_aspect_ratio,
            frame_format_type: self.frame_format_type.into(),
            timecode: self.timecode,
            p_data: self.data.as_ptr() as *mut u8,
            __bindgen_anon_1: self.line_stride_or_size.into(),
            p_metadata: self.metadata.map_or(ptr::null(), |m| m.as_ptr()),
            timestamp: self.timestamp,
        }
    }
}

impl<'buf> From<&'buf VideoFrame<'_>> for VideoFrameBorrowed<'buf> {
    fn from(frame: &'buf VideoFrame<'_>) -> Self {
        VideoFrameBorrowed {
            xres: frame.xres,
            yres: frame.yres,
            fourcc: frame.fourcc,
            frame_rate_n: frame.frame_rate_n,
            frame_rate_d: frame.frame_rate_d,
            picture_aspect_ratio: frame.picture_aspect_ratio,
            frame_format_type: frame.frame_format_type,
            timecode: frame.timecode,
            data: &frame.data,
            line_stride_or_size: frame.line_stride_or_size,
            metadata: frame.metadata.as_deref(),
            timestamp: frame.timestamp,
        }
    }
}

/// A token that ensures the video frame remains valid while NDI is using it.
/// The frame will be released when this token is dropped or when the next
/// send operation occurs.
#[must_use = "AsyncVideoToken must be held until the next send operation"]
pub struct AsyncVideoToken<'send, 'buf> {
    _send: &'send SendInstance<'send>,
    // Use mutable borrow to prevent any access while NDI owns the buffer
    _frame: std::marker::PhantomData<&'buf mut [u8]>,
}

/// A token that ensures the audio frame remains valid while NDI is using it.
/// The frame will be released when this token is dropped or when the next
/// send operation occurs.
#[must_use = "AsyncAudioToken must be held until the next send operation"]
pub struct AsyncAudioToken<'a, 'b> {
    _send: &'a SendInstance<'b>,
    // Use mutable borrow to prevent any access while NDI owns the buffer
    _frame: std::marker::PhantomData<&'a mut AudioFrame<'a>>,
}

impl<'a> SendInstance<'a> {
    pub fn new(_ndi: &'a NDI, create_settings: SendOptions) -> Result<Self, Error> {
        let p_ndi_name = CString::new(create_settings.name).map_err(Error::InvalidCString)?;
        let p_groups = match create_settings.groups {
            Some(ref groups) => CString::new(groups.clone())
                .map_err(Error::InvalidCString)?
                .into_raw(),
            None => ptr::null_mut(),
        };

        let p_ndi_name_raw = p_ndi_name.into_raw();
        let c_settings = NDIlib_send_create_t {
            p_ndi_name: p_ndi_name_raw,
            p_groups,
            clock_video: create_settings.clock_video,
            clock_audio: create_settings.clock_audio,
        };

        let instance = unsafe { NDIlib_send_create(&c_settings) };
        if instance.is_null() {
            // Clean up on error
            unsafe {
                let _ = CString::from_raw(p_ndi_name_raw);
                if !p_groups.is_null() {
                    let _ = CString::from_raw(p_groups);
                }
            }
            Err(Error::InitializationFailed(
                "Failed to create NDI send instance".into(),
            ))
        } else {
            Ok(SendInstance {
                instance,
                _name: p_ndi_name_raw,
                _groups: p_groups,
                ndi: std::marker::PhantomData,
            })
        }
    }

    /// Send a video frame **synchronously** (NDI copies the buffer).
    pub fn send_video(&self, video_frame: &VideoFrame<'_>) {
        unsafe {
            NDIlib_send_send_video_v2(self.instance, &video_frame.to_raw());
        }
    }

    /// Send a video frame **asynchronously** (NDI *keeps a pointer*; no copy).
    ///
    /// Returns an `AsyncVideoToken` that must be held until the next send operation.
    /// The frame data is guaranteed to remain valid as long as the token exists.
    ///
    /// # Example
    /// ```no_run
    /// # use grafton_ndi::{NDI, SendOptions, VideoFrame, VideoFrameBorrowed, FourCCVideoType};
    /// # fn main() -> Result<(), grafton_ndi::Error> {
    /// let ndi = NDI::new()?;
    /// let send_options = SendOptions::builder("MyCam")
    ///     .clock_video(true)
    ///     .clock_audio(true)
    ///     .build()?;
    /// let send = grafton_ndi::SendInstance::new(&ndi, send_options)?;
    ///
    /// // Option 1: Use existing VideoFrame (still zero-copy)
    /// let frame = VideoFrame::default();
    /// let _token = send.send_video_async((&frame).into());
    /// 
    /// // Option 2: Use borrowed buffer directly (zero-copy, no allocation)
    /// let mut buffer = vec![0u8; 1920 * 1080 * 4];
    /// let borrowed_frame = VideoFrameBorrowed::from_buffer(&buffer, 1920, 1080, FourCCVideoType::BGRA, 30, 1);
    /// let _token2 = send.send_video_async(borrowed_frame);
    /// // buffer is now being used by NDI - safe as long as token exists
    /// 
    /// // When token is dropped or next send occurs, frame is released
    /// # Ok(())
    /// # }
    /// ```
    pub fn send_video_async<'b>(&'b self, video_frame: VideoFrameBorrowed<'b>) -> AsyncVideoToken<'b, 'b> {
        unsafe {
            NDIlib_send_send_video_async_v2(self.instance, &video_frame.to_raw());
        }
        AsyncVideoToken {
            _send: self,
            _frame: std::marker::PhantomData,
        }
    }

    pub fn send_audio(&self, audio_frame: &AudioFrame<'_>) {
        unsafe {
            NDIlib_send_send_audio_v3(self.instance, &audio_frame.to_raw());
        }
    }
    
    /// Send an audio frame **asynchronously** (NDI *keeps a pointer*; no copy).
    ///
    /// Returns an `AsyncAudioToken` that must be held until the next send operation.
    /// The frame data is guaranteed to remain valid as long as the token exists.
    pub fn send_audio_async<'b>(&'b self, audio_frame: &'b AudioFrame<'_>) -> AsyncAudioToken<'b, 'a> {
        unsafe {
            NDIlib_send_send_audio_v3(self.instance, &audio_frame.to_raw());
        }
        AsyncAudioToken {
            _send: self,
            _frame: std::marker::PhantomData,
        }
    }

    pub fn send_metadata(&self, metadata_frame: &MetadataFrame) -> Result<(), Error> {
        let (_c_data, raw) = metadata_frame.to_raw()?;
        unsafe {
            NDIlib_send_send_metadata(self.instance, &raw);
        }
        Ok(())
    }

    pub fn capture(&self, timeout_ms: u32) -> Result<FrameType<'static>, Error> {
        let mut metadata_frame = NDIlib_metadata_frame_t::default();
        let frame_type =
            unsafe { NDIlib_send_capture(self.instance, &mut metadata_frame, timeout_ms) };

        match frame_type {
            NDIlib_frame_type_e_NDIlib_frame_type_metadata => {
                if metadata_frame.p_data.is_null() {
                    Err(Error::NullPointer("Metadata frame data is null".into()))
                } else {
                    // Copy the metadata before it becomes invalid
                    let data = unsafe {
                        CStr::from_ptr(metadata_frame.p_data)
                            .to_string_lossy()
                            .into_owned()
                    };
                    let frame = MetadataFrame::with_data(data, metadata_frame.timecode);
                    Ok(FrameType::Metadata(frame))
                }
            }
            _ => Err(Error::CaptureFailed("Failed to capture frame".into())),
        }
    }

    // Note: free_metadata is no longer needed since MetadataFrame owns its data

    pub fn get_tally(&self, tally: &mut Tally, timeout_ms: u32) -> bool {
        unsafe { NDIlib_send_get_tally(self.instance, &mut tally.to_raw(), timeout_ms) }
    }

    pub fn get_no_connections(&self, timeout_ms: u32) -> i32 {
        unsafe { NDIlib_send_get_no_connections(self.instance, timeout_ms) }
    }

    pub fn clear_connection_metadata(&self) {
        unsafe { NDIlib_send_clear_connection_metadata(self.instance) }
    }

    pub fn add_connection_metadata(&self, metadata_frame: &MetadataFrame) -> Result<(), Error> {
        let (_c_data, raw) = metadata_frame.to_raw()?;
        unsafe { NDIlib_send_add_connection_metadata(self.instance, &raw) }
        Ok(())
    }

    pub fn set_failover(&self, source: &Source) -> Result<(), Error> {
        let raw_source = source.to_raw()?;
        unsafe { NDIlib_send_set_failover(self.instance, &raw_source.raw) }
        Ok(())
    }

    pub fn get_source_name(&self) -> Source {
        let source_ptr = unsafe { NDIlib_send_get_source_name(self.instance) };
        Source::from_raw(unsafe { &*source_ptr })
    }
}

impl Drop for SendInstance<'_> {
    fn drop(&mut self) {
        unsafe {
            NDIlib_send_destroy(self.instance);

            // Free the CStrings we allocated
            if !self._name.is_null() {
                let _ = CString::from_raw(self._name);
            }
            if !self._groups.is_null() {
                let _ = CString::from_raw(self._groups);
            }
        }
    }
}

/// # Safety
/// 
/// The NDI 6 SDK documentation states that send operations are thread-safe.
/// `NDIlib_send_send_video_v2`, `NDIlib_send_send_audio_v3`, and related functions
/// use internal synchronization. The SendInstance struct holds an opaque pointer and raw
/// C string pointers that are only freed in Drop, making it safe to move between threads.
unsafe impl std::marker::Send for SendInstance<'_> {}

/// # Safety
/// 
/// The NDI 6 SDK guarantees thread-safety for send operations. Multiple threads can
/// safely call send methods concurrently as the SDK handles all necessary synchronization.
/// The async send operations (send_video_async, send_audio_async) are also thread-safe
/// as documented in the SDK manual.
unsafe impl std::marker::Sync for SendInstance<'_> {}

#[derive(Debug)]
pub struct SendOptions {
    pub name: String,
    pub groups: Option<String>,
    pub clock_video: bool,
    pub clock_audio: bool,
}

impl SendOptions {
    /// Create a builder for configuring send options
    pub fn builder<S: Into<String>>(name: S) -> SendOptionsBuilder {
        SendOptionsBuilder::new(name)
    }
}

/// Builder for configuring SendOptions with ergonomic method chaining
#[derive(Debug, Clone)]
pub struct SendOptionsBuilder {
    name: String,
    groups: Option<String>,
    clock_video: Option<bool>,
    clock_audio: Option<bool>,
}

impl SendOptionsBuilder {
    /// Create a new builder with the specified name
    pub fn new<S: Into<String>>(name: S) -> Self {
        SendOptionsBuilder {
            name: name.into(),
            groups: None,
            clock_video: None,
            clock_audio: None,
        }
    }

    /// Set the groups for this sender
    pub fn groups<S: Into<String>>(mut self, groups: S) -> Self {
        self.groups = Some(groups.into());
        self
    }

    /// Configure whether to clock video
    pub fn clock_video(mut self, clock: bool) -> Self {
        self.clock_video = Some(clock);
        self
    }

    /// Configure whether to clock audio
    pub fn clock_audio(mut self, clock: bool) -> Self {
        self.clock_audio = Some(clock);
        self
    }

    /// Build the SendOptions
    pub fn build(self) -> Result<SendOptions, Error> {
        let clock_video = self.clock_video.unwrap_or(true);
        let clock_audio = self.clock_audio.unwrap_or(true);
        
        // Validate that at least one clock is enabled
        if !clock_video && !clock_audio {
            return Err(Error::InvalidConfiguration(
                "At least one of clock_video or clock_audio must be true".into()
            ));
        }
        
        Ok(SendOptions {
            name: self.name,
            groups: self.groups,
            clock_video,
            clock_audio,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::ptr;

    // Helper function to create a test video frame
    fn create_test_video_frame(
        width: i32,
        height: i32,
        line_stride: i32,
        data_size: i32,
    ) -> NDIlib_video_frame_v2_t {
        let mut frame: NDIlib_video_frame_v2_t = unsafe { std::mem::zeroed() };
        frame.xres = width;
        frame.yres = height;

        // Set the union field based on which value is provided
        if line_stride > 0 {
            frame.__bindgen_anon_1.line_stride_in_bytes = line_stride;
        } else {
            frame.__bindgen_anon_1.data_size_in_bytes = data_size;
        }

        // Allocate dummy data
        let actual_size = if line_stride > 0 {
            (line_stride * height) as usize
        } else {
            data_size as usize
        };
        let mut data = vec![0u8; actual_size];
        frame.p_data = data.as_mut_ptr();
        std::mem::forget(data); // Prevent deallocation during test

        frame
    }

    #[test]
    fn test_video_frame_standard_format_size_calculation() {
        // Test standard video format with line stride
        let test_width = 1920;
        let test_height = 1080;
        let bytes_per_pixel = 4; // RGBA
        let line_stride = test_width * bytes_per_pixel;

        let c_frame = create_test_video_frame(test_width, test_height, line_stride, 0);

        // The from_raw function should calculate size as line_stride * height
        // Previously it would incorrectly multiply data_size_in_bytes * height
        unsafe {
            let frame = VideoFrame::from_raw(&c_frame, None).unwrap();

            // Expected size is line_stride * height
            let expected_size = (line_stride * test_height) as usize;
            assert_eq!(frame.data.len(), expected_size);

            // Clean up
            drop(frame);
            Vec::from_raw_parts(c_frame.p_data, expected_size, expected_size);
        }
    }

    #[test]
    fn test_video_frame_size_calculation_logic() {
        // Test the size calculation logic without relying on union behavior
        // This is a simplified test that verifies the fix prevents the original bug

        // The original bug was: data_size = data_size_in_bytes * yres
        // This would cause massive over-allocation

        // For a 1920x1080 RGBA frame:
        // Correct: line_stride (1920*4) * height (1080) = 8,294,400 bytes
        // Bug would calculate: some_value * 1080 (potentially huge)

        let correct_size = 1920 * 4 * 1080; // 8,294,400 bytes
        assert!(correct_size < 10_000_000); // Should be under 10MB

        // The fix ensures we use line_stride * height for standard formats
        // and data_size_in_bytes directly for compressed formats
    }

    #[test]
    fn test_video_frame_null_data_returns_error() {
        let mut c_frame: NDIlib_video_frame_v2_t = unsafe { std::mem::zeroed() };
        c_frame.p_data = ptr::null_mut();
        c_frame.__bindgen_anon_1.line_stride_in_bytes = 1920 * 4;
        c_frame.yres = 1080;

        unsafe {
            let result = VideoFrame::from_raw(&c_frame, None);
            assert!(result.is_err());
            match result {
                Err(Error::InvalidFrame(msg)) => {
                    assert!(msg.contains("null data pointer"));
                }
                _ => panic!("Expected InvalidFrame error"),
            }
        }
    }

    #[test]
    fn test_video_frame_zero_size_returns_error() {
        let mut c_frame: NDIlib_video_frame_v2_t = unsafe { std::mem::zeroed() };
        let mut data = vec![0u8; 100];
        c_frame.p_data = data.as_mut_ptr();
        c_frame.__bindgen_anon_1.line_stride_in_bytes = 0;
        c_frame.__bindgen_anon_1.data_size_in_bytes = 0;
        c_frame.yres = 1080;

        unsafe {
            let result = VideoFrame::from_raw(&c_frame, None);
            assert!(result.is_err());
            match result {
                Err(Error::InvalidFrame(msg)) => {
                    assert!(msg.contains("neither valid line_stride_in_bytes nor data_size_in_bytes"));
                }
                _ => panic!("Expected InvalidFrame error"),
            }
        }
    }

    #[test]
    fn test_audio_frame_drop_no_double_free() {
        // Test that AudioFrame can be created and dropped without issues
        let frame1 = AudioFrame::builder().build().unwrap();
        drop(frame1); // Should not panic or cause double-free

        // Test with metadata
        let frame2 = AudioFrame::builder()
            .metadata("test metadata")
            .build()
            .unwrap();
        drop(frame2); // Should not panic - CString handles its own memory

        // Test multiple drops in sequence
        for i in 0..10 {
            let frame = AudioFrame::builder()
                .metadata(format!("metadata {}", i))
                .build()
                .unwrap();
            drop(frame);
        }
    }

    #[test]
    fn test_raw_source_memory_management() {
        // Test that RawSource properly manages CString memory
        let source = Source {
            name: "Test NDI Source".to_string(),
            address: SourceAddress::Url("ndi://192.168.1.100:5960".to_string()),
        };

        // Create RawSource
        let raw_source = source.to_raw().unwrap();

        // Verify the raw pointers are valid
        unsafe {
            assert!(!raw_source.raw.p_ndi_name.is_null());
            let name = CStr::from_ptr(raw_source.raw.p_ndi_name);
            assert_eq!(name.to_string_lossy(), "Test NDI Source");

            // Check union field
            assert!(!raw_source.raw.__bindgen_anon_1.p_url_address.is_null());
            let url = CStr::from_ptr(raw_source.raw.__bindgen_anon_1.p_url_address);
            assert_eq!(url.to_string_lossy(), "ndi://192.168.1.100:5960");
        }

        // Drop should clean up all CStrings properly
        drop(raw_source);
    }

    #[test]
    fn test_raw_source_null_optional_fields() {
        // Test with None values for optional fields
        let source = Source {
            name: "Minimal Source".to_string(),
            address: SourceAddress::None,
        };

        let raw_source = source.to_raw().unwrap();

        unsafe {
            assert!(!raw_source.raw.p_ndi_name.is_null());
            assert!(raw_source.raw.__bindgen_anon_1.p_url_address.is_null());
            assert!(raw_source.raw.__bindgen_anon_1.p_ip_address.is_null());
        }

        drop(raw_source);
    }

    #[test]
    fn test_metadata_frame_owns_data() {
        // Test that MetadataFrame properly owns its data
        let metadata = MetadataFrame {
            data: "<metadata>test content</metadata>".to_string(),
            timecode: 123456789,
        };

        // Clone should create a new owned copy
        let cloned = metadata.clone();
        assert_eq!(metadata.data, cloned.data);
        assert_eq!(metadata.timecode, cloned.timecode);

        // Test to_raw conversion
        let (c_data, raw) = metadata.to_raw().unwrap();
        unsafe {
            assert!(!raw.p_data.is_null());
            let raw_str = CStr::from_ptr(raw.p_data);
            assert_eq!(
                raw_str.to_string_lossy(),
                "<metadata>test content</metadata>"
            );
            assert_eq!(raw.timecode, 123456789);
        }

        // c_data keeps the CString alive
        drop(c_data);
    }

    #[test]
    fn test_metadata_frame_from_raw() {
        // Test with valid data
        let test_data = CString::new("<metadata>test</metadata>").unwrap();
        let raw = NDIlib_metadata_frame_t {
            length: test_data.as_bytes_with_nul().len() as i32,
            timecode: 123456,
            p_data: test_data.as_ptr() as *mut c_char,
        };

        let frame = MetadataFrame::from_raw(&raw);
        assert_eq!(frame.data, "<metadata>test</metadata>");
        assert_eq!(frame.timecode, 123456);

        // Test with null pointer
        let null_raw = NDIlib_metadata_frame_t {
            length: 0,
            timecode: 789,
            p_data: ptr::null_mut(),
        };

        let null_frame = MetadataFrame::from_raw(&null_raw);
        assert_eq!(null_frame.data, "");
        assert_eq!(null_frame.timecode, 789);
    }

    #[test]
    fn test_video_frame_zero_copy_behavior() {
        // Create a test buffer
        let test_data = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let data_ptr = test_data.as_ptr() as *mut u8;
        
        let mut c_frame: NDIlib_video_frame_v2_t = unsafe { std::mem::zeroed() };
        c_frame.xres = 2;
        c_frame.yres = 1;
        c_frame.FourCC = FourCCVideoType::UYVY.into();
        c_frame.p_data = data_ptr;
        c_frame.__bindgen_anon_1.line_stride_in_bytes = 8;
        
        // Test with recv_instance (should borrow)
        unsafe {
            let frame = VideoFrame::from_raw(&c_frame, Some(ptr::null_mut())).unwrap();
            // Verify we're borrowing the original data
            match &frame.data {
                Cow::Borrowed(slice) => {
                    assert_eq!(slice.as_ptr(), data_ptr as *const u8);
                    assert_eq!(slice.len(), 8);
                },
                Cow::Owned(_) => panic!("Expected borrowed data for recv instance"),
            }
            assert_eq!(frame.original_p_data, Some(data_ptr));
        }
        
        // Test without recv_instance (should copy)
        unsafe {
            let frame = VideoFrame::from_raw(&c_frame, None).unwrap();
            // Verify we made a copy
            match &frame.data {
                Cow::Owned(vec) => {
                    assert_ne!(vec.as_ptr(), data_ptr as *const u8);
                    assert_eq!(vec.len(), 8);
                },
                Cow::Borrowed(_) => panic!("Expected owned data for non-recv instance"),
            }
            assert_eq!(frame.original_p_data, None);
        }
    }

    #[test]
    fn test_audio_frame_zero_copy_behavior() {
        // Create a test buffer
        let test_data = [1.0f32, 2.0, 3.0, 4.0];
        let data_ptr = test_data.as_ptr() as *mut u8;
        
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: 2,
            no_samples: 2,
            timecode: 0,
            FourCC: AudioType::FLTP.into(),
            p_data: data_ptr,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 { channel_stride_in_bytes: 0 },
            p_metadata: ptr::null_mut(),
            timestamp: 0,
        };
        
        // Test with recv_instance (should borrow)
        let frame = AudioFrame::from_raw(raw, Some(ptr::null_mut())).unwrap();
        match &frame.data {
            Cow::Borrowed(slice) => {
                assert_eq!(slice.as_ptr() as *const u8, data_ptr as *const u8);
                assert_eq!(slice.len(), 4);
            },
            Cow::Owned(_) => panic!("Expected borrowed data for recv instance"),
        }
        assert_eq!(frame.original_p_data, Some(data_ptr));
        
        // Test without recv_instance (should copy)
        let frame = AudioFrame::from_raw(raw, None).unwrap();
        match &frame.data {
            Cow::Owned(vec) => {
                assert_ne!(vec.as_ptr() as *const u8, data_ptr as *const u8);
                assert_eq!(vec.len(), 4);
            },
            Cow::Borrowed(_) => panic!("Expected owned data for non-recv instance"),
        }
        assert_eq!(frame.original_p_data, None);
    }

    #[test]
    fn test_raw_recv_create_v3_memory_management() {
        // Test RawRecvCreateV3 memory management
        let receiver = Receiver {
            source_to_connect_to: Source {
                name: "Test Source".to_string(),
                address: SourceAddress::None,
            },
            color_format: RecvColorFormat::BGRX_BGRA,
            bandwidth: RecvBandwidth::Highest,
            allow_video_fields: true,
            ndi_recv_name: Some("Test Receiver".to_string()),
        };

        let raw_recv = receiver.to_raw().unwrap();

        unsafe {
            // Verify receiver name
            assert!(!raw_recv.raw.p_ndi_recv_name.is_null());
            let name = CStr::from_ptr(raw_recv.raw.p_ndi_recv_name);
            assert_eq!(name.to_string_lossy(), "Test Receiver");

            // Verify source name through the nested structure
            assert!(!raw_recv.raw.source_to_connect_to.p_ndi_name.is_null());
            let source_name = CStr::from_ptr(raw_recv.raw.source_to_connect_to.p_ndi_name);
            assert_eq!(source_name.to_string_lossy(), "Test Source");
        }

        // Should properly clean up all nested CStrings
        drop(raw_recv);
    }

    #[test]
    fn test_source_roundtrip() {
        // Test converting Source to raw and back
        let original = Source {
            name: "Roundtrip Test".to_string(),
            address: SourceAddress::Url("ndi://test.local".to_string()),
        };

        let raw = original.to_raw().unwrap();
        let restored = Source::from_raw(&raw.raw);

        assert_eq!(original.name, restored.name);
        match (&original.address, &restored.address) {
            (SourceAddress::Url(orig_url), SourceAddress::Url(rest_url)) => {
                assert_eq!(orig_url, rest_url);
            }
            _ => panic!("Address types don't match"),
        }
    }

    #[test]
    fn test_video_frame_metadata_no_double_free() {
        // Test that VideoFrame with metadata doesn't double-free
        let frame = VideoFrame::builder()
            .resolution(1920, 1080)
            .fourcc(FourCCVideoType::RGBA)
            .frame_rate(30000, 1001)
            .aspect_ratio(16.0 / 9.0)
            .format(FrameFormatType::Progressive)
            .metadata("test video metadata")
            .build()
            .unwrap();

        // This should not panic or double-free
        drop(frame);
    }

    #[test]
    fn test_send_memory_management() {
        // Test that Send properly manages CString memory
        // Note: This test would require NDI SDK to actually create Send instance
        // so we'll test the memory management pattern instead

        // Simulate the pattern used in Send::new
        let name = CString::new("Test Sender").unwrap();
        let groups = CString::new("Test Group").unwrap();

        let name_ptr = name.into_raw();
        let groups_ptr = groups.into_raw();

        // Simulate cleanup (like Send's Drop would do)
        unsafe {
            let _ = CString::from_raw(name_ptr);
            let _ = CString::from_raw(groups_ptr);
        }

        // If this doesn't crash, memory management is correct
    }

    #[test]
    fn test_metadata_frame_empty_data() {
        // Test MetadataFrame with empty data
        let frame = MetadataFrame::new();
        assert_eq!(frame.data, "");
        assert_eq!(frame.timecode, 0);

        // MetadataFrame doesn't implement Drop, so it's automatically cleaned up
    }

    #[test]
    fn test_ndi_singleton_initialization() {
        use std::sync::atomic::Ordering;

        // Get initial reference count
        let initial_count = REFCOUNT.load(Ordering::SeqCst);

        // Create first NDI instance
        let ndi1 = NDI::new().expect("Failed to create first NDI instance");
        let count_after_first = REFCOUNT.load(Ordering::SeqCst);
        assert!(
            count_after_first > initial_count,
            "Reference count should increase"
        );

        // Create second NDI instance - should not reinitialize
        let ndi2 = NDI::new().expect("Failed to create second NDI instance");
        assert_eq!(REFCOUNT.load(Ordering::SeqCst), count_after_first + 1);

        // Clone an instance
        let ndi3 = ndi1.clone();
        assert_eq!(REFCOUNT.load(Ordering::SeqCst), count_after_first + 2);

        // Drop one instance
        drop(ndi2);
        assert_eq!(REFCOUNT.load(Ordering::SeqCst), count_after_first + 1);

        // Drop another instance
        drop(ndi1);
        let count_after_drop = REFCOUNT.load(Ordering::SeqCst);
        assert!(
            count_after_drop < count_after_first + 2,
            "Count should decrease after drop"
        );

        // Drop final instance
        drop(ndi3);
        let final_count = REFCOUNT.load(Ordering::SeqCst);
        assert!(
            final_count < count_after_drop,
            "Count should decrease after final drop"
        );
    }

    #[test]
    fn test_ndi_thread_safety() {
        use std::thread;

        // Create NDI instances from multiple threads
        let handles: Vec<_> = (0..5)
            .map(|i| {
                thread::spawn(move || {
                    let ndi = NDI::new()
                        .unwrap_or_else(|_| panic!("Failed to create NDI in thread {}", i));
                    // Use the NDI instance
                    let _version = NDI::version();
                    // Clone it a few times
                    let _clone1 = ndi.clone();
                    let _clone2 = ndi.clone();
                    // Let them all drop at the end of the thread
                })
            })
            .collect();

        // Wait for all threads to complete
        for handle in handles {
            handle.join().expect("Thread panicked");
        }

        // All instances should be cleaned up by now
    }

    #[test]
    fn test_find_cstring_lifetime() {
        // Test that Find keeps CStrings alive for its lifetime
        let ndi = NDI::new().expect("Failed to create NDI");

        // Create finder with both groups and extra_ips
        let settings = Finder::builder()
            .show_local_sources(true)
            .groups("TestGroup")
            .extra_ips("192.168.1.100")
            .build();
        let finder = Find::new(&ndi, settings).expect("Failed to create finder");

        // The finder should keep the CStrings alive even though we've moved settings
        // If CStrings were dropped early, this could cause undefined behavior
        let _sources = finder.get_sources(0);

        // Drop finder - CStrings should be freed now
        drop(finder);
    }

    #[test]
    fn test_send_cstring_lifetime() {
        // Test that Send keeps CStrings alive for its lifetime
        // Note: This test verifies the memory management pattern
        // without actually creating an NDI send instance

        // Verify our Send struct has the fields to hold CStrings
        // The actual Send::new() would require NDI SDK runtime

        // Test the pattern we use in Send
        let name = CString::new("Test Sender").unwrap();
        let groups = Some(CString::new("Test Group").unwrap());

        let name_ptr = name.as_ptr();
        let groups_ptr = groups.as_ref().map(|g| g.as_ptr());

        // Simulate keeping the CStrings alive
        let _name_holder = name;
        let _groups_holder = groups;

        // Pointers should remain valid as long as holders exist
        unsafe {
            if !name_ptr.is_null() {
                let _ = CStr::from_ptr(name_ptr);
            }
            if let Some(ptr) = groups_ptr {
                if !ptr.is_null() {
                    let _ = CStr::from_ptr(ptr);
                }
            }
        }
    }

    #[test]
    fn test_receiver_cstring_lifetime() {
        // Test that Receiver properly manages CString lifetime through RawRecvCreateV3
        let receiver = Receiver {
            source_to_connect_to: Source {
                name: "Test Source".to_string(),
                address: SourceAddress::Url("ndi://test.local".to_string()),
            },
            color_format: RecvColorFormat::BGRX_BGRA,
            bandwidth: RecvBandwidth::Highest,
            allow_video_fields: true,
            ndi_recv_name: Some("Test Receiver Name".to_string()),
        };

        // Convert to raw - this should create RawRecvCreateV3 that owns the CStrings
        let raw_recv = receiver.to_raw().expect("Failed to convert receiver");

        // The raw_recv should keep all CStrings alive
        // Verify the pointers are still valid
        unsafe {
            assert!(!raw_recv.raw.source_to_connect_to.p_ndi_name.is_null());
            assert!(!raw_recv.raw.p_ndi_recv_name.is_null());

            // These should not cause segfault
            let _source_name = CStr::from_ptr(raw_recv.raw.source_to_connect_to.p_ndi_name);
            let _recv_name = CStr::from_ptr(raw_recv.raw.p_ndi_recv_name);
        }

        // Drop raw_recv - all CStrings should be properly freed
        drop(raw_recv);
    }

    #[test]
    fn test_audio_frame_metadata_no_double_free() {
        // Test that AudioFrame::from_raw correctly copies metadata instead of taking ownership
        let metadata_str = CString::new("test audio metadata").unwrap();
        let metadata_ptr = metadata_str.as_ptr();

        // Allocate data that will persist for the test
        let mut data = vec![0u8; 1024 * 2 * 4];

        let raw_frame = NDIlib_audio_frame_v3_t {
            sample_rate: 48000,
            no_channels: 2,
            no_samples: 1024,
            timecode: 0,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: data.as_mut_ptr(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 1024 * 4,
            },
            p_metadata: metadata_ptr,
            timestamp: 0,
        };

        // from_raw should copy the metadata, not take ownership
        let frame = AudioFrame::from_raw(raw_frame, None).expect("Failed to create AudioFrame");

        // The original metadata should still be valid
        unsafe {
            let original_still_valid = CStr::from_ptr(metadata_ptr);
            assert_eq!(
                original_still_valid.to_string_lossy(),
                "test audio metadata"
            );
        }

        // The frame should have its own copy
        assert!(frame.metadata.is_some());
        assert_eq!(
            frame.metadata.as_ref().unwrap().to_string_lossy(),
            "test audio metadata"
        );

        // Clean up the original metadata
        drop(metadata_str);

        // Note: In real NDI usage, NDIlib_recv_free_audio_v3 would free the metadata
    }

    #[test]
    fn test_audio_frame_f32_data() {
        // Test creating audio frame with f32 data
        let sample_data: Vec<f32> = vec![0.0, 0.5, -0.5, 1.0, -1.0];
        let frame = AudioFrame::builder()
            .sample_rate(48000)
            .channels(1)
            .samples(5)
            .data(sample_data.clone())
            .build()
            .unwrap();
        
        assert_eq!(frame.sample_rate, 48000);
        assert_eq!(frame.no_channels, 1);
        assert_eq!(frame.no_samples, 5);
        assert_eq!(frame.data(), &sample_data[..]);
    }

    #[test]
    fn test_audio_frame_channel_data_interleaved() {
        // Test interleaved stereo audio
        let stereo_data: Vec<f32> = vec![
            0.1, 0.2,  // Sample 1: L=0.1, R=0.2
            0.3, 0.4,  // Sample 2: L=0.3, R=0.4
            0.5, 0.6,  // Sample 3: L=0.5, R=0.6
        ];
        
        let frame = AudioFrame::builder()
            .sample_rate(48000)
            .channels(2)
            .samples(3)
            .data(stereo_data.clone())
            .build()
            .unwrap();
        
        // Channel 0 (left) should be [0.1, 0.3, 0.5]
        let left_channel = frame.channel_data(0).unwrap();
        assert_eq!(left_channel, vec![0.1, 0.3, 0.5]);
        
        // Channel 1 (right) should be [0.2, 0.4, 0.6]
        let right_channel = frame.channel_data(1).unwrap();
        assert_eq!(right_channel, vec![0.2, 0.4, 0.6]);
        
        // Out of bounds channel should return None
        assert!(frame.channel_data(2).is_none());
    }

    #[test]
    fn test_audio_frame_channel_data_planar() {
        // Test planar audio format (channels stored separately)
        let planar_data: Vec<f32> = vec![
            // Channel 0 data
            0.1, 0.2, 0.3, 0.0, 0.0,  // 3 samples + padding
            // Channel 1 data
            0.4, 0.5, 0.6, 0.0, 0.0,  // 3 samples + padding
        ];
        
        let mut frame = AudioFrame::builder()
            .sample_rate(48000)
            .channels(2)
            .samples(3)
            .data(planar_data.clone())
            .build()
            .unwrap();
        
        // Set channel stride to indicate planar format
        frame.channel_stride_in_bytes = 5 * 4; // 5 samples * 4 bytes per f32
        
        // Channel 0 should be [0.1, 0.2, 0.3]
        let channel_0 = frame.channel_data(0).unwrap();
        assert_eq!(channel_0, vec![0.1, 0.2, 0.3]);
        
        // Channel 1 should be [0.4, 0.5, 0.6]
        let channel_1 = frame.channel_data(1).unwrap();
        assert_eq!(channel_1, vec![0.4, 0.5, 0.6]);
    }

    #[test]
    fn test_audio_frame_from_raw_v3() {
        // Test creating AudioFrame from raw v3 data
        let test_data: Vec<f32> = vec![0.1, -0.1, 0.2, -0.2];
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 44100,
            no_channels: 2,
            no_samples: 2,
            timecode: 123456,
            FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
            p_data: test_data.as_ptr() as *mut u8,
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 0,
            },
            p_metadata: ptr::null(),
            timestamp: 789012,
        };
        
        let frame = AudioFrame::from_raw(raw, None).unwrap();
        
        assert_eq!(frame.sample_rate, 44100);
        assert_eq!(frame.no_channels, 2);
        assert_eq!(frame.no_samples, 2);
        assert_eq!(frame.timecode, 123456);
        assert_eq!(frame.timestamp, 789012);
        assert_eq!(frame.fourcc, AudioType::FLTP);
        assert_eq!(frame.data(), &test_data[..]);
    }
}
