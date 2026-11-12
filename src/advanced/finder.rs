//! The metadata sources advertise to a finder.

use std::ffi::{c_char, CStr};

use crate::{
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
#[derive(Clone, Debug)]
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

/// Copy one descriptor of a metadata snapshot. A descriptor that names no
/// source is skipped, as the plain snapshot does.
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
    let mut descriptor = NDIlib_source_t {
        p_ndi_name: name,
        ..Default::default()
    };
    descriptor.__bindgen_anon_1.p_url_address = address;
    // SAFETY: the descriptor points to the caller's readable strings.
    let source = unsafe { Source::try_from_raw(&descriptor) }.ok()?;
    let metadata = (!metadata.is_null())
        // SAFETY: non-null was checked; the caller guarantees the terminator.
        .then(|| unsafe { CStr::from_ptr(metadata) })
        // The SDK gives a source that advertises nothing an empty string.
        .filter(|text| !text.is_empty())
        .map(|text| text.to_string_lossy().into_owned());
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
        Ok(self.snapshot(|instance| {
            native!(SourceMetadata, table => {
                let mut count = 0;
                // SAFETY: the finder is live, and the snapshot lock is held
                // until everything the SDK returns has been copied.
                let sources =
                    unsafe { table.NDIlib_find_get_current_sources_v2(instance, &mut count) };
                if sources.is_null() {
                    return Vec::new();
                }
                // SAFETY: the SDK returned `count` descriptors that stay valid
                // until the next snapshot call, which the lock excludes.
                unsafe { std::slice::from_raw_parts(sources, count as usize) }
                    .iter()
                    .filter_map(|source| {
                        // SAFETY: the descriptor's strings are valid for as
                        // long as the snapshot is.
                        unsafe {
                            extended_source(
                                source.p_ndi_name,
                                source.p_url_address,
                                source.p_metadata,
                            )
                        }
                    })
                    .collect()
            })
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            assert_eq!(plain.source().name, "HOST (Camera)");
            assert_eq!(plain.metadata(), None);
        }

        let lossy = copy(Some(name), Some(address), Some(invalid_utf8)).unwrap();
        assert_eq!(lossy.metadata(), Some("<ndi \u{fffd}/>"));
        assert_eq!(lossy.into_source().name, "HOST (Camera)");

        // Neither a name nor an address: there is nothing to connect to.
        assert!(copy(None, None, Some(invalid_utf8)).is_none());
    }
}
