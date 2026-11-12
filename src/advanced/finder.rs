//! The metadata sources advertise to a finder.

use std::ffi::c_char;

use crate::{
    frames::copy_sdk_string,
    sys::{
        advanced::{native, SourceMetadata},
        bindings::*,
    },
    Finder, Result, Source,
};

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
unsafe fn extended_source(
    name: *const c_char,
    address: *const c_char,
    metadata: *const c_char,
) -> Option<ExtendedSource> {
    const OPERATION: &str = "NDIlib_find_get_current_sources_v2";
    let mut descriptor = NDIlib_source_t {
        p_ndi_name: name,
        ..Default::default()
    };
    descriptor.__bindgen_anon_1.p_url_address = address;
    // SAFETY: the descriptor points to the caller's readable strings.
    let source = unsafe { Source::try_from_raw(&descriptor, OPERATION) }.ok()?;
    let metadata = if metadata.is_null() {
        None
    } else {
        // SAFETY: non-null was checked; the caller guarantees the terminator.
        let text = unsafe { copy_sdk_string(metadata, OPERATION) }.ok()?;
        // The SDK gives a source that advertises nothing an empty string.
        (!text.is_empty()).then_some(text)
    };
    Some(ExtendedSource { source, metadata })
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
    pub fn sources_with_metadata(&self) -> Result<Vec<ExtendedSource>> {
        let table = self.runtime().table::<SourceMetadata>()?;
        native!(SourceMetadata, table => {
            // SAFETY: `snapshot` hands the fetch the finder's live instance,
            // for which the SDK returns null or `count` descriptors whose
            // strings stay valid until the next snapshot call; the snapshot
            // lock excludes that call until every descriptor has been copied,
            // and each copy reads only its descriptor's strings meanwhile.
            Ok(unsafe {
                self.snapshot(
                    |instance, count| table.NDIlib_find_get_current_sources_v2(instance, count),
                    |source| {
                        extended_source(source.p_ndi_name, source.p_url_address, source.p_metadata)
                    },
                )
            })
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
            unsafe { extended_source(pointer(name), pointer(address), pointer(metadata)) }
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

        // A name or metadata whose terminator lies past the string limit.
        let mut too_long = vec![b'x'; MAX_SDK_STRING_BYTES + 1];
        too_long[MAX_SDK_STRING_BYTES] = 0;
        let too_long = CStr::from_bytes_with_nul(&too_long).unwrap();
        assert!(copy(Some(too_long), Some(address), None).is_none());
        assert!(copy(Some(name), Some(address), Some(too_long)).is_none());
    }
}
