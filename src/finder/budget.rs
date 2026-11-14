//! Finder-specific count bounds and the shared discovery snapshot copier.

use std::{ffi::c_char, mem::size_of};

use crate::{
    sdk_copy::{CopyBudget, CopyError},
    Error, ResourceKind, Result,
};

pub(super) use crate::sdk_copy::{allocation_failed, COPY_LIMIT as MAX_BYTES};
pub(super) const MAX_SOURCES: usize = 16_384;

pub(super) fn limit(resource: ResourceKind, limit: usize) -> Error {
    Error::ResourceLimit { resource, limit }
}

/// Finder keeps its own descriptor count limit in addition to shared spans.
pub(super) fn descriptor_bytes(count: usize, element_size: usize) -> Result<usize> {
    if count > MAX_SOURCES {
        return Err(limit(ResourceKind::FinderSnapshotSources, MAX_SOURCES));
    }
    CopyBudget::span_bytes(count, element_size).map_err(CopyError::into_error)
}

/// Finder's policy omits an entry whose optional string exceeds its bound.
/// All byte accounting and copying are performed by the shared budget.
pub(crate) struct SnapshotBudget(CopyBudget);

impl SnapshotBudget {
    pub(crate) fn new() -> Self {
        Self(CopyBudget::new(
            MAX_BYTES,
            ResourceKind::FinderSnapshotBytes,
        ))
    }

    /// # Safety
    /// A non-null pointer remains readable through its first NUL or the SDK
    /// string bound, whichever comes first.
    pub(crate) unsafe fn text(&mut self, pointer: *const c_char) -> Result<Option<String>> {
        // SAFETY: forwarded to the caller's obligations.
        match unsafe { self.0.text(pointer) } {
            Ok(text) => Ok(text),
            Err(CopyError::Malformed(_)) => Ok(None),
            Err(error) => Err(error.into_error()),
        }
    }
}

/// Deep-copy one SDK snapshot under the Finder snapshot lock.
///
/// # Safety
/// `descriptors` is null or points to `count` initialized descriptors that
/// remain live, along with their strings, for the duration of copying.
pub(super) unsafe fn copy_snapshot<D, T>(
    descriptors: *const D,
    count: u32,
    mut copy: impl FnMut(&D, &mut SnapshotBudget) -> Result<Option<T>>,
) -> Result<Vec<T>> {
    descriptor_bytes(count as usize, size_of::<D>())?;
    descriptor_bytes(count as usize, size_of::<T>())?;
    let mut budget = SnapshotBudget::new();
    // SAFETY: the count and native span passed preflight; the caller keeps
    // this array live and excludes snapshot replacement.
    let raw = unsafe { budget.0.slice(descriptors, count) }.map_err(CopyError::into_error)?;
    let mut copied = budget.0.reserve(raw.len()).map_err(CopyError::into_error)?;
    for entry in raw {
        if let Some(value) = copy(entry, &mut budget)? {
            copied.push(value);
        }
    }
    Ok(copied)
}

pub(super) fn copy_string(text: &str) -> Result<String> {
    let mut copy = String::new();
    copy.try_reserve_exact(text.len())
        .map_err(|_| allocation_failed())?;
    copy.push_str(text);
    Ok(copy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        finder::snapshot_entry, frames::MAX_SDK_STRING_BYTES, sys::bindings::NDIlib_source_t,
        Source,
    };
    use std::{ffi::CString, ptr};

    fn named(pointer: *const c_char) -> NDIlib_source_t {
        NDIlib_source_t {
            p_ndi_name: pointer,
            ..Default::default()
        }
    }

    fn snapshot(entries: &[NDIlib_source_t]) -> Result<Vec<Source>> {
        // SAFETY: tests keep all arrays and their strings live until return.
        unsafe {
            copy_snapshot(entries.as_ptr(), entries.len() as u32, |entry, budget| {
                snapshot_entry(entry, budget)
            })
        }
    }

    #[test]
    fn count_and_span_are_checked_before_dereferencing_or_reserving() {
        assert_eq!(descriptor_bytes(MAX_SOURCES, 16).unwrap(), MAX_SOURCES * 16);
        for (count, size) in [(MAX_SOURCES + 1, 16), (usize::MAX, 2)] {
            assert!(matches!(
                descriptor_bytes(count, size),
                Err(Error::ResourceLimit {
                    resource: ResourceKind::FinderSnapshotSources,
                    limit: MAX_SOURCES
                })
            ));
        }
        assert!(matches!(
            descriptor_bytes(2, isize::MAX as usize),
            Err(Error::UnexpectedSdkResult(_))
        ));
        // SAFETY: a null array is allowed; the excessive count is rejected
        // before the copier is called or any storage is reserved.
        let (result, allocations) = crate::test_alloc::allocation_count(|| unsafe {
            copy_snapshot::<NDIlib_source_t, Source>(ptr::null(), u32::MAX, |_, _| {
                panic!("preflight")
            })
        });
        assert!(matches!(result, Err(Error::ResourceLimit { .. })));
        assert_eq!(allocations, 0);
        let entries = vec![named(c"Camera".as_ptr()); MAX_SOURCES];
        assert_eq!(snapshot(&entries).unwrap().len(), MAX_SOURCES);
    }

    #[test]
    fn aggregate_bytes_are_checked_before_string_allocation() {
        let overhead = 2 * (size_of::<NDIlib_source_t>() + size_of::<Source>());
        let len = (MAX_BYTES - overhead - 2) / 4;
        let fits = CString::new(vec![b'x'; len]).unwrap();
        assert_eq!(snapshot(&[named(fits.as_ptr()); 2]).unwrap().len(), 2);
        let exceeds = CString::new(vec![b'x'; len + 1]).unwrap();
        assert!(matches!(
            snapshot(&[named(exceeds.as_ptr()); 2]),
            Err(Error::ResourceLimit { .. })
        ));
        // Many individually small strings are subject to the same budget.
        let short = CString::new(vec![b'x'; 1024]).unwrap();
        assert!(matches!(
            snapshot(&vec![named(short.as_ptr()); MAX_SOURCES]),
            Err(Error::ResourceLimit { .. })
        ));

        // Name and address share the aggregate budget, even when each one
        // fits its individual string cap.
        let largest = CString::new(vec![b'x'; MAX_SDK_STRING_BYTES - 1]).unwrap();
        let mut both = named(largest.as_ptr());
        both.__bindgen_anon_1.p_url_address = largest.as_ptr();
        assert!(matches!(
            snapshot(&[both]),
            Err(Error::ResourceLimit { .. })
        ));
    }

    #[test]
    fn lossy_expansion_is_charged_before_allocating_the_output() {
        let text = CString::new(vec![0xff; MAX_SDK_STRING_BYTES - 1]).unwrap();
        let (result, allocations) =
            crate::test_alloc::allocation_count(|| snapshot(&[named(text.as_ptr())]));
        assert!(matches!(result, Err(Error::ResourceLimit { .. })));
        assert_eq!(
            allocations, 1,
            "only the bounded result vector was reserved"
        );
        let mut budget = SnapshotBudget::new();
        let bytes = c"a\xff\xe2\x82\xac\xe2\x82";
        // SAFETY: live literal, including a NUL terminator.
        let copied = unsafe { budget.text(bytes.as_ptr()) }.unwrap().unwrap();
        assert_eq!(copied, bytes.to_string_lossy());
    }

    #[test]
    fn rejected_entries_still_consume_the_scan_budget() {
        // No terminator within the individual limit; every scan is charged.
        let unterminated = vec![b'x'; MAX_SDK_STRING_BYTES];
        let malformed = named(unterminated.as_ptr().cast());
        let valid = named(c"Survivor".as_ptr());
        assert_eq!(snapshot(&[malformed, valid]).unwrap()[0].name(), "Survivor");
        assert!(matches!(
            snapshot(&[malformed; 4]),
            Err(Error::ResourceLimit { .. })
        ));
        let largest = CString::new(vec![b'x'; MAX_SDK_STRING_BYTES - 1]).unwrap();
        assert_eq!(
            snapshot(&[named(largest.as_ptr())]).unwrap()[0]
                .name()
                .len(),
            MAX_SDK_STRING_BYTES - 1
        );
    }
}
