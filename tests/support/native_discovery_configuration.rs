//! SDK behavior that justifies refusing explicitly empty Rust overrides.

#[rustfmt::skip]
#[path = "../../src/sys/bindings.rs"]
#[allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code,
    unused_imports,
    clippy::all,
    clippy::pedantic
)]
mod bindings;

use super::{await_listed, common, discovery_server, eventually, Server};
use bindings::*;
use grafton_ndi::{
    discovery_server::{ListenerOptions, SendListener},
    Ndi,
};
use libloading::Library;
use std::{
    ffi::{CStr, CString},
    ptr, slice,
};

const CONFIG_CHILD: &str = "GRAFTON_DISCOVERY_EMPTY_SETTINGS_CHILD";

fn configured_child(test: &str, config: &str) {
    let directory = common::FixtureDir::create("ndi-empty-settings");
    std::fs::write(directory.join("ndi-config.v1.json"), config).unwrap();
    common::TestChild::new(test)
        .env(CONFIG_CHILD, "1")
        .env("NDI_CONFIG_DIR", directory.path())
        .run();
}

struct Native<'sdk, T> {
    handle: *mut T,
    destroy: libloading::Symbol<'sdk, unsafe extern "C" fn(*mut T)>,
}

impl<'sdk, T> Native<'sdk, T> {
    /// The symbols must match `S` and `T`. All pointers the SDK retains from
    /// `settings` stay live until destruction.
    unsafe fn create<S>(sdk: &'sdk Library, create: &[u8], destroy: &[u8], settings: &S) -> Self {
        let create =
            unsafe { sdk.get::<unsafe extern "C" fn(*const S) -> *mut T>(create) }.unwrap();
        let destroy = unsafe { sdk.get(destroy) }.unwrap();
        let handle = unsafe { create(settings) };
        assert!(!handle.is_null(), "the SDK refused the configured setting");
        Self { handle, destroy }
    }
}

impl<T> Drop for Native<'_, T> {
    fn drop(&mut self) {
        // SAFETY: this is the unique owner of the matching native handle.
        unsafe { (self.destroy)(self.handle) };
    }
}

fn library(ndi: &Ndi) -> Library {
    // SAFETY: use the already initialized and pinned test runtime.
    unsafe { Library::new(ndi.info().library_path().unwrap()) }.unwrap()
}

fn native_sender<'sdk>(
    sdk: &'sdk Library,
    label: &'sdk CStr,
    group: Option<&'static CStr>,
) -> Native<'sdk, NDIlib_send_instance_type> {
    // SAFETY: the symbols match the settings and handle types. The caller
    // keeps the source name live and only static group strings are passed.
    unsafe {
        Native::create(
            sdk,
            b"NDIlib_send_create",
            b"NDIlib_send_destroy",
            &NDIlib_send_create_t {
                p_ndi_name: label.as_ptr(),
                p_groups: group.map_or(ptr::null(), CStr::as_ptr),
                clock_audio: false,
                clock_video: false,
            },
        )
    }
}

fn sees_configured_sources(
    sdk: &Library,
    group: Option<&'static CStr>,
    extra_ips: Option<&'static CStr>,
    labels: &[CString],
    hidden: &[CString],
) {
    // SAFETY: the symbols match the settings and handle types, and the
    // configuration strings are static.
    unsafe {
        let finder: Native<'_, NDIlib_find_instance_type> = Native::create(
            sdk,
            b"NDIlib_find_create_v2",
            b"NDIlib_find_destroy",
            &NDIlib_find_create_t {
                show_local_sources: true,
                p_groups: group.map_or(ptr::null(), CStr::as_ptr),
                p_extra_ips: extra_ips.map_or(ptr::null(), CStr::as_ptr),
            },
        );
        let current_sources = sdk
            .get::<unsafe extern "C" fn(NDIlib_find_instance_t, *mut u32) -> *const NDIlib_source_t>(
                b"NDIlib_find_get_current_sources",
            )
            .unwrap();
        let wait = sdk
            .get::<unsafe extern "C" fn(NDIlib_find_instance_t, u32) -> bool>(
                b"NDIlib_find_wait_for_sources",
            )
            .unwrap();
        eventually(
            "all senders in the SDK's configured group or address",
            || {
                wait(finder.handle, 100);
                let mut count = 0;
                let sources = current_sources(finder.handle, &mut count);
                if count == 0 {
                    return None;
                }
                assert!(!sources.is_null());
                // The snapshot is readable until the next call on this finder.
                let sources = slice::from_raw_parts(sources, count as usize);
                let visible = |label: &CString| {
                    sources.iter().any(|source| {
                        !source.p_ndi_name.is_null()
                            && CStr::from_ptr(source.p_ndi_name)
                                .to_bytes()
                                .strip_suffix(b")")
                                .and_then(|name| name.strip_suffix(label.to_bytes()))
                                .is_some_and(|name| name.ends_with(b" ("))
                    })
                };
                if !labels.iter().all(visible) {
                    return None;
                }
                assert!(hidden.iter().all(|label| !visible(label)));
                Some(())
            },
        );
    }
}

#[test]
fn sdk_empty_lists_inherit_configured_groups_and_extra_addresses() {
    if std::env::var_os(CONFIG_CHILD).is_none() {
        configured_child(
            "native_discovery_configuration::sdk_empty_lists_inherit_configured_groups_and_extra_addresses",
            r#"{"ndi":{"networks":{"ips":"127.0.0.1"},"groups":{"send":"grafton-cfg","recv":"grafton-cfg"}}}"#,
        );
        return;
    }
    let Some(ndi) = common::ndi() else { return };
    let sdk = library(&ndi);
    let labels = [
        "inherited group from NULL",
        "inherited group from empty",
        "direct address inherited",
    ]
    .map(|label| CString::new(common::unique_name(label)).unwrap());
    let _null_group = native_sender(&sdk, &labels[0], None);
    let _empty_group = native_sender(&sdk, &labels[1], Some(c""));
    let _explicit_group = native_sender(&sdk, &labels[2], Some(c"public"));
    for inherited in [None, Some(c"")] {
        sees_configured_sources(
            &sdk,
            inherited,
            Some(c"127.0.0.1"),
            &labels[..2],
            &labels[2..],
        );
        sees_configured_sources(&sdk, Some(c"public"), inherited, &labels[2..], &labels[..2]);
    }
}

#[test]
fn sdk_empty_endpoints_inherit_the_configured_discovery_server() {
    if std::env::var_os(CONFIG_CHILD).is_none() {
        let Some((_ndi, binary)) = discovery_server() else {
            return;
        };
        let server = Server::start(&binary);
        configured_child(
            "native_discovery_configuration::sdk_empty_endpoints_inherit_the_configured_discovery_server",
            &format!(
                r#"{{"ndi":{{"networks":{{"discovery":"{}"}}}}}}"#,
                server.endpoint()
            ),
        );
        return;
    }
    let Some(ndi) = common::ndi() else { return };
    let sdk = library(&ndi);
    let mut listener = SendListener::new(&ndi, &ListenerOptions::new()).unwrap();

    for (suffix, url) in [("null", ptr::null()), ("empty", c"".as_ptr())] {
        let label = CString::new(common::unique_name(suffix)).unwrap();
        let sender = native_sender(&sdk, &label, None);
        // SAFETY: the symbols match the settings and handle types, and the
        // URL is null or points to a static C string.
        let (advertiser, raw_listener): (
            Native<'_, NDIlib_send_advertiser_instance_type>,
            Native<'_, NDIlib_send_listener_instance_type>,
        ) = unsafe {
            (
                Native::create(
                    &sdk,
                    b"NDIlib_send_advertiser_create",
                    b"NDIlib_send_advertiser_destroy",
                    &NDIlib_send_advertiser_create_t { p_url_address: url },
                ),
                Native::create(
                    &sdk,
                    b"NDIlib_send_listener_create",
                    b"NDIlib_send_listener_destroy",
                    &NDIlib_send_listener_create_t { p_url_address: url },
                ),
            )
        };
        // SAFETY: pass this SDK's live sender, advertiser and listener to
        // their declared entry points.
        unsafe {
            let add_sender = sdk
                .get::<unsafe extern "C" fn(
                    NDIlib_send_advertiser_instance_t,
                    NDIlib_send_instance_t,
                    bool,
                ) -> bool>(b"NDIlib_send_advertiser_add_sender")
                .unwrap();
            assert!(add_sender(advertiser.handle, sender.handle, false));
            let connected = sdk
                .get::<unsafe extern "C" fn(NDIlib_send_listener_instance_t) -> bool>(
                    b"NDIlib_send_listener_is_connected",
                )
                .unwrap();
            eventually(
                "the SDK listener to connect to the configured server",
                || {
                    if connected(raw_listener.handle) {
                        Some(())
                    } else {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                        None
                    }
                },
            );
            let source_name = sdk
                .get::<unsafe extern "C" fn(NDIlib_send_instance_t) -> *const NDIlib_source_t>(
                    b"NDIlib_send_get_source_name",
                )
                .unwrap()(sender.handle);
            assert!(!source_name.is_null());
            let source_name = CStr::from_ptr((*source_name).p_ndi_name).to_str().unwrap();
            await_listed(
                &mut listener,
                "the SDK sender to reach the configured server",
                source_name,
            );
        }
    }
}
