//! Bounded knowledge of the files a native recorder may still own.

use super::{EventText, OpenFile, RecorderEventKind};

const MAX_FILES: usize = 16_384;
const MAX_BYTES: usize = 16 * 1024 * 1024;

/// Incomplete knowledge is itself a cleanup obligation. Closing every known
/// file cannot retire it; only destroying/replacing the native recorder can.
pub(super) enum FileTracker {
    Complete(Files),
    Incomplete(Files),
}

/// What the available lifecycle evidence proves about a closed file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileClose {
    ConfirmedEmpty,
    HadMedia,
    Unconfirmed,
}

#[derive(Default)]
pub(super) struct Files {
    // Sorted by exact path bytes, so an unmatched open does not compare its
    // path with every earlier open. Insertion/removal only moves bounded data.
    entries: Vec<OpenFile>,
    path_bytes: usize,
    next_order: u64,
}

impl Files {
    fn position(&self, path: &EventText) -> Result<usize, usize> {
        self.entries
            .binary_search_by(|open| open.path.as_bytes().cmp(path.as_bytes()))
    }
}

impl FileTracker {
    pub(super) fn new() -> Self {
        Self::Complete(Files::default())
    }

    fn data(&self) -> &Files {
        match self {
            Self::Complete(files) | Self::Incomplete(files) => files,
        }
    }

    fn data_mut(&mut self) -> &mut Files {
        match self {
            Self::Complete(files) | Self::Incomplete(files) => files,
        }
    }

    pub(super) fn mark_incomplete(&mut self) {
        if let Self::Complete(files) = self {
            *self = Self::Incomplete(std::mem::take(files));
        }
    }

    pub(super) fn is_complete(&self) -> bool {
        matches!(self, Self::Complete(_))
    }

    /// Whether there are no *known* files; this says nothing about an
    /// incomplete tracker's unknown cleanup obligation.
    pub(super) fn is_empty(&self) -> bool {
        self.data().entries.is_empty()
    }

    pub(super) fn cleanup_pending(&self) -> bool {
        !self.is_complete() || !self.is_empty()
    }

    pub(super) fn current(&self) -> Option<&EventText> {
        self.data()
            .entries
            .iter()
            .max_by_key(|open| open.order)
            .map(|open| &open.path)
    }

    #[cfg(test)]
    pub(super) fn files(&self) -> &[OpenFile] {
        &self.data().entries
    }

    fn retained_bytes(&self) -> usize {
        self.data().entries.capacity() * size_of::<OpenFile>() + self.data().path_bytes
    }

    fn exact(path: &EventText) -> bool {
        !path.as_bytes().is_empty() && path.is_terminated() && !path.is_possibly_truncated()
    }

    pub(super) fn open(&mut self, kind: RecorderEventKind, path: EventText) {
        // Once evidence was lost, accepting more unmatched paths cannot
        // reconstruct an exhaustive set. It would only retain unbounded work.
        if !self.is_complete() {
            return;
        }
        if !Self::exact(&path) || (kind == RecorderEventKind::Started && !self.is_empty()) {
            self.mark_incomplete();
            return;
        }
        let files = self.data_mut();
        let index = match files.position(&path) {
            Ok(_) => {
                self.mark_incomplete();
                return;
            }
            Err(index) => index,
        };
        let Some(next_order) = files.next_order.checked_add(1) else {
            self.mark_incomplete();
            return;
        };
        let capacity = if files.entries.len() == files.entries.capacity() {
            (files.entries.capacity().max(4) * 2).min(MAX_FILES)
        } else {
            files.entries.capacity()
        };
        let bytes = capacity * size_of::<OpenFile>() + files.path_bytes;
        if files.entries.len() == MAX_FILES
            || path.retained_bytes() > MAX_BYTES.saturating_sub(bytes)
            || files
                .entries
                .try_reserve_exact(capacity - files.entries.len())
                .is_err()
        {
            self.mark_incomplete();
            return;
        }
        // Reserve-exact requests the checked capacity. Also account for the
        // actual capacity an allocator supplies before retaining this path.
        if self.retained_bytes() + path.retained_bytes() > MAX_BYTES
            || self.data().entries.capacity() > MAX_FILES
        {
            // An unexpectedly oversized allocation cannot remain retained.
            // Unknown obligations preserve cleanup even if known paths go.
            let files = self.data_mut();
            files.entries = Vec::new();
            files.path_bytes = 0;
            self.mark_incomplete();
            return;
        }
        let files = self.data_mut();
        files.path_bytes += path.retained_bytes();
        files.entries.insert(
            index,
            OpenFile {
                path,
                has_media: false,
                order: files.next_order,
            },
        );
        files.next_order = next_order;
    }

    pub(super) fn first_frame(&mut self, path: &EventText) {
        if Self::exact(path) {
            let files = self.data_mut();
            if let Ok(index) = files.position(path) {
                files.entries[index].has_media = true;
                return;
            }
        }
        self.mark_incomplete();
    }

    /// Release a matching known file, even after the report has become final.
    /// Absence of a first frame proves emptiness only under complete tracking.
    pub(super) fn close(&mut self, path: &EventText) -> FileClose {
        let complete = self.is_complete();
        if Self::exact(path) {
            let files = self.data_mut();
            if let Ok(index) = files.position(path) {
                let closed = files.entries.remove(index);
                files.path_bytes -= closed.path.retained_bytes();
                return if closed.has_media {
                    FileClose::HadMedia
                } else if complete {
                    FileClose::ConfirmedEmpty
                } else {
                    FileClose::Unconfirmed
                };
            }
        }
        self.mark_incomplete();
        FileClose::Unconfirmed
    }
}

#[cfg(test)]
#[path = "tracker_tests.rs"]
mod tests;
