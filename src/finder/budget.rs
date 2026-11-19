//! Finder-specific count bounds and the shared discovery snapshot copier.

use std::{
    ffi::{c_char, CStr},
    mem::size_of,
};

use crate::{
    finder::SourceText,
    sdk_copy::{CopyBudget, TextError},
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
    Ok(CopyBudget::span_bytes(count, element_size)?)
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

    #[cfg(test)]
    pub(crate) fn spent(&self) -> usize {
        MAX_BYTES - self.0.remaining()
    }

    /// Charge the shared empty name used for an address-only source.
    pub(super) fn charge_empty_source_name(&mut self) -> Result<()> {
        self.0.take(SourceText::STORAGE_OVERHEAD)
    }

    /// Copy advertised metadata without building native source storage.
    ///
    /// # Safety
    /// As for [`Self::source_c_str`].
    #[cfg(all(feature = "advanced", any(test, ndi_has_advanced_source_metadata)))]
    pub(crate) unsafe fn metadata_text(
        &mut self,
        pointer: *const c_char,
    ) -> Result<Option<String>> {
        // SAFETY: forwarded to the caller's obligations.
        match unsafe { self.0.text(pointer) } {
            Ok(text) => Ok(text),
            Err(TextError::Unterminated) => Ok(None),
            Err(TextError::Failed(error)) => Err(error),
        }
    }

    /// Borrow bounded source text and charge its scan and storage.
    /// The charge uses the original bytes even when a descriptor is omitted.
    ///
    /// # Safety
    /// A non-null pointer stays readable through its first NUL or the SDK
    /// string bound, with storage kept live for the returned lifetime.
    pub(super) unsafe fn source_c_str<'a>(
        &mut self,
        pointer: *const c_char,
    ) -> Result<Option<&'a CStr>> {
        // SAFETY: forwarded to the caller's obligations.
        match unsafe { self.0.c_str(pointer) } {
            Ok(text) => {
                if let Some(text) = text {
                    self.0
                        .take(text.to_bytes().len() + SourceText::STORAGE_OVERHEAD)?;
                }
                Ok(text)
            }
            Err(TextError::Unterminated) => Ok(None),
            Err(TextError::Failed(error)) => Err(error),
        }
    }
}

/// Scan and budget one SDK snapshot under the Finder snapshot lock. A
/// selecting collector spends the full descriptor and result spans even when
/// it retains fewer entries.
/// # Safety
/// `descriptors` is null or points to `count` initialized descriptors that
/// remain live, along with their strings, for the duration of the scan.
pub(super) unsafe fn scan_snapshot<D, T>(
    descriptors: *const D,
    count: u32,
    mut visit: impl FnMut(&D, &mut SnapshotBudget, usize) -> Result<()>,
) -> Result<()> {
    descriptor_bytes(count as usize, size_of::<D>())?;
    descriptor_bytes(count as usize, size_of::<T>())?;
    let mut budget = SnapshotBudget::new();
    // SAFETY: the count and native span passed preflight; the caller keeps
    // this array live and excludes snapshot replacement.
    let raw = unsafe { budget.0.slice(descriptors, count) }?;
    budget.0.charge_span::<T>(raw.len())?;
    for (index, entry) in raw.iter().enumerate() {
        visit(entry, &mut budget, raw.len() - index)?;
    }
    Ok(())
}

pub(super) fn push_snapshot_item<T>(
    copied: &mut Vec<T>,
    item: Option<T>,
    remaining: usize,
) -> Result<()> {
    if let Some(item) = item {
        // Full snapshots reserve once, after finding an entry to store.
        if copied.is_empty() {
            copied
                .try_reserve_exact(remaining)
                .map_err(|_| allocation_failed())?;
        }
        copied.push(item);
    }
    Ok(())
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
        finder::{decode_source, source_pointers, HostQuery, SeenSource, SourceTextView},
        sdk_copy::MAX_SDK_STRING_BYTES,
        sys::bindings::NDIlib_source_t,
        Source,
    };
    use std::{collections::HashSet, ffi::CString, ptr};

    unsafe fn copy_text(
        budget: &mut SnapshotBudget,
        pointer: *const c_char,
    ) -> Result<Option<SourceText>> {
        // SAFETY: tests keep the bounded string readable.
        unsafe { budget.source_c_str(pointer) }?
            .map(|text| {
                let text = SourceTextView::new(text).map_err(Error::InvalidUtf8)?;
                SourceText::from_sdk(text, crate::AllocationKind::DiscoveryResults)
            })
            .transpose()
    }

    fn named(pointer: *const c_char) -> NDIlib_source_t {
        NDIlib_source_t {
            p_ndi_name: pointer,
            ..Default::default()
        }
    }

    unsafe fn copy_snapshot<D, T>(
        descriptors: *const D,
        count: u32,
        mut copy: impl FnMut(&D, &mut SnapshotBudget) -> Result<Option<T>>,
    ) -> Result<Vec<T>> {
        let mut copied = Vec::new();
        // SAFETY: the caller keeps the descriptors and their strings live.
        unsafe {
            scan_snapshot::<D, T>(descriptors, count, |entry, budget, remaining| {
                push_snapshot_item(&mut copied, copy(entry, budget)?, remaining)
            })?;
        }
        Ok(copied)
    }

    fn snapshot(entries: &[NDIlib_source_t]) -> Result<Vec<Source>> {
        // SAFETY: tests keep all arrays and their strings live until return.
        unsafe {
            copy_snapshot(entries.as_ptr(), entries.len() as u32, |entry, budget| {
                let (name, address) = source_pointers(entry);
                decode_source(name, address, budget, |_| true)
            })
        }
    }

    #[test]
    fn small_source_text_budgets_check_exact_copy_and_scan_bounds() {
        let bytes = c"a�";
        let charged =
            bytes.to_bytes_with_nul().len() + bytes.to_bytes().len() + SourceText::STORAGE_OVERHEAD;
        let budget = |remaining| {
            SnapshotBudget(CopyBudget::new(
                remaining,
                ResourceKind::FinderSnapshotBytes,
            ))
        };
        let mut exact = budget(charged);
        // SAFETY: the literal remains live through its NUL terminator.
        let copied = unsafe { copy_text(&mut exact, bytes.as_ptr()) }
            .unwrap()
            .unwrap();
        assert_eq!(copied.as_str(), "a\u{fffd}");
        assert_eq!(exact.0.remaining(), 0);

        let mut short = budget(charged - 1);
        let (result, allocations) = crate::test_alloc::allocation_count(|| {
            // SAFETY: the literal remains live through its NUL terminator.
            unsafe { copy_text(&mut short, bytes.as_ptr()) }
        });
        assert!(matches!(result, Err(Error::ResourceLimit { .. })));
        assert_eq!(allocations, 0);

        let unterminated = [b'x'; 8];
        let mut scan = budget(unterminated.len());
        // SAFETY: every byte within this budget's scan bound is live.
        let result = unsafe { copy_text(&mut scan, unterminated.as_ptr().cast()) };
        assert!(matches!(result, Err(Error::ResourceLimit { .. })));
        assert_eq!(scan.0.remaining(), 0);
        assert_eq!(descriptor_bytes(MAX_SOURCES, 16).unwrap(), MAX_SOURCES * 16);
        assert!(matches!(
            descriptor_bytes(MAX_SOURCES + 1, 16),
            Err(Error::ResourceLimit { .. })
        ));
    }

    #[test]
    fn snapshot_builds_each_source_string_in_two_allocations() {
        let mut entry = named(c"HOST (Camera)".as_ptr());
        entry.__bindgen_anon_1.p_url_address = c"192.0.2.7:5961".as_ptr();
        let entries = [entry; 100];
        let (sources, allocations) = crate::test_alloc::allocation_count(|| snapshot(&entries));
        assert_eq!(sources.unwrap().len(), 100);
        assert_eq!(allocations, 401, "two per source string, plus the vector");
    }

    fn selected_snapshot(
        entries: &[NDIlib_source_t],
        select: impl Fn(&super::super::BorrowedSource<'_>) -> bool,
    ) -> Result<Vec<Source>> {
        // SAFETY: tests keep the descriptors and all pointed-to strings live.
        unsafe {
            copy_snapshot(entries.as_ptr(), entries.len() as u32, |entry, budget| {
                let (name, address) = super::super::source_pointers(entry);
                super::super::decode_source(name, address, budget, &select)
            })
        }
    }

    #[test]
    fn selecting_names_and_hosts_allocates_only_the_matching_sources() {
        let mut nonmatch = named(c"OTHER (Camera)".as_ptr());
        nonmatch.__bindgen_anon_1.p_url_address = c"192.0.2.8:5961".as_ptr();
        let mut matching = named(c"HOST (Camera)".as_ptr());
        matching.__bindgen_anon_1.p_url_address = c"192.0.2.7:5961".as_ptr();
        let mut entries = [nonmatch; 100];
        entries[72] = matching;
        for (name, expected) in [("missing", 0), ("HOST (Camera)", 1)] {
            let (sources, allocations) = crate::test_alloc::allocation_count(|| {
                selected_snapshot(&entries, |source| source.name.as_str() == name)
            });
            assert_eq!(sources.unwrap().len(), expected);
            assert_eq!(allocations, 4 * expected + usize::from(expected > 0));
        }
        let query = HostQuery::checked("192.0.2.7").unwrap();
        let empty_seen = HashSet::new();
        let (sources, allocations) = crate::test_alloc::allocation_count(|| {
            selected_snapshot(&entries, |source| {
                source.selected_by(Some(query), &empty_seen)
            })
        });
        assert_eq!(sources.unwrap().len(), 1);
        assert_eq!(allocations, 5);

        let seen = snapshot(&entries)
            .unwrap()
            .into_iter()
            .map(SeenSource)
            .collect();
        let (sources, allocations) = crate::test_alloc::allocation_count(|| {
            selected_snapshot(&entries, |source| source.selected_by(None, &seen))
        });
        assert!(sources.unwrap().is_empty());
        assert_eq!(
            allocations, 0,
            "a repeated snapshot allocates no results or source strings"
        );
    }

    #[test]
    fn selecting_snapshots_preserves_host_rules_and_order() {
        let mut invalid = named(c"STUDIO \xff (Camera)".as_ptr());
        invalid.__bindgen_anon_1.p_url_address = c"http://camera.local:8080/path".as_ptr();
        let mut replacement = invalid;
        replacement.p_ndi_name = c"STUDIO � (Camera)".as_ptr();
        let mut addressed = named(ptr::null());
        addressed.__bindgen_anon_1.p_url_address = c"192.0.2.7:5961".as_ptr();
        let mut missing_address = named(c"STUDIO (Camera)".as_ptr());
        missing_address.__bindgen_anon_1.p_url_address = ptr::null();
        let mut ipv6 = named(c"OTHER (IPv6)".as_ptr());
        ipv6.__bindgen_anon_1.p_url_address = c"http://[2001:db8::1]:8080/path".as_ptr();
        let entries = [
            invalid,
            addressed,
            missing_address,
            replacement,
            ipv6,
            NDIlib_source_t::default(),
        ];
        let copied = snapshot(&entries).unwrap();
        for name in ["STUDIO \u{fffd} (Camera)", "STUDIO (Camera)", "", "absent"] {
            let expected = copied
                .iter()
                .filter(|source| source.name() == name)
                .cloned()
                .collect::<Vec<_>>();
            let actual =
                selected_snapshot(&entries, |source| source.name.as_str() == name).unwrap();
            assert_eq!(actual, expected);
        }
        let empty_seen = HashSet::new();
        for host in [
            "studio",
            "STUDIO \u{fffd}",
            "camera.local",
            "192.0.2.7",
            "[2001:db8::1]",
            "missing",
        ] {
            let query = HostQuery::checked(host).unwrap();
            let expected = copied
                .iter()
                .filter(|source| source.matches_query(query))
                .cloned()
                .collect::<Vec<_>>();
            let actual = selected_snapshot(&entries, |source| {
                source.selected_by(Some(query), &empty_seen)
            })
            .unwrap();
            assert_eq!(actual, expected, "{host}");
        }
        let seen = copied.iter().cloned().map(SeenSource).collect();
        assert!(
            selected_snapshot(&entries, |source| source.selected_by(None, &seen))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn omitted_selection_keeps_ordinary_copy_charges_and_errors() {
        let seen = HashSet::new();
        let query = HostQuery::checked("192.0.2.254").unwrap();
        let reject =
            |source: &super::super::BorrowedSource<'_>| source.selected_by(Some(query), &seen);
        let mut both = named(c"a\xff".as_ptr());
        both.__bindgen_anon_1.p_url_address = c"http://camera.local:8080".as_ptr();
        let mut address_only = named(ptr::null());
        address_only.__bindgen_anon_1.p_url_address = c"192.0.2.7".as_ptr();
        for entry in [
            both,
            address_only,
            named(c"name".as_ptr()),
            NDIlib_source_t::default(),
        ] {
            let (name, address) = super::super::source_pointers(&entry);
            let mut old = SnapshotBudget::new();
            let mut selected = SnapshotBudget::new();
            // SAFETY: each test descriptor points to live terminated literals.
            unsafe {
                super::super::decode_source(name, address, &mut old, |_| true).unwrap();
                assert!(
                    super::super::decode_source(name, address, &mut selected, reject)
                        .unwrap()
                        .is_none()
                );
            }
            assert_eq!(selected.0.remaining(), old.0.remaining());
        }

        for remaining in 0..128 {
            let mut old = SnapshotBudget(CopyBudget::new(
                remaining,
                ResourceKind::FinderSnapshotBytes,
            ));
            let mut selected = SnapshotBudget(CopyBudget::new(
                remaining,
                ResourceKind::FinderSnapshotBytes,
            ));
            let (name, address) = super::super::source_pointers(&both);
            // SAFETY: the descriptor points to live terminated literals.
            let (expected, actual) = unsafe {
                (
                    super::super::decode_source(name, address, &mut old, |_| true).map(|_| ()),
                    super::super::decode_source(name, address, &mut selected, reject).map(|_| ()),
                )
            };
            assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
            assert_eq!(selected.0.remaining(), old.0.remaining());
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
        let overhead =
            2 * (size_of::<NDIlib_source_t>() + size_of::<Source>() + SourceText::STORAGE_OVERHEAD);
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

        // Selection must also retain ordinary errors at the full SDK bounds.
        let unterminated = vec![b'x'; MAX_SDK_STRING_BYTES];
        let malformed = named(unterminated.as_ptr().cast());
        let largest = CString::new(vec![b'x'; MAX_SDK_STRING_BYTES - 1]).unwrap();
        let mut oversized = named(largest.as_ptr());
        oversized.__bindgen_anon_1.p_url_address = largest.as_ptr();
        for entries in [
            vec![malformed],
            vec![malformed; 4],
            vec![oversized],
            vec![named(largest.as_ptr()); 3],
        ] {
            let expected = snapshot(&entries).map(|_| ());
            let actual = selected_snapshot(&entries, |_| false).map(|_| ());
            assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
        }
    }

    #[test]
    fn invalid_utf8_spends_the_copy_budget_without_allocating() {
        let text = CString::new(vec![0xff; MAX_SDK_STRING_BYTES - 1]).unwrap();
        let invalid = named(text.as_ptr());
        let (result, allocations) = crate::test_alloc::allocation_count(|| snapshot(&[invalid]));
        assert!(result.unwrap().is_empty());
        assert_eq!(allocations, 0);
        let (result, allocations) = crate::test_alloc::allocation_count(|| snapshot(&[invalid; 4]));
        assert!(matches!(result, Err(Error::ResourceLimit { .. })));
        assert_eq!(allocations, 0);

        let mut budget = SnapshotBudget::new();
        let name = c"a\xff";
        let address = c"192.0.2.1";
        // SAFETY: both literals remain live. Validation must precede selection.
        let result = unsafe {
            decode_source(name.as_ptr(), address.as_ptr(), &mut budget, |_| {
                panic!("malformed descriptor reached the selector")
            })
        };
        assert!(result.unwrap().is_none());
        assert_eq!(
            budget.spent(),
            name.to_bytes().len() * 2
                + address.to_bytes().len() * 2
                + 2 * (1 + SourceText::STORAGE_OVERHEAD)
        );
    }

    #[test]
    fn rejected_entries_still_consume_the_scan_budget() {
        // No terminator within the individual limit; every scan is charged.
        let unterminated = vec![b'x'; MAX_SDK_STRING_BYTES];
        let malformed = named(unterminated.as_ptr().cast());
        let first = named(c"Before".as_ptr());
        let last = named(c"After".as_ptr());
        let copied = snapshot(&[first, malformed, last]).unwrap();
        assert_eq!(
            copied.iter().map(Source::name).collect::<Vec<_>>(),
            ["Before", "After"]
        );
        assert!(matches!(
            snapshot(&[malformed; 4]),
            Err(Error::ResourceLimit { .. })
        ));

        let mut overlong_address = named(c"OTHER (Camera)".as_ptr());
        overlong_address.__bindgen_anon_1.p_url_address = unterminated.as_ptr().cast();
        let mut entries = [overlong_address; 5];
        entries[0] = named(c"STUDIO_A (First)".as_ptr());
        let query = HostQuery::checked("STUDIO_A").unwrap();
        let seen = HashSet::new();
        assert!(matches!(
            selected_snapshot(&entries, |source| source.selected_by(Some(query), &seen)),
            Err(Error::ResourceLimit {
                resource: ResourceKind::FinderSnapshotBytes,
                ..
            })
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
