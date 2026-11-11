//! One typed dispatch boundary for the selected, process-retained SDK implementation.

use crate::{
    capabilities::{
        Capabilities, CapabilityStatus, CapabilityUnavailable, RuntimeInfo, SdkEdition, SdkLinkage,
        SdkVersion,
    },
    ndi_lib::raw::*,
    runtime::RuntimeOptions,
    Error, Result,
};
use std::{
    ffi::{c_void, CStr},
    path::{Path, PathBuf},
    sync::{Condvar, Mutex, OnceLock},
    thread::ThreadId,
};

#[cfg(not(ndi_link_static))]
use std::ffi::OsString;

static SELECTED: OnceLock<CoreApi> = OnceLock::new();
static LOADING: Mutex<Option<ThreadId>> = Mutex::new(None);
static LOAD_CHANGED: Condvar = Condvar::new();

pub(crate) struct CoreApi {
    pub(crate) baseline: Baseline,
    provider: Provider,
    pub(crate) info: RuntimeInfo,
    pub(crate) capabilities: Capabilities,
}

#[cfg(not(ndi_link_static))]
struct Provider {
    library: libloading::Library,
    identity: usize,
    path: PathBuf,
}
#[cfg(ndi_link_static)]
struct Provider;

pub(crate) fn selected() -> &'static CoreApi {
    SELECTED
        .get()
        .expect("native SDK call requires a successfully selected NDI runtime")
}

/// Failure is retryable; successful selection is immutable for the process lifetime.
pub(crate) fn select(options: &RuntimeOptions) -> Result<&'static CoreApi> {
    loop {
        if let Some(api) = SELECTED.get() {
            check_selection(api, options)?;
            return Ok(api);
        }
        let mut loading = LOADING.lock().unwrap_or_else(|e| e.into_inner());
        if SELECTED.get().is_some() {
            continue;
        }
        if let Some(owner) = *loading {
            if owner == std::thread::current().id() {
                return Err(Error::RuntimeConflict(
                    "native module loading re-entered NDI runtime selection".into(),
                ));
            }
            loading = LOAD_CHANGED
                .wait(loading)
                .unwrap_or_else(|e| e.into_inner());
            drop(loading);
            continue;
        }
        *loading = Some(std::thread::current().id());
        drop(loading);
        // Native module constructors and the version query run outside our mutex.
        let loaded = CoreApi::load(options);
        let mut loading = LOADING.lock().unwrap_or_else(|e| e.into_inner());
        *loading = None;
        let result = match loaded {
            Ok(api) => {
                // Only this loading owner can publish a selection.
                let _ = SELECTED.set(api);
                Ok(selected())
            }
            Err(error) => Err(error),
        };
        LOAD_CHANGED.notify_all();
        return result;
    }
}

fn check_selection(api: &CoreApi, options: &RuntimeOptions) -> Result<()> {
    if options
        .edition
        .is_some_and(|edition| edition != api.info.edition)
    {
        return Err(Error::RuntimeConflict(
            "a different SDK edition has already been selected".into(),
        ));
    }
    if let Some(requested) = &options.library_path {
        if !requested.is_absolute() {
            return Err(Error::InvalidConfiguration(
                "an explicit NDI library path must be absolute".into(),
            ));
        }
        let matches = api
            .info
            .library_path
            .as_ref()
            .is_some_and(|actual| same_path(requested, actual));
        if !matches {
            return Err(Error::RuntimeConflict("a different SDK module has already been selected; runtime switching is not supported".into()));
        }
    }
    Ok(())
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

pub(crate) fn default_edition() -> SdkEdition {
    if option_env!("GRAFTON_NDI_EDITION") == Some("advanced") {
        SdkEdition::Advanced
    } else {
        SdkEdition::Standard
    }
}

#[cfg(not(ndi_link_static))]
fn module_name(edition: SdkEdition) -> &'static str {
    // The Advanced umbrella retains Standard name macros: actual artifacts own this map.
    if edition == default_edition() {
        if let Some(name) =
            option_env!("GRAFTON_NDI_CORE_LIBRARY_NAME").or(option_env!("GRAFTON_NDI_LIBRARY_NAME"))
        {
            return name;
        }
    }
    match edition {
        SdkEdition::Standard => {
            #[cfg(all(windows, target_pointer_width = "64"))]
            {
                "Processing.NDI.Lib.x64.dll"
            }
            #[cfg(all(windows, target_pointer_width = "32"))]
            {
                "Processing.NDI.Lib.x86.dll"
            }
            #[cfg(target_os = "macos")]
            {
                "libndi.dylib"
            }
            #[cfg(target_os = "android")]
            {
                "libndi.so"
            }
            #[cfg(target_os = "linux")]
            {
                "libndi.so.6"
            }
        }
        SdkEdition::Advanced => {
            #[cfg(all(windows, target_pointer_width = "64"))]
            {
                "Processing.NDI.Lib.Advanced.x64.dll"
            }
            #[cfg(all(windows, target_pointer_width = "32"))]
            {
                "Processing.NDI.Lib.Advanced.x86.dll"
            }
            #[cfg(target_os = "macos")]
            {
                "libndi_advanced.dylib"
            }
            #[cfg(target_os = "android")]
            {
                "libndi_advanced.so"
            }
            #[cfg(target_os = "linux")]
            {
                "libndi_advanced.so.6"
            }
        }
    }
}

impl CoreApi {
    fn load(options: &RuntimeOptions) -> Result<Self> {
        let edition = options.edition.unwrap_or_else(default_edition);
        let provider = Provider::open(options, edition)?;
        let edition_verified = provider.validate_edition(edition)?;
        let baseline = Baseline::load(&provider)?;
        let raw_version = unsafe {
            let ptr = (baseline.NDIlib_version)();
            if ptr.is_null() {
                return Err(Error::NullPointer("NDIlib_version".into()));
            }
            CStr::from_ptr(ptr)
                .to_str()
                .map_err(|e| Error::InvalidUtf8(e.to_string()))?
                .to_owned()
        };
        let version = SdkVersion::parse(&raw_version);
        let receive_delay = provider.group("receive_delay")? == CapabilityStatus::Available;
        let timestamp = provider.group("sdk_timestamp")? == CapabilityStatus::Available;
        let groups = provider.capability_groups(edition)?;
        let info = RuntimeInfo {
            edition,
            edition_verified,
            linkage: if cfg!(ndi_link_static) {
                SdkLinkage::Static
            } else {
                SdkLinkage::Shared
            },
            library_path: provider.path(),
            raw_version,
            version,
        };
        Ok(Self {
            baseline,
            provider,
            info,
            capabilities: Capabilities {
                version,
                receive_delay,
                timestamp,
                groups,
            },
        })
    }

    /// `T` must be the exact native function-pointer type for this symbol.
    pub(crate) unsafe fn optional<T: Copy>(&self, name: &'static [u8]) -> Result<Option<T>> {
        self.provider.optional(name)
    }
}

impl Provider {
    fn group(&self, id: &str) -> Result<CapabilityStatus> {
        let (names, _) = crate::capabilities::requirements(id).ok_or_else(|| {
            Error::InvalidConfiguration(format!("missing internal SDK capability descriptor {id}"))
        })?;
        for name in names {
            let name = format!("{name}\0");
            if self.address(name.as_bytes())?.is_none() {
                return Ok(CapabilityStatus::Unavailable(
                    CapabilityUnavailable::SymbolUnavailable,
                ));
            }
        }
        Ok(CapabilityStatus::Available)
    }

    fn capability_groups(&self, edition: SdkEdition) -> Result<[CapabilityStatus; 7]> {
        let advanced = |id: &str| -> Result<CapabilityStatus> {
            if !cfg!(all(feature = "advanced", ndi_has_advanced_header)) {
                return Ok(CapabilityStatus::Unavailable(
                    CapabilityUnavailable::BuildAbiUnavailable,
                ));
            }
            if edition != SdkEdition::Advanced {
                return Ok(CapabilityStatus::Unavailable(
                    CapabilityUnavailable::WrongEdition,
                ));
            }
            self.group(id)
        };
        Ok([
            self.group("routing")?,
            self.group("send_advertiser")?,
            self.group("recv_advertiser")?,
            self.group("send_listener")?,
            self.group("recv_listener")?,
            advanced("send_create_v2")?,
            advanced("recv_create_v4")?,
        ])
    }

    /// `T` must be an ABI-correct function pointer, not arbitrary pointer-sized data.
    unsafe fn optional<T: Copy>(&self, name: &'static [u8]) -> Result<Option<T>> {
        if std::mem::size_of::<T>() != std::mem::size_of::<*const c_void>() {
            return Err(Error::InvalidConfiguration(
                "native symbol type must be a function pointer".into(),
            ));
        }
        self.address(name)?
            .map(|address| {
                // Safety: caller guarantees exact function-pointer type; size checked above.
                Ok(std::mem::transmute_copy::<*const c_void, T>(&address))
            })
            .transpose()
    }

    unsafe fn required<T: Copy>(&self, name: &'static [u8]) -> Result<T> {
        self.optional(name)?.ok_or_else(|| {
            Error::RuntimeLoadFailed(format!(
                "selected NDI runtime is missing required baseline symbol {}",
                symbol_label(name)
            ))
        })
    }
}

fn symbol_label(name: &[u8]) -> String {
    String::from_utf8_lossy(name.strip_suffix(&[0]).unwrap_or(name)).into_owned()
}

/// Resolve a capability operation from the already selected core module.
/// The caller must supply the exact SDK C function-pointer signature.
pub(crate) unsafe fn require_optional<T: Copy>(name: &'static [u8], capability: &str) -> Result<T> {
    selected()
        .optional(name)?
        .ok_or_else(|| Error::UnsupportedCapability {
            capability: capability.into(),
            reason: format!("native symbol {} is unavailable", symbol_label(name)),
        })
}

#[cfg(not(ndi_link_static))]
impl Provider {
    fn open(options: &RuntimeOptions, edition: SdkEdition) -> Result<Self> {
        let mut exact_path = options.library_path.clone();
        let requested: OsString = if let Some(path) = &options.library_path {
            if !path.is_absolute() {
                return Err(Error::InvalidConfiguration(
                    "an explicit NDI library path must be absolute".into(),
                ));
            }
            path.as_os_str().to_owned()
        } else if let Some(directory) =
            std::env::var_os("NDI_RUNTIME_DIR_V6").filter(|d| !d.is_empty())
        {
            let path = PathBuf::from(directory).join(module_name(edition));
            exact_path = Some(path.clone());
            path.into_os_string()
        } else {
            #[cfg(target_os = "macos")]
            {
                format!("@rpath/{}", module_name(edition)).into()
            }
            #[cfg(not(target_os = "macos"))]
            {
                module_name(edition).into()
            }
        };
        let open =
            |name: &OsString| -> std::result::Result<libloading::Library, libloading::Error> {
                unsafe {
                    #[cfg(unix)]
                    {
                        libloading::os::unix::Library::open(
                            Some(name),
                            libloading::os::unix::RTLD_NOW | libloading::os::unix::RTLD_LOCAL,
                        )
                        .map(Into::into)
                    }
                    #[cfg(windows)]
                    {
                        libloading::os::windows::Library::load_with_flags(name, 0).map(Into::into)
                    }
                }
            };
        let loaded = open(&requested);
        // App bundles normally provide @rpath. Preserve the system loader's
        // basename search for callers using an installed runtime or DYLD paths.
        #[cfg(target_os = "macos")]
        let loaded = loaded.or_else(|first| {
            if exact_path.is_some() {
                return Err(first);
            }
            open(&OsString::from(module_name(edition)))
        });
        let library = loaded.map_err(|e| {
            Error::RuntimeLoadFailed(format!(
                "cannot load {}: {e}",
                Path::new(&requested).display()
            ))
        })?;
        let anchor = unsafe {
            library.get::<unsafe extern "C" fn() -> *const std::ffi::c_char>(b"NDIlib_version\0")
        }
        .map_err(|e| Error::RuntimeLoadFailed(format!("NDIlib_version unavailable: {e}")))?;
        let (identity, path) = symbol_identity(*anchor as *const () as *const c_void)?;
        let path = path.canonicalize().unwrap_or(path);
        if let Some(explicit) = &exact_path {
            if !same_path(explicit, &path) {
                return Err(Error::RuntimeConflict(format!(
                    "requested {} but loader selected {}",
                    explicit.display(),
                    path.display()
                )));
            }
        }
        Ok(Self {
            library,
            identity,
            path,
        })
    }

    fn path(&self) -> Option<PathBuf> {
        Some(self.path.clone())
    }

    fn validate_edition(&self, requested: SdkEdition) -> Result<bool> {
        // The current Advanced ABI has these two exclusive markers. Do not make
        // their historical availability a new requirement for the legacy API.
        let send = self.address(b"NDIlib_send_create_v2\0")?.is_some();
        let recv = self.address(b"NDIlib_recv_create_v4\0")?.is_some();
        if send && recv {
            if requested != SdkEdition::Advanced {
                return Err(Error::RuntimeConflict(
                    "requested Standard SDK edition but selected module exposes Advanced ABI"
                        .into(),
                ));
            }
            return Ok(true);
        }
        if requested == SdkEdition::Standard && !send && !recv {
            return Ok(true);
        }
        let filename = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let named_advanced = filename.starts_with("libndi_advanced.so")
            || (filename.starts_with("libndi_advanced") && filename.ends_with(".dylib"))
            || matches!(
                filename.as_str(),
                "processing.ndi.lib.advanced.x64.dll" | "processing.ndi.lib.advanced.x86.dll"
            );
        if requested == SdkEdition::Advanced && named_advanced {
            // Selection label plus filename is enough to preserve old core use,
            // but is not evidence for File/other edition-dependent operations.
            return Ok(false);
        }
        Err(Error::RuntimeConflict(
            "requested SDK edition conflicts with selected module identity".into(),
        ))
    }

    fn address(&self, name: &[u8]) -> Result<Option<*const c_void>> {
        CStr::from_bytes_with_nul(name).map_err(|_| {
            Error::InvalidConfiguration(
                "native symbol names must have exactly one final NUL".into(),
            )
        })?;
        let symbol = match unsafe { self.library.get::<*const c_void>(name) } {
            Ok(symbol) => *symbol,
            Err(_) => return Ok(None),
        };
        if symbol.is_null() {
            return Ok(None);
        }
        let (identity, _) = symbol_identity(symbol)?;
        if identity != self.identity {
            return Err(Error::RuntimeConflict(format!(
                "{} resolved outside the selected NDI module",
                symbol_label(name)
            )));
        }
        Ok(Some(symbol))
    }
}

#[cfg(ndi_link_static)]
impl Provider {
    fn open(options: &RuntimeOptions, edition: SdkEdition) -> Result<Self> {
        if options.library_path.is_some() || edition != default_edition() {
            return Err(Error::RuntimeConflict("a statically linked NDI implementation cannot select a different module or edition".into()));
        }
        Ok(Self)
    }
    fn path(&self) -> Option<PathBuf> {
        None
    }
    fn validate_edition(&self, _requested: SdkEdition) -> Result<bool> {
        // The build selected and verified a single edition-specific archive.
        Ok(true)
    }
    fn address(&self, name: &[u8]) -> Result<Option<*const c_void>> {
        CStr::from_bytes_with_nul(name).map_err(|_| {
            Error::InvalidConfiguration(
                "native symbol names must have exactly one final NUL".into(),
            )
        })?;
        Ok(include!(concat!(env!("OUT_DIR"), "/ndi_static_symbols.rs")))
    }
}

#[cfg(all(unix, not(ndi_link_static)))]
fn symbol_identity(address: *const c_void) -> Result<(usize, PathBuf)> {
    use std::os::unix::ffi::OsStrExt;
    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
    let success = unsafe { libc::dladdr(address, info.as_mut_ptr()) };
    if success == 0 {
        return Err(Error::RuntimeLoadFailed(
            "cannot identify native symbol's loaded module".into(),
        ));
    }
    let info = unsafe { info.assume_init() };
    if info.dli_fname.is_null() || info.dli_fbase.is_null() {
        return Err(Error::RuntimeLoadFailed(
            "native module identity is incomplete".into(),
        ));
    }
    let filename = unsafe { CStr::from_ptr(info.dli_fname) };
    Ok((
        info.dli_fbase as usize,
        PathBuf::from(std::ffi::OsStr::from_bytes(filename.to_bytes())),
    ))
}

#[cfg(all(windows, not(ndi_link_static)))]
fn symbol_identity(address: *const c_void) -> Result<(usize, PathBuf)> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{
        Foundation::FreeLibrary,
        System::LibraryLoader::{
            GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        },
    };
    let mut module = std::ptr::null_mut();
    if unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            address.cast(),
            &mut module,
        )
    } == 0
    {
        return Err(Error::RuntimeLoadFailed(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    let result = (|| {
        let mut size = 256usize;
        loop {
            let mut buffer = vec![0u16; size];
            let len =
                unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), size as u32) } as usize;
            if len == 0 {
                return Err(Error::RuntimeLoadFailed(
                    std::io::Error::last_os_error().to_string(),
                ));
            }
            if len < size {
                return Ok((
                    module as usize,
                    PathBuf::from(OsString::from_wide(&buffer[..len])),
                ));
            }
            size = size.checked_mul(2).filter(|s| *s <= 65536).ok_or_else(|| {
                Error::RuntimeLoadFailed(
                    "native module path exceeds supported Windows path length".into(),
                )
            })?;
        }
    })();
    unsafe {
        FreeLibrary(module);
    }
    result
}

macro_rules! baseline_functions {
    ($(fn $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $ret:ty;)*) => {
        #[allow(non_snake_case)]
        pub(crate) struct Baseline { $(pub(crate) $name: unsafe extern "C" fn($($ty),*) -> $ret,)* }
        impl Baseline {
            fn load(provider: &Provider) -> Result<Self> {
                Ok(Self { $($name: unsafe { provider.required(concat!(stringify!($name), "\0").as_bytes())? },)* })
            }
        }
        #[allow(non_snake_case)]
        pub(crate) mod wrappers {
            use super::*;
            $(pub(crate) unsafe extern "C" fn $name($($arg: $ty),*) -> $ret { (selected().baseline.$name)($($arg),*) })*
        }
    }
}

// This baseline is the union of symbols referenced by the 2.0.1 wrappers, including
// PTZ functions passed as callbacks. New APIs must use optional capability groups.
baseline_functions! {
    fn NDIlib_initialize() -> bool;
    fn NDIlib_destroy() -> ();
    fn NDIlib_version() -> * const :: std :: os :: raw :: c_char;
    fn NDIlib_is_supported_CPU() -> bool;
    fn NDIlib_find_create_v2(p_create_settings : * const NDIlib_find_create_t) -> NDIlib_find_instance_t;
    fn NDIlib_find_destroy(p_instance : NDIlib_find_instance_t) -> ();
    fn NDIlib_find_get_current_sources(p_instance : NDIlib_find_instance_t , p_no_sources : * mut u32) -> * const NDIlib_source_t;
    fn NDIlib_find_wait_for_sources(p_instance : NDIlib_find_instance_t , timeout_in_ms : u32) -> bool;
    fn NDIlib_recv_create_v3(p_create_settings : * const NDIlib_recv_create_v3_t) -> NDIlib_recv_instance_t;
    fn NDIlib_recv_destroy(p_instance : NDIlib_recv_instance_t) -> ();
    fn NDIlib_recv_connect(p_instance : NDIlib_recv_instance_t , p_src : * const NDIlib_source_t) -> ();
    fn NDIlib_recv_capture_v3(p_instance : NDIlib_recv_instance_t , p_video_data : * mut NDIlib_video_frame_v2_t , p_audio_data : * mut NDIlib_audio_frame_v3_t , p_metadata : * mut NDIlib_metadata_frame_t , timeout_in_ms : u32) -> NDIlib_frame_type_e;
    fn NDIlib_recv_free_video_v2(p_instance : NDIlib_recv_instance_t , p_video_data : * const NDIlib_video_frame_v2_t) -> ();
    fn NDIlib_recv_free_audio_v3(p_instance : NDIlib_recv_instance_t , p_audio_data : * const NDIlib_audio_frame_v3_t) -> ();
    fn NDIlib_recv_free_metadata(p_instance : NDIlib_recv_instance_t , p_metadata : * const NDIlib_metadata_frame_t) -> ();
    fn NDIlib_recv_send_metadata(p_instance : NDIlib_recv_instance_t , p_metadata : * const NDIlib_metadata_frame_t) -> bool;
    fn NDIlib_recv_set_tally(p_instance : NDIlib_recv_instance_t , p_tally : * const NDIlib_tally_t) -> bool;
    fn NDIlib_recv_get_performance(p_instance : NDIlib_recv_instance_t , p_total : * mut NDIlib_recv_performance_t , p_dropped : * mut NDIlib_recv_performance_t) -> ();
    fn NDIlib_recv_get_queue(p_instance : NDIlib_recv_instance_t , p_total : * mut NDIlib_recv_queue_t) -> ();
    fn NDIlib_recv_get_no_connections(p_instance : NDIlib_recv_instance_t) -> :: std :: os :: raw :: c_int;
    fn NDIlib_recv_ptz_is_supported(p_instance : NDIlib_recv_instance_t) -> bool;
    fn NDIlib_recv_ptz_zoom(p_instance : NDIlib_recv_instance_t , zoom_value : f32) -> bool;
    fn NDIlib_recv_ptz_zoom_speed(p_instance : NDIlib_recv_instance_t , zoom_speed : f32) -> bool;
    fn NDIlib_recv_ptz_pan_tilt(p_instance : NDIlib_recv_instance_t , pan_value : f32 , tilt_value : f32) -> bool;
    fn NDIlib_recv_ptz_pan_tilt_speed(p_instance : NDIlib_recv_instance_t , pan_speed : f32 , tilt_speed : f32) -> bool;
    fn NDIlib_recv_ptz_store_preset(p_instance : NDIlib_recv_instance_t , preset_no : :: std :: os :: raw :: c_int) -> bool;
    fn NDIlib_recv_ptz_recall_preset(p_instance : NDIlib_recv_instance_t , preset_no : :: std :: os :: raw :: c_int , speed : f32) -> bool;
    fn NDIlib_recv_ptz_auto_focus(p_instance : NDIlib_recv_instance_t) -> bool;
    fn NDIlib_recv_ptz_focus(p_instance : NDIlib_recv_instance_t , focus_value : f32) -> bool;
    fn NDIlib_recv_ptz_focus_speed(p_instance : NDIlib_recv_instance_t , focus_speed : f32) -> bool;
    fn NDIlib_recv_ptz_white_balance_auto(p_instance : NDIlib_recv_instance_t) -> bool;
    fn NDIlib_recv_ptz_white_balance_indoor(p_instance : NDIlib_recv_instance_t) -> bool;
    fn NDIlib_recv_ptz_white_balance_outdoor(p_instance : NDIlib_recv_instance_t) -> bool;
    fn NDIlib_recv_ptz_white_balance_oneshot(p_instance : NDIlib_recv_instance_t) -> bool;
    fn NDIlib_recv_ptz_white_balance_manual(p_instance : NDIlib_recv_instance_t , red : f32 , blue : f32) -> bool;
    fn NDIlib_recv_ptz_exposure_auto(p_instance : NDIlib_recv_instance_t) -> bool;
    fn NDIlib_recv_ptz_exposure_manual(p_instance : NDIlib_recv_instance_t , exposure_level : f32) -> bool;
    fn NDIlib_recv_ptz_exposure_manual_v2(p_instance : NDIlib_recv_instance_t , iris : f32 , gain : f32 , shutter_speed : f32) -> bool;
    fn NDIlib_send_create(p_create_settings : * const NDIlib_send_create_t) -> NDIlib_send_instance_t;
    fn NDIlib_send_destroy(p_instance : NDIlib_send_instance_t) -> ();
    fn NDIlib_send_send_video_v2(p_instance : NDIlib_send_instance_t , p_video_data : * const NDIlib_video_frame_v2_t) -> ();
    fn NDIlib_send_send_video_async_v2(p_instance : NDIlib_send_instance_t , p_video_data : * const NDIlib_video_frame_v2_t) -> ();
    fn NDIlib_send_send_audio_v3(p_instance : NDIlib_send_instance_t , p_audio_data : * const NDIlib_audio_frame_v3_t) -> ();
    fn NDIlib_send_send_metadata(p_instance : NDIlib_send_instance_t , p_metadata : * const NDIlib_metadata_frame_t) -> ();
    fn NDIlib_send_get_tally(p_instance : NDIlib_send_instance_t , p_tally : * mut NDIlib_tally_t , timeout_in_ms : u32) -> bool;
    fn NDIlib_send_get_no_connections(p_instance : NDIlib_send_instance_t , timeout_in_ms : u32) -> :: std :: os :: raw :: c_int;
    fn NDIlib_send_clear_connection_metadata(p_instance : NDIlib_send_instance_t) -> ();
    fn NDIlib_send_add_connection_metadata(p_instance : NDIlib_send_instance_t , p_metadata : * const NDIlib_metadata_frame_t) -> ();
    fn NDIlib_send_set_failover(p_instance : NDIlib_send_instance_t , p_failover_source : * const NDIlib_source_t) -> ();
    fn NDIlib_send_get_source_name(p_instance : NDIlib_send_instance_t) -> * const NDIlib_source_t;
    fn NDIlib_framesync_create(p_receiver : NDIlib_recv_instance_t) -> NDIlib_framesync_instance_t;
    fn NDIlib_framesync_destroy(p_instance : NDIlib_framesync_instance_t) -> ();
    fn NDIlib_framesync_capture_audio_v2(p_instance : NDIlib_framesync_instance_t , p_audio_data : * mut NDIlib_audio_frame_v3_t , sample_rate : :: std :: os :: raw :: c_int , no_channels : :: std :: os :: raw :: c_int , no_samples : :: std :: os :: raw :: c_int) -> ();
    fn NDIlib_framesync_free_audio_v2(p_instance : NDIlib_framesync_instance_t , p_audio_data : * mut NDIlib_audio_frame_v3_t) -> ();
    fn NDIlib_framesync_audio_queue_depth(p_instance : NDIlib_framesync_instance_t) -> :: std :: os :: raw :: c_int;
    fn NDIlib_framesync_capture_video(p_instance : NDIlib_framesync_instance_t , p_video_data : * mut NDIlib_video_frame_v2_t , field_type : NDIlib_frame_format_type_e) -> ();
    fn NDIlib_framesync_free_video(p_instance : NDIlib_framesync_instance_t , p_video_data : * mut NDIlib_video_frame_v2_t) -> ();
}
