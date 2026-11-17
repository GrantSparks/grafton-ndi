//! Finder-specific count bounds and the shared discovery snapshot copier.

use std::{ffi::c_char, mem::size_of};

use crate::{
    finder::SourceText,
    sdk_copy::{CopyBudget, LossyText, TextError},
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

    #[cfg(all(test, feature = "advanced"))]
    pub(crate) fn spent(&self) -> usize {
        MAX_BYTES - self.0.remaining()
    }

    /// Charge the shared empty name used for an address-only source.
    pub(super) fn empty_source_name(&mut self) -> Result<()> {
        self.0.take(SourceText::STORAGE_OVERHEAD)
    }

    /// Copy advertised metadata without building native source storage.
    ///
    /// # Safety
    /// As for [`Self::text`].
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

    /// Borrow classified text after charging exactly the ordinary source copy.
    ///
    /// # Safety
    /// As for [`Self::text`], with storage kept live for the returned lifetime.
    pub(super) unsafe fn source_text_view<'a>(
        &mut self,
        pointer: *const c_char,
    ) -> Result<Option<LossyText<'a>>> {
        // SAFETY: forwarded to the caller's obligations.
        match unsafe { self.0.source_text_view(pointer) } {
            Ok(text) => Ok(text),
            Err(TextError::Unterminated) => Ok(None),
            Err(TextError::Failed(error)) => Err(error),
        }
    }

    /// # Safety
    /// A non-null pointer remains readable through its first NUL or the SDK
    /// string bound, whichever comes first.
    pub(crate) unsafe fn text(&mut self, pointer: *const c_char) -> Result<Option<SourceText>> {
        // SAFETY: forwarded to the caller's obligations.
        match unsafe { self.0.source_text(pointer) } {
            Ok(text) => Ok(text),
            Err(TextError::Unterminated) => Ok(None),
            Err(TextError::Failed(error)) => Err(error),
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
    let raw = unsafe { budget.0.slice(descriptors, count) }?;
    let mut copied = budget.0.reserve(raw.len())?;
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
        finder::snapshot_entry, sdk_copy::MAX_SDK_STRING_BYTES, sys::bindings::NDIlib_source_t,
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
    fn small_source_text_budgets_check_exact_copy_and_scan_bounds() {
        let bytes = c"a\xff";
        let charged = bytes.to_bytes_with_nul().len() + 4 + SourceText::STORAGE_OVERHEAD;
        let budget = |remaining| {
            SnapshotBudget(CopyBudget::new(
                remaining,
                ResourceKind::FinderSnapshotBytes,
            ))
        };
        let mut exact = budget(charged);
        // SAFETY: the literal remains live through its NUL terminator.
        let copied = unsafe { exact.text(bytes.as_ptr()) }.unwrap().unwrap();
        assert_eq!(copied.as_str(), "a\u{fffd}");
        assert_eq!(exact.0.remaining(), 0);

        let mut short = budget(charged - 1);
        let (result, allocations) = crate::test_alloc::allocation_count(|| {
            // SAFETY: the literal remains live through its NUL terminator.
            unsafe { short.text(bytes.as_ptr()) }
        });
        assert!(matches!(result, Err(Error::ResourceLimit { .. })));
        assert_eq!(allocations, 0);

        let unterminated = [b'x'; 8];
        let mut scan = budget(unterminated.len());
        // SAFETY: every byte within this budget's scan bound is live.
        let result = unsafe { scan.text(unterminated.as_ptr().cast()) };
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
        select: impl Fn(&super::super::BorrowedSource<'_>) -> Result<bool>,
    ) -> Result<Vec<Source>> {
        // SAFETY: tests keep the descriptors and all pointed-to strings live.
        unsafe {
            copy_snapshot(entries.as_ptr(), entries.len() as u32, |entry, budget| {
                let (name, address) = super::super::source_pointers(entry);
                super::super::snapshot_selected_source(name, address, budget, &select)
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
                selected_snapshot(&entries, |source| Ok(source.name.equals(name)))
            });
            assert_eq!(sources.unwrap().len(), expected);
            assert_eq!(allocations, 1 + 4 * expected);
        }
        let query = super::super::HostQuery::checked("192.0.2.7").unwrap();
        let (sources, allocations) = crate::test_alloc::allocation_count(|| {
            selected_snapshot(&entries, |source| source.matches_query(query))
        });
        assert_eq!(sources.unwrap().len(), 1);
        assert_eq!(allocations, 5);

        let seen = snapshot(&entries)
            .unwrap()
            .into_iter()
            .map(super::super::SeenSource)
            .collect();
        let (sources, allocations) = crate::test_alloc::allocation_count(|| {
            selected_snapshot(&entries, |source| Ok(!source.seen_in(&seen)?))
        });
        assert!(sources.unwrap().is_empty());
        assert_eq!(
            allocations, 1,
            "a repeated snapshot copies no source strings"
        );
    }

    #[test]
    fn selecting_snapshots_preserves_lossy_identity_host_rules_and_order() {
        let mut lossy = named(c"STUDIO \xff (Camera)".as_ptr());
        lossy.__bindgen_anon_1.p_url_address = c"http://camera.local:8080/path".as_ptr();
        let mut addressed = named(ptr::null());
        addressed.__bindgen_anon_1.p_url_address = c"192.0.2.7:5961".as_ptr();
        let mut missing_address = named(c"STUDIO (Camera)".as_ptr());
        missing_address.__bindgen_anon_1.p_url_address = ptr::null();
        let mut ipv6 = named(c"OTHER (IPv6)".as_ptr());
        ipv6.__bindgen_anon_1.p_url_address = c"http://[2001:db8::1]:8080/path".as_ptr();
        let entries = [
            lossy,
            addressed,
            missing_address,
            lossy,
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
                selected_snapshot(&entries, |source| Ok(source.name.equals(name))).unwrap();
            assert_eq!(actual, expected);
        }
        for host in [
            "studio",
            "STUDIO \u{fffd}",
            "camera.local",
            "192.0.2.7",
            "[2001:db8::1]",
            "missing",
        ] {
            let query = super::super::HostQuery::checked(host).unwrap();
            let expected = copied
                .iter()
                .filter(|source| source.matches_query(query))
                .cloned()
                .collect::<Vec<_>>();
            let actual = selected_snapshot(&entries, |source| source.matches_query(query)).unwrap();
            assert_eq!(actual, expected, "{host}");
        }
        let seen = copied
            .iter()
            .cloned()
            .map(super::super::SeenSource)
            .collect();
        assert!(
            selected_snapshot(&entries, |source| Ok(!source.seen_in(&seen)?))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn omitted_selection_keeps_ordinary_copy_charges_and_errors() {
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
            if name.is_null() && !address.is_null() {
                old.empty_source_name().unwrap();
            }
            // SAFETY: each test descriptor points to live terminated literals.
            unsafe {
                super::super::decode_source(name, address, |pointer| old.text(pointer)).unwrap();
                assert!(super::super::snapshot_selected_source(
                    name,
                    address,
                    &mut selected,
                    |_| Ok(false)
                )
                .unwrap()
                .is_none());
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
                    super::super::decode_source(name, address, |pointer| old.text(pointer))
                        .map(|_| ()),
                    super::super::snapshot_selected_source(name, address, &mut selected, |_| {
                        Ok(false)
                    })
                    .map(|_| ()),
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
            let actual = selected_snapshot(&entries, |_| Ok(false)).map(|_| ());
            assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
        }
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
        assert_eq!(copied.as_str(), bytes.to_string_lossy());
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
        let largest = CString::new(vec![b'x'; MAX_SDK_STRING_BYTES - 1]).unwrap();
        assert_eq!(
            snapshot(&[named(largest.as_ptr())]).unwrap()[0]
                .name()
                .len(),
            MAX_SDK_STRING_BYTES - 1
        );
    }
}
