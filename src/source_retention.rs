//! Stable source descriptors retained across native requests.
//!
//! Owners keep their existing locks and destroy their native object before
//! dropping this storage. Replacement calls retain both the old descriptor and
//! the replacement until the native call returns. No SDK copying is assumed.

use std::{ptr, sync::Arc};

use crate::{finder::RawSource, sys::bindings::NDIlib_source_t, Source};

/// One source and its stable descriptor, shared only for an in-progress
/// receiver request so snapshots need not hold the target lock during SDK work.
#[derive(Clone)]
pub(crate) struct RetainedSource {
    source: Source,
    descriptor: Arc<RawSource>,
}

impl RetainedSource {
    pub(crate) fn new(source: &Source) -> Self {
        Self {
            source: source.clone(),
            descriptor: Arc::new(source.to_raw()),
        }
    }

    pub(crate) fn current(&self) -> &Source {
        &self.source
    }

    pub(crate) fn descriptor(&self) -> &NDIlib_source_t {
        self.descriptor.as_raw()
    }

    /// Reissue this exact descriptor without allocating or converting it.
    pub(crate) fn reissue<R>(&self, call: impl FnOnce(*const NDIlib_source_t) -> R) -> R {
        call(ptr::from_ref(self.descriptor.as_raw()))
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
    use std::ffi::CStr;

    unsafe fn name<'a>(pointer: *const NDIlib_source_t) -> &'a CStr {
        // SAFETY: tests retain every descriptor passed to this helper.
        unsafe { CStr::from_ptr((*pointer).p_ndi_name) }
    }

    #[test]
    fn replacement_keeps_previous_descriptor_live_and_commits_the_passed_pointer() {
        let mut source = RetainedSource::new(&Source::new("previous").unwrap());
        let previous_storage = Arc::downgrade(&source.descriptor);
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
        let effective = Arc::downgrade(&sources.current.as_ref().unwrap().descriptor);
        let refused = Arc::downgrade(&sources.refused.as_ref().unwrap().descriptor);
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
}
