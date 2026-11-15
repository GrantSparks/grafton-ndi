//! The metadata sources advertise to a finder.

#[cfg(any(test, ndi_has_advanced_source_metadata))]
use std::ffi::c_char;

use crate::{
    sys::advanced::{native, SourceMetadata},
    Finder, Result, Source,
};

#[cfg(any(test, ndi_has_advanced_source_metadata))]
use crate::finder::{decode_source, SnapshotBudget};

/// A discovered source together with the metadata it advertises.
///
/// A sender advertises metadata by naming it in the `send.metadata` property
/// of its configuration, and a finder is told it by a Discovery Server.
///
/// Returned by [`Finder::sources_with_metadata`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExtendedSource {
    source: Source,
    metadata: Option<String>,
}

impl ExtendedSource {
    /// The source, usable wherever a discovered [`Source`] is.
    pub fn source(&self) -> &Source {
        &self.source
    }

    /// The metadata the source advertises, or `None` when it advertises
    /// none. Bytes that are not valid UTF-8 are replaced.
    pub fn metadata(&self) -> Option<&str> {
        self.metadata.as_deref()
    }

    /// Take the source, discarding the metadata.
    pub fn into_source(self) -> Source {
        self.source
    }
}

/// Copy one descriptor of a metadata snapshot. A descriptor the plain
/// snapshot would leave out is skipped, as is one whose metadata is longer
/// than the 4 MiB one SDK string may be.
///
/// # Safety
///
/// Every non-null argument is a NUL-terminated string that stays readable
/// until this returns.
#[cfg(any(test, ndi_has_advanced_source_metadata))]
unsafe fn extended_source(
    name: *const c_char,
    address: *const c_char,
    metadata: *const c_char,
    budget: &mut SnapshotBudget,
) -> Result<Option<ExtendedSource>> {
    // SAFETY: the caller keeps the strings readable while the snapshot policy
    // copies them, omitting malformed entries without losing their scan charges.
    let Some(source) = (unsafe { decode_source(name, address, |pointer| budget.text(pointer))? })
    else {
        return Ok(None);
    };
    let metadata = if metadata.is_null() {
        None
    } else {
        // SAFETY: non-null was checked; the caller guarantees the terminator.
        let Some(text) = (unsafe { budget.text(metadata)? }) else {
            return Ok(None);
        };
        // The SDK gives a source that advertises nothing an empty string.
        (!text.is_empty()).then_some(text)
    };
    Ok(Some(ExtendedSource { source, metadata }))
}

#[cfg(ndi_has_advanced_source_metadata)]
impl crate::finder::snapshot_sealed::Sealed for SourceMetadata {}

// SAFETY: the SDK's v2 snapshot obeys the same descriptor/string lifetime
// contract as v1. The copier includes metadata in the shared byte budget.
#[cfg(ndi_has_advanced_source_metadata)]
unsafe impl crate::finder::FinderSnapshot for SourceMetadata {
    type Descriptor = crate::sys::advanced_bindings::NDIlib_source_v2_t;
    type Item = ExtendedSource;

    unsafe fn fetch(
        &self,
        instance: crate::sys::bindings::NDIlib_find_instance_t,
        count: &mut u32,
    ) -> *const Self::Descriptor {
        // SAFETY: the private snapshot guard supplies a live serialized finder.
        unsafe { self.NDIlib_find_get_current_sources_v2(instance, count) }
    }

    unsafe fn copy(
        &self,
        source: &Self::Descriptor,
        budget: &mut SnapshotBudget,
    ) -> Result<Option<ExtendedSource>> {
        // SAFETY: the guard keeps this snapshot and all its strings live.
        unsafe {
            extended_source(
                source.p_ndi_name,
                source.p_url_address,
                source.p_metadata,
                budget,
            )
        }
    }
}

impl Finder {
    /// The sources currently known, each with the metadata it advertises.
    ///
    /// This is [`current_sources`](Self::current_sources) with metadata: a
    /// snapshot, copied before the SDK can replace it.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// without an Advanced runtime.
    /// [`crate::Error::ResourceLimit`] when the descriptor count or total work
    /// exceeds the limits of [`Self::current_sources`]. Advertised metadata
    /// is included in that same byte budget, including lossy UTF-8 expansion.
    #[doc(alias = "NDIlib_find_get_current_sources_v2")]
    pub fn sources_with_metadata(&self) -> Result<Vec<ExtendedSource>> {
        let table = self.runtime().table::<SourceMetadata>()?;
        native!(SourceMetadata, table => {
            self.snapshot(table)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::CStr;

    use super::*;
    use crate::frames::MAX_SDK_STRING_BYTES;

    #[test]
    fn one_unusable_descriptor_does_not_hide_the_others() {
        let name = c"HOST (Camera)";
        let address = c"192.0.2.7:5961";
        let invalid_utf8 = c"<ndi \xff/>";
        let copy = |name: Option<&CStr>, address: Option<&CStr>, metadata: Option<&CStr>| {
            let pointer = |text: Option<&CStr>| text.map_or(std::ptr::null(), CStr::as_ptr);
            // SAFETY: every pointer is null or a live C string literal.
            unsafe {
                extended_source(
                    pointer(name),
                    pointer(address),
                    pointer(metadata),
                    &mut SnapshotBudget::new(),
                )
            }
            .unwrap()
        };

        // A source without metadata, as a null pointer and as the empty
        // string the SDK gives.
        for nothing in [None, Some(c"")] {
            let plain = copy(Some(name), Some(address), nothing).unwrap();
            assert_eq!(plain.source().name(), "HOST (Camera)");
            assert_eq!(plain.metadata(), None);
        }

        let lossy = copy(Some(name), Some(address), Some(invalid_utf8)).unwrap();
        assert_eq!(lossy.metadata(), Some("<ndi \u{fffd}/>"));
        assert_eq!(lossy.into_source().name(), "HOST (Camera)");

        // Neither a name nor an address: there is nothing to connect to.
        assert!(copy(None, None, Some(invalid_utf8)).is_none());

        // A name, address or metadata whose terminator lies past the string limit.
        let mut too_long = vec![b'x'; MAX_SDK_STRING_BYTES + 1];
        too_long[MAX_SDK_STRING_BYTES] = 0;
        let too_long = CStr::from_bytes_with_nul(&too_long).unwrap();
        assert!(copy(Some(too_long), Some(address), None).is_none());
        assert!(copy(Some(name), Some(too_long), None).is_none());
        assert!(copy(Some(name), Some(address), Some(too_long)).is_none());
    }
}
