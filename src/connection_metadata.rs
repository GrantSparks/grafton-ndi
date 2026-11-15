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

/// Retain both the descriptor address and its text until native clear/destroy.
#[derive(Default)]
pub(crate) struct ConnectionMetadata {
    // Boxes keep descriptor addresses stable while new entries grow the list.
    #[allow(clippy::vec_box)]
    entries: Vec<Box<Registration>>,
}

impl ConnectionMetadata {
    pub(crate) fn add<S: MetadataSource + ?Sized>(
        &mut self,
        metadata: &S,
    ) -> &NDIlib_metadata_frame_t {
        let frame = metadata.to_owned();
        let raw = frame.metadata_raw();
        let index = self.entries.len();
        self.entries
            .push(Box::new(Registration { raw, _frame: frame }));
        &self.entries[index].raw
    }

    /// The native owner must clear its registrations before releasing ours.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}
