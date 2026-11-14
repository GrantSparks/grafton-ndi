//! Bounded, exact and fallible copies shared by native discovery snapshots.

use std::{
    collections::TryReserveError,
    ffi::{c_char, CStr, CString},
    mem::size_of,
};

use crate::{
    frames::{bounded_sdk_text, MAX_SDK_STRING_BYTES},
    Error, ResourceKind, Result,
};

pub(crate) const COPY_LIMIT: usize = 16 * 1024 * 1024;

/// Malformed peer data can omit one entry. Resource and allocation failures
/// instead stop the complete copy, including when found in a nested array.
#[derive(Debug)]
pub(crate) enum CopyError {
    Malformed(&'static str),
    Failed(Error),
}

pub(crate) type CopyResult<T> = std::result::Result<T, CopyError>;

impl CopyError {
    pub(crate) fn into_error(self) -> Error {
        match self {
            Self::Malformed(reason) => Error::UnexpectedSdkResult(reason.into()),
            Self::Failed(error) => error,
        }
    }
}

impl From<Error> for CopyError {
    fn from(error: Error) -> Self {
        Self::Failed(error)
    }
}

pub(crate) fn allocation_failed() -> Error {
    Error::UnexpectedSdkResult("could not reserve memory for discovery results".into())
}

/// One budget for native bytes scanned and owned storage reserved. Individual
/// malformed strings spend their scans even when their entry is omitted.
pub(crate) struct CopyBudget {
    remaining: usize,
    limit: usize,
    resource: ResourceKind,
}

impl CopyBudget {
    pub(crate) fn new(limit: usize, resource: ResourceKind) -> Self {
        Self {
            remaining: limit,
            limit,
            resource,
        }
    }

    fn exceeded(&self) -> Error {
        Error::ResourceLimit {
            resource: self.resource,
            limit: self.limit,
        }
    }

    pub(crate) fn take(&mut self, bytes: usize) -> Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(bytes)
            .ok_or_else(|| self.exceeded())?;
        Ok(())
    }

    /// Check spans before pointer arithmetic or reservation, on every target.
    pub(crate) fn span_bytes(count: usize, element_size: usize) -> CopyResult<usize> {
        count
            .checked_mul(element_size)
            .filter(|&bytes| bytes <= isize::MAX as usize)
            .ok_or(CopyError::Malformed("SDK discovery array span overflow"))
    }

    /// Charge complete owned element storage before reserving it, including
    /// elements whose contents may be omitted during conversion.
    pub(crate) fn reserve<T>(&mut self, count: usize) -> CopyResult<Vec<T>> {
        self.take(Self::span_bytes(count, size_of::<T>())?)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| allocation_failed())?;
        Ok(values)
    }

    /// Nested collections share the outer budget and propagate malformed data
    /// to the caller, which chooses the boundary of an omitted entry.
    pub(crate) fn collect<S, T>(
        &mut self,
        source: &[S],
        mut copy: impl FnMut(&S, &mut Self) -> CopyResult<Option<T>>,
    ) -> CopyResult<Vec<T>> {
        let mut values = self.reserve(source.len())?;
        for value in source {
            if let Some(value) = copy(value, self)? {
                values.push(value);
            }
        }
        Ok(values)
    }

    /// Omit malformed top-level entries while retaining their scan charges.
    pub(crate) fn collect_entries<S, T>(
        &mut self,
        source: &[S],
        mut copy: impl FnMut(&S, &mut Self) -> CopyResult<T>,
    ) -> Result<Vec<T>> {
        self.collect(source, |value, budget| match copy(value, budget) {
            Ok(value) => Ok(Some(value)),
            Err(CopyError::Malformed(_)) => Ok(None),
            Err(error) => Err(error),
        })
        .map_err(CopyError::into_error)
    }

    /// Borrow bounded SDK text without scanning its bytes a second time.
    ///
    /// # Safety
    /// A non-null `value` stays readable for `'a` through its first NUL or
    /// `MAX_SDK_STRING_BYTES`, whichever comes first.
    pub(crate) unsafe fn c_str<'a>(
        &mut self,
        value: *const c_char,
    ) -> CopyResult<Option<&'a CStr>> {
        if value.is_null() {
            return Ok(None);
        }
        let limit = self.remaining.min(MAX_SDK_STRING_BYTES);
        // SAFETY: the caller keeps the prefix readable; the budget can only
        // reduce the scan below the individual string bound.
        let Some(bytes) = (unsafe { bounded_sdk_text(value, limit) }) else {
            self.take(limit)?;
            return Err(if limit < MAX_SDK_STRING_BYTES {
                self.exceeded().into()
            } else {
                CopyError::Malformed("SDK discovery string exceeds its 4 MiB bound")
            });
        };
        self.take(bytes.len() + 1)?;
        // SAFETY: the scan found the first NUL, and the caller keeps that
        // complete prefix readable for `'a`.
        let terminated = unsafe { std::slice::from_raw_parts(value.cast(), bytes.len() + 1) };
        Ok(Some(unsafe {
            CStr::from_bytes_with_nul_unchecked(terminated)
        }))
    }

    /// Charge lossy UTF-8 expansion before allocating any owned text.
    ///
    /// # Safety
    /// As for [`Self::c_str`].
    pub(crate) unsafe fn text(&mut self, value: *const c_char) -> CopyResult<Option<String>> {
        // SAFETY: forwarded to the caller's obligations.
        let text = unsafe { self.c_str(value)? };
        text.map(|text| {
            let text = LossyText::new(text.to_bytes());
            self.take(text.len())?;
            text.copy().map_err(|_| allocation_failed().into())
        })
        .transpose()
    }

    /// Copy exact identifier bytes, including their sole trailing NUL.
    ///
    /// # Safety
    /// As for [`Self::c_str`].
    pub(crate) unsafe fn id(&mut self, value: *const c_char) -> CopyResult<Option<CString>> {
        // SAFETY: forwarded to the caller's obligations.
        let id = unsafe { self.c_str(value)? };
        id.map(|id| {
            let mut bytes = self.reserve(id.to_bytes_with_nul().len())?;
            bytes.extend_from_slice(id.to_bytes_with_nul());
            // SAFETY: these are exactly the bytes of the validated C string.
            Ok(unsafe { CString::from_vec_with_nul_unchecked(bytes) })
        })
        .transpose()
    }

    /// Borrow and charge an array. A malformed array can omit a nested entry,
    /// while callers convert the same outcome to an error at the top level.
    ///
    /// # Safety
    /// A non-null `value` points to `count` initialized elements that remain
    /// readable for `'a`.
    pub(crate) unsafe fn slice<'a, T>(
        &mut self,
        value: *const T,
        count: u32,
    ) -> CopyResult<&'a [T]> {
        if count == 0 {
            return Ok(&[]);
        }
        if value.is_null() {
            return Err(CopyError::Malformed(
                "SDK discovery array is null with a nonzero count",
            ));
        }
        let count = usize::try_from(count)
            .map_err(|_| CopyError::Malformed("SDK discovery array count overflow"))?;
        self.take(Self::span_bytes(count, size_of::<T>())?)?;
        // SAFETY: the caller provides initialized storage, and its total span
        // has passed preflight before a slice is formed.
        Ok(unsafe { std::slice::from_raw_parts(value, count) })
    }
}

/// Borrowed SDK text with the exact size of its lossy UTF-8 representation.
/// Callers charge `len()` to their snapshot budget before asking for a copy.
pub(crate) struct LossyText<'a> {
    bytes: &'a [u8],
    len: usize,
}

impl<'a> LossyText<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        let len = bytes
            .utf8_chunks()
            .map(|chunk| chunk.valid().len() + usize::from(!chunk.invalid().is_empty()) * 3)
            .sum();
        Self { bytes, len }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn copy(&self) -> std::result::Result<String, TryReserveError> {
        let mut text = String::new();
        text.try_reserve_exact(self.len)?;
        for chunk in self.bytes.utf8_chunks() {
            text.push_str(chunk.valid());
            if !chunk.invalid().is_empty() {
                text.push('\u{fffd}');
            }
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr;

    fn budget(remaining: usize) -> CopyBudget {
        let mut budget = CopyBudget::new(COPY_LIMIT, ResourceKind::DiscoveryServerCopyBytes);
        budget.remaining = remaining;
        budget
    }

    fn limit_error<T>(result: CopyResult<T>) {
        assert!(matches!(
            result,
            Err(CopyError::Failed(Error::ResourceLimit {
                resource: ResourceKind::DiscoveryServerCopyBytes,
                limit: COPY_LIMIT,
            }))
        ));
    }

    #[test]
    fn bounded_copies_preserve_null_and_empty_values() {
        let mut budget = budget(COPY_LIMIT);
        // SAFETY: every non-null pointer is a live NUL-terminated string.
        unsafe {
            assert_eq!(budget.text(ptr::null()).unwrap(), None);
            assert_eq!(budget.text(c"".as_ptr()).unwrap(), Some(String::new()));
            assert_eq!(budget.id(ptr::null()).unwrap(), None);
            assert!(matches!(
                budget.slice::<u32>(ptr::null(), 1),
                Err(CopyError::Malformed(_))
            ));
            assert!(budget.slice::<u32>(ptr::null(), 0).unwrap().is_empty());
        }
    }

    #[test]
    fn exact_charging_preserves_each_callers_resource_kind() {
        for resource in [
            ResourceKind::FinderSnapshotBytes,
            ResourceKind::DiscoveryServerCopyBytes,
        ] {
            let mut budget = CopyBudget::new(14, resource);
            // SAFETY: the literal stays live through each copy.
            unsafe {
                assert_eq!(
                    budget.text(c"abc".as_ptr()).unwrap().as_deref(),
                    Some("abc")
                );
                assert_eq!(
                    budget.text(c"abc".as_ptr()).unwrap().as_deref(),
                    Some("abc")
                );
                assert_eq!(budget.remaining, 0);
                let Err(CopyError::Failed(Error::ResourceLimit {
                    resource: reported,
                    limit: 14,
                })) = budget.text(c"a".as_ptr())
                else {
                    panic!("exhausted budget must retain its error kind");
                };
                assert_eq!(reported, resource);
            }
        }
    }

    #[test]
    fn lossy_expansion_is_charged_before_copying() {
        let mut exact = budget(5);
        // SAFETY: the literal stays live and NUL-terminated.
        let text = unsafe { exact.text(c"\xff".as_ptr()) }.unwrap().unwrap();
        assert_eq!(text, "\u{fffd}");
        assert_eq!(exact.remaining, 0);
        let (result, allocations) = crate::test_alloc::allocation_count(|| {
            // SAFETY: as above; rejection precedes allocation.
            unsafe { budget(4).text(c"\xff".as_ptr()) }
        });
        limit_error(result);
        assert_eq!(allocations, 0);

        let bytes = c"a\xff\xe2\x82\xac\xe2\x82";
        let mut mixed = budget(COPY_LIMIT);
        // SAFETY: live literal, including a NUL terminator.
        let copied = unsafe { mixed.text(bytes.as_ptr()) }.unwrap().unwrap();
        assert_eq!(copied, bytes.to_string_lossy());
        assert_eq!(
            mixed.remaining,
            COPY_LIMIT - bytes.to_bytes_with_nul().len() - copied.len()
        );
    }

    #[test]
    fn identifiers_charge_exact_bytes_including_the_nul() {
        for id in [c"", c"sender-\xff"] {
            let bytes = id.to_bytes_with_nul().len();
            let mut exact = budget(2 * bytes);
            let (copy, allocations) = crate::test_alloc::allocation_count(|| {
                // SAFETY: the literal stays live and NUL-terminated.
                unsafe { exact.id(id.as_ptr()) }.unwrap().unwrap()
            });
            assert_eq!(copy.as_c_str(), id);
            assert_eq!(exact.remaining, 0);
            assert_eq!(allocations, 1);
            let (result, allocations) = crate::test_alloc::allocation_count(|| {
                // SAFETY: as above; rejection precedes allocation.
                unsafe { budget(2 * bytes - 1).id(id.as_ptr()) }
            });
            limit_error(result);
            assert_eq!(allocations, 0);
        }
    }

    #[test]
    fn collections_charge_storage_before_converting_elements() {
        let source = [1u8, 2, 3];
        let bytes = size_of_val(&source) + source.len() * size_of::<u64>();
        let mut exact = budget(bytes);
        let (copy, allocations) = crate::test_alloc::allocation_count(|| {
            // SAFETY: the array remains live through its copy.
            let raw = unsafe { exact.slice(source.as_ptr(), source.len() as u32) }.unwrap();
            exact
                .collect(raw, |value, _| Ok(Some(u64::from(*value))))
                .unwrap()
        });
        assert_eq!(copy, [1, 2, 3]);
        assert_eq!(copy.capacity(), source.len());
        assert_eq!(exact.remaining, 0);
        assert_eq!(allocations, 1);
        let (result, allocations) = crate::test_alloc::allocation_count(|| {
            let mut short = budget(bytes - 1);
            // SAFETY: the array remains live through the attempted copy.
            let raw = unsafe { short.slice(source.as_ptr(), source.len() as u32) }.unwrap();
            short.collect::<_, u64>(raw, |_, _| panic!("preflight must reject first"))
        });
        limit_error(result);
        assert_eq!(allocations, 0);
        assert!(matches!(
            budget(COPY_LIMIT).reserve::<u64>(usize::MAX),
            Err(CopyError::Malformed(_))
        ));
        assert!(matches!(
            CopyBudget::span_bytes(2, isize::MAX as usize),
            Err(CopyError::Malformed(_))
        ));
    }

    #[test]
    fn nested_collections_and_text_share_the_outer_budget() {
        let pointers = [c"\xff".as_ptr(), ptr::null(), c"".as_ptr()];
        let bytes = size_of_val(&pointers) + pointers.len() * size_of::<String>() + 5 + 1;
        for remaining in [bytes, bytes - 1] {
            let mut budget = budget(remaining);
            // SAFETY: the pointer array and all non-null strings stay live.
            let raw = unsafe { budget.slice(pointers.as_ptr(), pointers.len() as u32) }.unwrap();
            let result = budget.collect(raw, |pointer, budget| {
                // SAFETY: each literal remains live and NUL-terminated.
                unsafe { budget.text(*pointer) }
            });
            if remaining == bytes {
                assert_eq!(result.unwrap(), ["\u{fffd}", ""]);
                assert_eq!(budget.remaining, 0);
            } else {
                limit_error(result);
            }
        }
    }

    #[test]
    fn expanded_text_cannot_allocate_past_the_snapshot_limit() {
        let text = CString::new(vec![0xff; 2048]).unwrap();
        let pointers = [text.as_ptr(); 4096];
        let (result, peak) = crate::test_alloc::peak_bytes(|| {
            let mut budget = budget(COPY_LIMIT);
            // SAFETY: the array and all strings stay live through the copy.
            unsafe {
                let raw = budget.slice(pointers.as_ptr(), pointers.len() as u32)?;
                budget.collect(raw, |pointer, budget| budget.text(*pointer))
            }
        });
        limit_error(result);
        assert!(peak <= COPY_LIMIT, "allocated {peak} bytes");
    }

    #[test]
    fn rejected_scans_stay_charged_and_short_budgets_are_fatal() {
        let mut longest = vec![b'x'; MAX_SDK_STRING_BYTES];
        longest[MAX_SDK_STRING_BYTES - 1] = 0;
        let too_long = vec![b'x'; MAX_SDK_STRING_BYTES];
        let mut full = budget(COPY_LIMIT);
        // SAFETY: each buffer remains readable through its NUL or the bound.
        unsafe {
            let copied = full.c_str(longest.as_ptr().cast()).unwrap().unwrap();
            assert_eq!(copied.to_bytes().len(), MAX_SDK_STRING_BYTES - 1);
            assert_eq!(full.remaining, COPY_LIMIT - MAX_SDK_STRING_BYTES);
            assert!(matches!(
                full.c_str(too_long.as_ptr().cast()),
                Err(CopyError::Malformed(_))
            ));
            assert_eq!(full.remaining, COPY_LIMIT - 2 * MAX_SDK_STRING_BYTES);
            let mut exact = budget(4);
            assert_eq!(exact.c_str(c"abc".as_ptr()).unwrap(), Some(c"abc"));
            assert_eq!(exact.remaining, 0);
            let mut short = budget(3);
            limit_error(short.c_str(c"abc".as_ptr()));
            assert_eq!(short.remaining, 0);
        }
    }

    #[test]
    fn omitting_malformed_entries_keeps_their_scan_cost_and_fatal_errors() {
        let too_long = vec![b'x'; MAX_SDK_STRING_BYTES];
        let pointers = [too_long.as_ptr().cast(), c"survivor".as_ptr()];
        let mut full = budget(COPY_LIMIT);
        let copied = full
            .collect_entries(&pointers, |pointer, budget| {
                // SAFETY: both strings remain readable through their bound.
                unsafe { budget.text(*pointer) }
            })
            .unwrap();
        assert_eq!(copied, [Some("survivor".into())]);
        assert_eq!(
            full.remaining,
            COPY_LIMIT - pointers.len() * size_of::<Option<String>>() - MAX_SDK_STRING_BYTES - 17
        );
        let rejected = [too_long.as_ptr().cast(); 4];
        let error = budget(COPY_LIMIT)
            .collect_entries(&rejected, |pointer, budget| {
                // SAFETY: every buffer remains readable through the bound.
                unsafe { budget.text(*pointer) }
            })
            .unwrap_err();
        assert!(matches!(
            error,
            Error::ResourceLimit {
                resource: ResourceKind::DiscoveryServerCopyBytes,
                limit: COPY_LIMIT,
            }
        ));
    }

    #[test]
    fn arrays_are_bounded_by_bytes_rather_than_element_count() {
        let many = vec![0u8; 1024 * 1024];
        let mut full = budget(COPY_LIMIT);
        // SAFETY: the vectors remain readable for the requested counts.
        unsafe {
            assert_eq!(
                full.slice(many.as_ptr(), many.len() as u32).unwrap().len(),
                many.len()
            );
            assert_eq!(full.remaining, COPY_LIMIT - many.len());
            let wide = vec![0u64; COPY_LIMIT / 8 + 1];
            assert_eq!(
                budget(COPY_LIMIT)
                    .slice(wide.as_ptr(), (wide.len() - 1) as u32)
                    .unwrap()
                    .len(),
                wide.len() - 1
            );
            limit_error(budget(COPY_LIMIT).slice(wide.as_ptr(), wide.len() as u32));
        }
    }

    #[test]
    fn exact_copies_match_standard_utf8_replacement() {
        let check = |bytes: &[u8]| {
            let expected = String::from_utf8_lossy(bytes);
            let text = LossyText::new(bytes);
            assert_eq!(text.len(), expected.len());
            assert_eq!(text.copy().unwrap(), expected);
        };
        check(b"");
        // Exhaust every two-byte prefix, including continuation bytes,
        // overlong sequences and truncated multibyte characters.
        for first in 0..=u8::MAX {
            for second in 0..=u8::MAX {
                check(&[first, second]);
            }
        }
        for text in ["ASCII", "caf\u{e9}", "\u{20ac}", "\u{1f980}"] {
            for end in 0..=text.len() {
                check(&text.as_bytes()[..end]);
            }
        }
        for bytes in [
            &b"\xed\xa0\x80"[..],                  // Surrogate.
            &b"\xf4\x90\x80\x80"[..],              // Beyond Unicode's maximum.
            &b"\xe2\x82A\xff\xf0\x9f\xa6\x80"[..], // Mixed valid and malformed chunks.
        ] {
            check(bytes);
        }
    }
}
