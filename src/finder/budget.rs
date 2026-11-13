//! Resource bounds shared by plain and Advanced Finder snapshots.

use std::{ffi::c_char, mem::size_of};

use crate::{
    frames::{bounded_sdk_text, MAX_SDK_STRING_BYTES},
    sdk_copy::LossyText,
    Error, Result,
};

pub(super) const MAX_SOURCES: usize = 16_384;
pub(super) const MAX_BYTES: usize = 16 * 1024 * 1024;

pub(super) fn limit(resource: &'static str, limit: usize) -> Error {
    Error::ResourceLimit { resource, limit }
}

pub(super) fn allocation_failed() -> Error {
    Error::UnexpectedSdkResult("could not reserve memory for Finder results".into())
}

/// Check the native span before pointer arithmetic, even on 32-bit targets.
pub(super) fn descriptor_bytes(count: usize, element_size: usize) -> Result<usize> {
    let bytes = count
        .checked_mul(element_size)
        .filter(|&bytes| bytes <= isize::MAX as usize)
        .ok_or_else(|| limit("Finder descriptor span", isize::MAX as usize))?;
    if count > MAX_SOURCES {
        return Err(limit("Finder snapshot source count", MAX_SOURCES));
    }
    Ok(bytes)
}

pub(crate) struct SnapshotBudget {
    remaining: usize,
}

impl SnapshotBudget {
    pub(crate) fn new() -> Self {
        Self {
            remaining: MAX_BYTES,
        }
    }

    fn take(&mut self, bytes: usize) -> Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or_else(|| limit("Finder snapshot bytes", MAX_BYTES))?;
        Ok(())
    }

    /// Copy one optional string. An unterminated individual string is left
    /// out, but its entire scan is charged, including for rejected entries.
    ///
    /// # Safety
    /// A non-null pointer must remain initialized and readable through its
    /// first NUL or `MAX_SDK_STRING_BYTES`, whichever comes first.
    pub(crate) unsafe fn text(&mut self, pointer: *const c_char) -> Result<Option<String>> {
        if pointer.is_null() {
            return Ok(None);
        }
        let scan_limit = self.remaining.min(MAX_SDK_STRING_BYTES);
        // SAFETY: the caller provides a live string, and the scan stops at
        // the first NUL or the smaller of the work and individual limits.
        let bytes = unsafe { bounded_sdk_text(pointer, scan_limit) };
        let Some(bytes) = bytes else {
            self.take(scan_limit)?;
            return if scan_limit < MAX_SDK_STRING_BYTES {
                Err(limit("Finder snapshot bytes", MAX_BYTES))
            } else {
                Ok(None)
            };
        };
        self.take(bytes.len() + 1)?;
        let text = LossyText::new(bytes);
        self.take(text.len())?;
        Ok(Some(text.copy().map_err(|_| allocation_failed())?))
    }
}

/// Deep-copy one SDK snapshot. Used under the Finder snapshot lock; kept
/// separate so limits can be tested without an installed native runtime.
///
/// # Safety
/// `descriptors` is null or points to `count` initialized descriptors that
/// remain live, along with their strings, for the duration of copying.
pub(super) unsafe fn copy_snapshot<D, T>(
    descriptors: *const D,
    count: u32,
    mut copy: impl FnMut(&D, &mut SnapshotBudget) -> Result<Option<T>>,
) -> Result<Vec<T>> {
    let count = count as usize;
    let bytes = descriptor_bytes(count, size_of::<D>())?;
    let mut budget = SnapshotBudget::new();
    budget.take(bytes)?;
    // Include the returned vector's storage as well as the native scan.
    budget.take(descriptor_bytes(count, size_of::<T>())?)?;
    if descriptors.is_null() {
        return if count == 0 {
            Ok(Vec::new())
        } else {
            Err(Error::UnexpectedSdkResult(
                "Finder returned a nonzero source count with a null array".into(),
            ))
        };
    }
    let mut copied = Vec::new();
    copied
        .try_reserve_exact(count)
        .map_err(|_| allocation_failed())?;
    for index in 0..count {
        // SAFETY: the count and descriptor span passed preflight; the
        // caller keeps this array live and excludes snapshot replacement.
        if let Some(value) = copy(unsafe { &*descriptors.add(index) }, &mut budget)? {
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
    use crate::{finder::snapshot_entry, sys::bindings::NDIlib_source_t, Source};
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
        for (count, size) in [
            (MAX_SOURCES + 1, 16),
            (usize::MAX, 2),
            (2, isize::MAX as usize),
        ] {
            assert!(matches!(
                descriptor_bytes(count, size),
                Err(Error::ResourceLimit { .. })
            ));
        }
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
        assert_eq!(
            budget.remaining,
            MAX_BYTES - bytes.to_bytes_with_nul().len() - copied.len()
        );
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
