//! Sole owner of native source descriptors passed into SDK requests.
//!
//! Owners keep their existing locks and destroy their native object before
//! dropping this storage. Replacement calls retain both the old descriptor and
//! the replacement until the native call returns. No SDK copying is assumed.

use std::{ptr, sync::Arc};

use crate::{
    sys::bindings::{NDIlib_source_t, NDIlib_source_t__bindgen_ty_1},
    Source,
};

/// One source and its stable descriptor, shared only for an in-progress
/// receiver request so snapshots need not hold the target lock during SDK work.
#[derive(Clone)]
pub(crate) struct RetainedSource(Arc<Retained>);

struct Retained {
    source: Source,
    raw: NDIlib_source_t,
}

// SAFETY: the descriptor's pointers lead only into the immutable native
// strings owned by source. Their buffers do not move, and the enclosing
// allocation keeps the descriptor stable until every native reader is done.
unsafe impl Send for Retained {}

// SAFETY: shared calls and replacement retain the allocation across all SDK
// reads. The SDK reads the descriptor and its strings through const pointers,
// so concurrent readers cannot race with a write.
unsafe impl Sync for Retained {}

impl RetainedSource {
    pub(crate) fn new(source: &Source) -> Self {
        let raw = NDIlib_source_t {
            p_ndi_name: source.name_c_str().as_ptr(),
            __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                p_url_address: source
                    .address()
                    .map_or(ptr::null(), |address| address.as_c_str().as_ptr()),
            },
        };
        Self(Arc::new(Retained {
            source: source.clone(),
            raw,
        }))
    }

    pub(crate) fn current(&self) -> &Source {
        &self.0.source
    }

    pub(crate) fn descriptor(&self) -> &NDIlib_source_t {
        &self.0.raw
    }

    /// Reissue this exact descriptor without allocating or converting it.
    pub(crate) fn reissue<R>(&self, call: impl FnOnce(*const NDIlib_source_t) -> R) -> R {
        call(ptr::from_ref(self.descriptor()))
    }

    /// Install a prepared target only after its native request has returned.
    pub(crate) fn replace<R>(
        &mut self,
        next: Self,
        call: impl FnOnce(*const NDIlib_source_t) -> R,
    ) -> R {
        replace(self, next, |next| next.reissue(call))
    }
}

/// An optional native source, also retaining the last refused router request.
#[derive(Default)]
pub(crate) struct RetainedSources {
    current: Option<RetainedSource>,
    refused: Option<RetainedSource>,
}

impl RetainedSources {
    #[cfg(any(feature = "advanced", test))]
    pub(crate) fn current(&self) -> Option<&Source> {
        self.current.as_ref().map(RetainedSource::current)
    }

    pub(crate) fn replace<R>(
        &mut self,
        next: Option<&Source>,
        call: impl FnOnce(*const NDIlib_source_t) -> R,
    ) -> R {
        let next = next.map(RetainedSource::new);
        let result = replace(&mut self.current, next, |next| reissue(next, call));
        self.refused = None;
        result
    }

    /// A refusal leaves the effective route intact. The SDK makes no promise
    /// about what it retains from a refused request, so retain that request too.
    pub(crate) fn replace_if(
        &mut self,
        next: Option<&Source>,
        call: impl FnOnce(*const NDIlib_source_t) -> crate::sys::table::Called<bool>,
    ) -> crate::Result<()> {
        let next = next.map(RetainedSource::new);
        let changed = reissue(&next, call);
        if *changed.value() {
            self.current = next;
            self.refused = None;
            Ok(())
        } else {
            // A failed clear handed the SDK no replacement descriptor and
            // therefore cannot release an earlier refused route either.
            if next.is_some() {
                self.refused = next;
            }
            Err(changed.failed())
        }
    }
}

fn reissue<R>(
    source: &Option<RetainedSource>,
    call: impl FnOnce(*const NDIlib_source_t) -> R,
) -> R {
    match source {
        Some(source) => source.reissue(call),
        None => call(ptr::null()),
    }
}

/// The old value remains owned through the call; moving the new value into the
/// slot is the last step, after native code has stopped reading the old one.
fn replace<T, R>(current: &mut T, next: T, call: impl FnOnce(&T) -> R) -> R {
    let result = call(&next);
    *current = next;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{sys::table::for_test, SourceAddress};
    use std::ffi::CStr;

    unsafe fn name<'a>(pointer: *const NDIlib_source_t) -> &'a CStr {
        // SAFETY: tests retain every descriptor passed to this helper.
        unsafe { CStr::from_ptr((*pointer).p_ndi_name) }
    }

    #[test]
    fn separate_targets_share_one_sources_validated_string_storage() {
        let input = Source::new("Studio (Camera)")
            .unwrap()
            .with_address(SourceAddress::new("192.0.2.1:5960").unwrap());
        let first = RetainedSource::new(&input);
        let second = RetainedSource::new(&input);
        drop(input);

        let first = first.descriptor();
        let second = second.descriptor();
        assert!(!ptr::eq(first, second));
        assert_eq!(first.p_ndi_name, second.p_ndi_name);
        // SAFETY: both retained targets own the source buffers and initialize
        // the address member of the SDK descriptor.
        unsafe {
            let address = first.__bindgen_anon_1.p_url_address;
            assert_eq!(address, second.__bindgen_anon_1.p_url_address);
            assert_eq!(CStr::from_ptr(first.p_ndi_name), c"Studio (Camera)");
            assert_eq!(CStr::from_ptr(address), c"192.0.2.1:5960");
        }
    }

    #[test]
    fn replacement_keeps_previous_descriptor_live_and_commits_the_passed_pointer() {
        let mut source = RetainedSource::new(&Source::new("previous").unwrap());
        let previous_storage = Arc::downgrade(&source.0);
        let previous = source.reissue(|pointer| pointer);
        let next = RetainedSource::new(&Source::new("replacement").unwrap());
        let passed = source.replace(next, |pointer| {
            // SAFETY: replacement retains both allocations throughout the call.
            unsafe {
                assert_eq!(name(previous), c"previous");
                assert_eq!(name(pointer), c"replacement");
            }
            pointer
        });
        assert_eq!(source.reissue(|pointer| pointer), passed);
        assert_eq!(source.current().name(), "replacement");
        assert!(previous_storage.upgrade().is_none());
    }

    #[test]
    fn cloned_receiver_target_keeps_the_same_descriptor_without_allocating() {
        let input = Source::new("receiver").unwrap();
        let (source, allocations) =
            crate::test_alloc::allocation_count(|| RetainedSource::new(&input));
        assert_eq!(allocations, 1, "one stable allocation per prepared target");
        let (snapshot, allocations) = crate::test_alloc::allocation_count(|| source.clone());
        assert_eq!(allocations, 0);
        let pointer = source.reissue(|pointer| pointer);
        drop(source);
        assert_eq!(snapshot.reissue(|pointer| pointer), pointer);
        // SAFETY: the snapshot still owns the exact descriptor and its strings.
        assert_eq!(unsafe { name(pointer) }, c"receiver");
    }

    #[test]
    fn optional_replacement_retains_previous_storage_through_clear() {
        let mut sources = RetainedSources::default();
        let previous = sources.replace(Some(&Source::new("previous").unwrap()), |pointer| pointer);
        sources.replace(None, |pointer| {
            assert!(pointer.is_null());
            // SAFETY: clear retains its previous source through the native call.
            assert_eq!(unsafe { name(previous) }, c"previous");
        });
        assert!(sources.current().is_none());
    }

    #[test]
    fn refused_routes_keep_the_effective_and_latest_refused_descriptors() {
        let mut sources = RetainedSources::default();
        let called = |value| crate::sys::table::for_test(value, c"routing_change");
        let mut effective = ptr::null();
        sources
            .replace_if(Some(&Source::new("effective").unwrap()), |pointer| {
                effective = pointer;
                called(true)
            })
            .unwrap();
        let mut refused = ptr::null();
        assert!(sources
            .replace_if(Some(&Source::new("refused").unwrap()), |pointer| {
                refused = pointer;
                called(false)
            })
            .is_err());
        assert!(sources
            .replace_if(Some(&Source::new("next refused").unwrap()), |_| {
                // SAFETY: both descriptors stay retained until this request returns.
                unsafe {
                    assert_eq!(name(effective), c"effective");
                    assert_eq!(name(refused), c"refused");
                }
                called(false)
            })
            .is_err());
        assert_eq!(sources.current().unwrap().name(), "effective");
        assert_eq!(
            sources.refused.as_ref().unwrap().current().name(),
            "next refused"
        );
        assert!(sources.replace_if(None, |_| called(false)).is_err());
        assert_eq!(sources.current().unwrap().name(), "effective");
        assert_eq!(
            sources.refused.as_ref().unwrap().current().name(),
            "next refused"
        );
        let effective = Arc::downgrade(&sources.current.as_ref().unwrap().0);
        let refused = Arc::downgrade(&sources.refused.as_ref().unwrap().0);
        sources
            .replace_if(None, |pointer| {
                assert!(pointer.is_null());
                assert!(effective.upgrade().is_some());
                assert!(refused.upgrade().is_some());
                called(true)
            })
            .unwrap();
        assert!(effective.upgrade().is_none());
        assert!(refused.upgrade().is_none());
    }

    #[test]
    fn configured_and_discovered_sources_share_text_identity() {
        use std::hash::{DefaultHasher, Hash, Hasher};

        let hash = |source: &Source| {
            let mut state = DefaultHasher::new();
            source.hash(&mut state);
            state.finish()
        };
        for (text, is_url) in [
            ("camera.local:8080", false),
            ("192.0.2.1:5960", false),
            ("[2001:db8::1]:5960", false),
            ("ndi://h:1", true),
            ("camera.local:8080?next=ndi://h:1", false),
            ("opaque text", false),
        ] {
            let configured = Source::new("Camera")
                .unwrap()
                .with_address(SourceAddress::new(text).unwrap());
            let retained = RetainedSource::new(&configured);
            let raw = retained.descriptor();
            // SAFETY: The retained target owns both C
            // strings for this synchronous owned copy.
            let discovered =
                unsafe { Source::try_from_raw(for_test(ptr::from_ref(raw), c"test")) }.unwrap();
            assert_eq!(configured, discovered);
            assert_eq!(hash(&configured), hash(&discovered));
            let address = discovered.address().unwrap();
            assert_eq!(address.as_str(), text);
            assert_eq!(address.is_url(), is_url);
            // Both native union member names denote the same address pointer.
            // SAFETY: The retained target initialized the pointer member of this union.
            unsafe {
                assert_eq!(
                    raw.__bindgen_anon_1.p_url_address,
                    raw.__bindgen_anon_1.p_ip_address
                );
                assert_eq!(
                    std::ffi::CStr::from_ptr(raw.__bindgen_anon_1.p_url_address)
                        .to_str()
                        .unwrap(),
                    text
                );
            }
        }
    }

    #[test]
    fn address_only_source_round_trips_with_an_empty_name() {
        let url = c"ndi://127.0.0.1:5960";
        let raw = NDIlib_source_t {
            p_ndi_name: ptr::null(),
            __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                p_url_address: url.as_ptr(),
            },
        };
        // SAFETY: raw borrows the static terminated URL and is live through copying.
        let source =
            unsafe { Source::try_from_raw(for_test(ptr::from_ref(&raw), c"test")) }.unwrap();
        assert!(source.name().is_empty());
        assert!(
            matches!(source.address(), Some(address) if address.is_url() && address.as_str() == "ndi://127.0.0.1:5960")
        );
        let roundtrip = RetainedSource::new(&source);
        let descriptor = roundtrip.descriptor();
        // SAFETY: the retained target owns the non-null empty name and URL.
        unsafe {
            assert_eq!(CStr::from_ptr(descriptor.p_ndi_name), c"");
            assert_eq!(
                CStr::from_ptr(descriptor.__bindgen_anon_1.p_url_address),
                url
            );
        }
    }
}
