//! Stable owned descriptors for metadata replayed on later connections.

use crate::{
    frames::MetadataPartsExt, sys::bindings::NDIlib_metadata_frame_t, MetadataFrame, MetadataSource,
};

struct Registration {
    raw: NDIlib_metadata_frame_t,
    // The descriptor points into this frame's immutable text allocation.
    _frame: MetadataFrame,
}

// SAFETY: each registration owns its descriptor and immutable UTF-8 text. The
// raw pointer refers only to that owned allocation, which moves neither when
// the registration is boxed nor when its owning collection moves between threads.
unsafe impl Send for Registration {}
// SAFETY: shared access cannot change the descriptor or its owned text.
unsafe impl Sync for Registration {}

/// Retain each descriptor and its text until native clear returns, or until
/// the native owner is destroyed. Registration and clearing run their native
/// calls here so the SDK cannot observe released storage, even if a call unwinds.
#[derive(Default)]
pub(crate) struct ConnectionMetadata {
    // Boxes keep descriptor addresses stable while new entries grow the list.
    #[allow(clippy::vec_box)]
    entries: Vec<Box<Registration>>,
}

impl ConnectionMetadata {
    pub(crate) fn add<S: MetadataSource + ?Sized, R>(
        &mut self,
        metadata: &S,
        native: impl FnOnce(*const NDIlib_metadata_frame_t) -> R,
    ) -> R {
        let frame = metadata.to_owned();
        let raw = frame.metadata_raw();
        let index = self.entries.len();
        self.entries
            .push(Box::new(Registration { raw, _frame: frame }));
        native(std::ptr::from_ref(&self.entries[index].raw))
    }

    pub(crate) fn clear<R>(&mut self, native: impl FnOnce() -> R) -> R {
        let result = native();
        self.entries.clear();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ffi::CStr,
        panic::{catch_unwind, AssertUnwindSafe},
        ptr,
    };

    unsafe fn text<'a>(descriptor: *const NDIlib_metadata_frame_t) -> &'a CStr {
        // SAFETY: each test retains the registration while reading its descriptor.
        unsafe { CStr::from_ptr((*descriptor).p_data) }
    }

    #[test]
    fn add_retains_descriptor_and_text_after_input_drop_and_collection_growth() {
        let mut registrations = ConnectionMetadata::default();
        let input = MetadataFrame::new("<first/>").unwrap();
        let mut first = ptr::null();
        let result = registrations.add(&input, |descriptor| {
            // SAFETY: add stores the copied registration before invoking this call.
            assert_eq!(unsafe { text(descriptor) }, c"<first/>");
            first = descriptor;
            17
        });
        assert_eq!(result, 17);
        drop(input);

        for _ in 0..32 {
            registrations.add(&MetadataFrame::new("<later/>").unwrap(), |descriptor| {
                // SAFETY: the collection owns both descriptors and their text.
                unsafe {
                    assert_eq!(text(descriptor), c"<later/>");
                    assert_eq!(text(first), c"<first/>");
                }
            });
        }
        let mut moved = registrations;
        moved.clear(|| {
            // SAFETY: moving the collection and adding entries preserve the boxes.
            assert_eq!(unsafe { text(first) }, c"<first/>");
        });
    }

    #[test]
    fn clear_retains_storage_through_native_call_then_releases_entries() {
        let mut registrations = ConnectionMetadata::default();
        let descriptor = registrations.add(&MetadataFrame::new("<clear/>").unwrap(), |p| p);
        let result = registrations.clear(|| {
            // SAFETY: clear must keep both the descriptor and text live until return.
            assert_eq!(unsafe { text(descriptor) }, c"<clear/>");
            23
        });
        assert_eq!(result, 23);
        assert!(registrations.entries.is_empty());
    }

    #[test]
    fn unwinding_clear_keeps_registered_descriptors_and_text() {
        let mut registrations = ConnectionMetadata::default();
        let descriptor = registrations.add(&MetadataFrame::new("<retained/>").unwrap(), |p| p);
        assert!(catch_unwind(AssertUnwindSafe(|| {
            registrations.clear(|| panic!("native clear did not return"));
        }))
        .is_err());
        assert_eq!(registrations.entries.len(), 1);
        // SAFETY: an unwinding clear must retain the registration for a later call.
        assert_eq!(unsafe { text(descriptor) }, c"<retained/>");
        registrations.clear(|| {
            // SAFETY: the successful clear still owns the previous registration.
            assert_eq!(unsafe { text(descriptor) }, c"<retained/>");
        });
        assert!(registrations.entries.is_empty());
    }

    #[test]
    fn unwinding_add_keeps_the_descriptor_it_passed_to_native_code() {
        let mut registrations = ConnectionMetadata::default();
        let mut descriptor = ptr::null();
        assert!(catch_unwind(AssertUnwindSafe(|| {
            registrations.add(&MetadataFrame::new("<retained/>").unwrap(), |p| {
                descriptor = p;
                panic!("native add did not return");
            });
        }))
        .is_err());
        assert_eq!(registrations.entries.len(), 1);
        registrations.clear(|| {
            // SAFETY: storage was committed before the failed native add began.
            assert_eq!(unsafe { text(descriptor) }, c"<retained/>");
        });
    }
}
