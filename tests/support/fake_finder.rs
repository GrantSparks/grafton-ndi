//! Valid native descriptor fixtures for public Finder resource-limit tests.
use super::*;

// Advanced source metadata ABI: name, URL and metadata, in that order.
#[repr(C)]
pub struct ExtendedDescriptor {
    name: *const c_char,
    address: *const c_char,
    metadata: *const c_char,
}

struct Snapshot {
    generation: usize,
    scope: String,
    text: Vec<u8>,
    plain: Vec<NDIlib_source_t>,
    extended: Vec<ExtendedDescriptor>,
}

impl Snapshot {
    fn refresh(&mut self) {
        self.generation += 1;
        let scenario = std::env::var("GRAFTON_FAKE_FINDER_CASE").unwrap();
        if scenario.starts_with("selection") {
            let alternate = self.scope == "Remote";
            let (first, second) = if alternate {
                (c"192.0.2.20:5961", c"192.0.2.20:5962")
            } else {
                (c"192.0.2.10:5961", c"192.0.2.10:5962")
            };
            let source = |name: &'static CStr, address: &'static CStr| NDIlib_source_t {
                p_ndi_name: name.as_ptr(),
                __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                    p_url_address: address.as_ptr(),
                },
            };
            self.plain = vec![source(c"OTHER (Unrelated)", c"198.51.100.1:5960")];
            if self.generation >= 2 {
                self.plain.extend([
                    source(c"STUDIO (Camera 1)", first),
                    source(c"STUDIO (Program)", first),
                    source(c"STUDIO (Program)", second),
                ]);
            }
            if self.generation >= 3 {
                self.plain.push(source(c"STUDIO (Camera 2)", second));
            }
            if scenario == "selection-reverse" {
                self.plain.reverse();
            }
            return;
        }
        let (count, len, byte) = match scenario.as_str() {
            "count" => (16_385, 4, b'x'),
            "many-short" => (16_384, 8, b'x'),
            "bytes" | "metadata" => (4_096, 8_192, b'x'),
            "lossy" => (64, 128 * 1024, 0xff),
            "unterminated" => (16, 4 * 1024 * 1024, b'x'),
            "retained-bytes" => (1, 64 * 1024, b'x'),
            _ => (1, 16, b'x'),
        };
        self.text = vec![byte; len];
        if scenario.starts_with("retained") {
            let label = self.generation.to_string();
            self.text[..label.len()].copy_from_slice(label.as_bytes());
        }
        if scenario != "unterminated" {
            self.text.push(0);
        }
        let pointer = self.text.as_ptr().cast();
        self.plain = (0..count)
            .map(|_| NDIlib_source_t {
                p_ndi_name: pointer,
                ..Default::default()
            })
            .collect();
        self.extended = (0..count)
            .map(|_| ExtendedDescriptor {
                name: if scenario == "metadata" {
                    c"Camera".as_ptr()
                } else {
                    pointer
                },
                address: std::ptr::null(),
                metadata: if scenario == "metadata" {
                    pointer
                } else {
                    c"".as_ptr()
                },
            })
            .collect();
    }
}

export! {
    fn NDIlib_find_create_v2(p_create_settings: *const NDIlib_find_create_t) -> NDIlib_find_instance_t {
        let settings = unsafe { &*p_create_settings };
        let text = |pointer: *const c_char| if pointer.is_null() { String::new() } else { unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned() };
        let scope = text(settings.p_groups);
        if std::env::var("GRAFTON_FAKE_FINDER_CASE").unwrap().starts_with("selection") {
            assert!(!settings.show_local_sources, "local-source policy was replaced");
            assert!(scope == "Studio" || scope == "Remote", "groups were replaced");
            assert_eq!(text(settings.p_extra_ips), "192.0.2.10,192.0.2.20", "IP hints were replaced");
        }
        Box::into_raw(Box::new(Snapshot { generation: 0, scope, text: Vec::new(), plain: Vec::new(), extended: Vec::new() })).cast()
    }
    fn NDIlib_find_destroy(p_instance: NDIlib_find_instance_t) {
        unsafe { drop(Box::from_raw(p_instance.cast::<Snapshot>())) };
    }
    fn NDIlib_find_get_current_sources(p_instance: NDIlib_find_instance_t, p_no_sources: *mut u32) -> *const NDIlib_source_t {
        let snapshot = unsafe { &mut *p_instance.cast::<Snapshot>() };
        snapshot.refresh();
        unsafe { *p_no_sources = snapshot.plain.len() as u32 };
        snapshot.plain.as_ptr()
    }
    fn NDIlib_find_wait_for_sources(p_instance: NDIlib_find_instance_t, timeout_in_ms: u32) -> bool {
        if std::env::var("GRAFTON_FAKE_FINDER_CASE").unwrap().starts_with("selection") {
            std::thread::sleep(std::time::Duration::from_millis(u64::from(timeout_in_ms.min(20))));
        }
        true
    }
}

#[no_mangle]
pub unsafe extern "C" fn NDIlib_find_get_current_sources_v2(
    instance: NDIlib_find_instance_t,
    count: *mut u32,
) -> *const ExtendedDescriptor {
    let snapshot = unsafe { &mut *instance.cast::<Snapshot>() };
    snapshot.refresh();
    unsafe { *count = snapshot.extended.len() as u32 };
    snapshot.extended.as_ptr()
}
