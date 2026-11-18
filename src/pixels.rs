//! Storage that keeps each validated media layout with its initialized elements.

use std::mem::MaybeUninit;

use crate::{
    frames::{
        AudioChannels, AudioParts, FrameMetadataRef, Initialized, ValidatedAudioLayout,
        ValidatedVideoLayout, VideoHeader, VideoParts, VideoPlanes,
    },
    Rejected, Result, ScanType, SdkTimestamp,
};

mod sealed {
    pub trait Layout {}
    pub trait Source {}
}

/// A sealed layout whose element count and storage error describe one medium.
pub(crate) trait PackedLayout: sealed::Layout + Copy + std::fmt::Debug {
    type Element: Copy + Default + std::fmt::Debug;
    fn len(self) -> usize;
    fn length_error(self, actual: usize) -> crate::Error;
}

impl sealed::Layout for ValidatedVideoLayout {}
impl PackedLayout for ValidatedVideoLayout {
    type Element = u8;
    fn len(self) -> usize {
        self.data_len_bytes()
    }
    fn length_error(self, actual: usize) -> crate::Error {
        crate::Error::InvalidFrame(crate::FrameDefect::VideoDataLength {
            actual,
            expected: self.data_len_bytes(),
        })
    }
}
impl sealed::Layout for ValidatedAudioLayout {}
impl PackedLayout for ValidatedAudioLayout {
    type Element = f32;
    fn len(self) -> usize {
        self.sample_count()
    }
    fn length_error(self, actual: usize) -> crate::Error {
        crate::Error::InvalidFrame(crate::FrameDefect::AudioDataLength {
            detail: crate::CompactDiagnostic::new(crate::AudioLengthDefect {
                actual,
                expected: self.sample_count(),
                channels: self.num_channels(),
                samples: self.num_samples(),
            }),
        })
    }
}

/// A snapshot of one source layout and its active-element views.
pub(crate) trait PackedSource: sealed::Source {
    type Layout: PackedLayout;
    fn layout(&self) -> Self::Layout;
    fn tail(&self) -> usize;
    fn write(self, elements: &mut Vec<<Self::Layout as PackedLayout>::Element>);
}

/// A frame's layout, initialized elements, and zeroed SDK tail in one allocation.
/// Private fields prevent a layout from being installed without its exact storage.
#[derive(Debug)]
pub(crate) struct PackedStorage<L: PackedLayout> {
    layout: L,
    elements: Vec<L::Element>,
    tail: usize,
}

impl<L: PackedLayout> PackedStorage<L> {
    pub(crate) fn check_len(layout: L, len: usize) -> Result<()> {
        if len == layout.len() {
            Ok(())
        } else {
            Err(layout.length_error(len))
        }
    }

    pub(crate) fn zeroed(layout: L, tail: usize) -> Self {
        let mut elements = vec![L::Element::default(); layout.len().saturating_add(tail)];
        elements.truncate(layout.len());
        Self {
            layout,
            elements,
            tail,
        }
    }

    pub(crate) fn adopt(
        layout: L,
        mut elements: Vec<L::Element>,
        tail: usize,
    ) -> std::result::Result<Self, Rejected<Vec<L::Element>>> {
        if let Err(error) = Self::check_len(layout, elements.len()) {
            return Err(Rejected::new(error, elements));
        }
        seal(&mut elements, tail);
        Ok(Self {
            layout,
            elements,
            tail,
        })
    }

    pub(crate) fn swap(
        &mut self,
        elements: Vec<L::Element>,
        tail: usize,
    ) -> std::result::Result<Vec<L::Element>, Rejected<Vec<L::Element>>> {
        let replacement = Self::adopt(self.layout, elements, tail)?;
        Ok(std::mem::replace(self, replacement).into_vec())
    }

    pub(crate) fn copied(source: impl PackedSource<Layout = L>) -> Self {
        let layout = source.layout();
        let tail = source.tail();
        let mut elements = Vec::with_capacity(layout.len().saturating_add(tail));
        refill(
            &mut elements,
            layout.len(),
            L::Element::default(),
            |_| {},
            |elements| source.write(elements),
        );
        seal(&mut elements, tail);
        Self {
            layout,
            elements,
            tail,
        }
    }

    /// Reuse only storage which already holds the snapshotted layout and tail.
    /// Otherwise fill a fresh allocation and install it after the checked fill.
    /// On unwind the previous layout still covers initialized elements; their
    /// contents are unspecified, and the previous SDK tail remains zero.
    pub(crate) fn refill_from(&mut self, source: impl PackedSource<Layout = L>) {
        let layout = source.layout();
        let tail = source.tail();
        if self.elements.capacity() < layout.len().saturating_add(tail) {
            *self = Self::copied(source);
        } else {
            let previous_tail = self.tail;
            refill(
                &mut self.elements,
                layout.len(),
                L::Element::default(),
                |elements| seal(elements, previous_tail),
                |elements| source.write(elements),
            );
            seal(&mut self.elements, tail);
            self.layout = layout;
            self.tail = tail;
        }
    }

    pub(crate) fn layout(&self) -> &L {
        &self.layout
    }
    pub(crate) fn into_vec(self) -> Vec<L::Element> {
        self.elements
    }
    pub(crate) fn as_slice(&self) -> &[L::Element] {
        &self.elements
    }
    pub(crate) fn as_mut_slice(&mut self) -> &mut [L::Element] {
        &mut self.elements
    }
    pub(crate) fn as_ptr(&self) -> *const L::Element {
        self.elements.as_ptr()
    }
    pub(crate) fn reserve_tail(&mut self, tail: usize) {
        if tail > self.tail {
            seal(&mut self.elements, tail);
            self.tail = tail;
        }
    }
    pub(crate) fn room(&self) -> usize {
        self.tail
    }
    #[cfg(any(test, feature = "advanced"))]
    pub(crate) fn capacity(&self) -> usize {
        self.elements.capacity()
    }
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.elements.len()
    }
    #[cfg(test)]
    pub(crate) fn room_bytes(&self) -> &[L::Element] {
        // SAFETY: sealing initializes tail spare elements and all mutators
        // preserve that extent. The shared borrow prevents changes.
        unsafe { std::slice::from_raw_parts(self.as_ptr().add(self.elements.len()), self.tail) }
    }
}

impl<L: PackedLayout> Clone for PackedStorage<L> {
    fn clone(&self) -> Self {
        let mut elements = Vec::with_capacity(self.elements.len().saturating_add(self.tail));
        elements.extend_from_slice(&self.elements);
        seal(&mut elements, self.tail);
        Self {
            layout: self.layout,
            elements,
            tail: self.tail,
        }
    }
}

fn seal<T: Copy + Default>(elements: &mut Vec<T>, tail: usize) {
    elements.reserve_exact(tail);
    elements.spare_capacity_mut()[..tail].fill(MaybeUninit::new(T::default()));
}

/// Video views derived from exactly one call to the source layout accessor.
pub(crate) struct VideoCopy<'a> {
    layout: ValidatedVideoLayout,
    planes: VideoPlanes<'a>,
    tail: usize,
}
impl<'a> VideoCopy<'a> {
    pub(crate) fn new(source: &'a (impl VideoParts + ?Sized), scan: ScanType) -> Self {
        let source_layout = source.layout();
        let layout = source_layout.packed();
        // SAFETY: VideoParts lends initialized active rows for the borrow.
        let planes = unsafe { source_layout.view_planes(source.pixels()) };
        Self {
            layout,
            planes,
            tail: send_room(&layout, scan),
        }
    }
}
impl sealed::Source for VideoCopy<'_> {}
impl PackedSource for VideoCopy<'_> {
    type Layout = ValidatedVideoLayout;
    fn layout(&self) -> Self::Layout {
        self.layout
    }
    fn tail(&self) -> usize {
        self.tail
    }
    fn write(self, elements: &mut Vec<u8>) {
        for plane in self.planes {
            match plane.data() {
                Some(active) => elements.extend_from_slice(active),
                None => plane.rows().for_each(|row| elements.extend_from_slice(row)),
            }
        }
    }
}

/// Audio views derived from exactly one call to the source layout accessor.
pub(crate) struct AudioCopy<'a> {
    layout: ValidatedAudioLayout,
    channels: AudioChannels<'a>,
}
impl<'a> AudioCopy<'a> {
    pub(crate) fn new(source: &'a (impl AudioParts + ?Sized)) -> Self {
        let source_layout = source.layout();
        // SAFETY: AudioParts lends initialized aligned active channels.
        let channels = unsafe { source_layout.channels(source.samples()) };
        Self {
            layout: source_layout.packed(),
            channels,
        }
    }
}
impl sealed::Source for AudioCopy<'_> {}
impl PackedSource for AudioCopy<'_> {
    type Layout = ValidatedAudioLayout;
    fn layout(&self) -> Self::Layout {
        self.layout
    }
    fn tail(&self) -> usize {
        0
    }
    fn write(self, elements: &mut Vec<f32>) {
        for channel in self.channels {
            elements.extend_from_slice(channel);
        }
    }
}

/// Packed video whose fields are private to the checked copy kernel.
pub(crate) struct PackedVideo<'a> {
    storage: &'a PackedStorage<ValidatedVideoLayout>,
    header: &'a VideoHeader,
    metadata: FrameMetadataRef<'a>,
    timestamp: Option<SdkTimestamp>,
}
// SAFETY: private fields bind the pixels to their storage's own layout and
// initialized zero tail. The source's header and metadata remain borrowed.
unsafe impl VideoParts for PackedVideo<'_> {
    fn layout(&self) -> &ValidatedVideoLayout {
        self.storage.layout()
    }
    fn header(&self) -> &VideoHeader {
        self.header
    }
    fn pixels(&self) -> *const u8 {
        self.storage.as_ptr()
    }
    fn frame_metadata(&self) -> FrameMetadataRef<'_> {
        self.metadata
    }
    fn sdk_timestamp(&self) -> Option<SdkTimestamp> {
        self.timestamp
    }
    fn initialized(&self) -> Initialized {
        Initialized::Storage {
            room_after: self.storage.room(),
        }
    }
}

pub(crate) fn stage_video<'a>(
    source: &'a (impl VideoParts + ?Sized),
    staging: &'a mut Option<PackedStorage<ValidatedVideoLayout>>,
) -> PackedVideo<'a> {
    let header = source.header();
    let metadata = source.frame_metadata();
    let timestamp = source.sdk_timestamp();
    let copy = VideoCopy::new(source, header.scan_type());
    let storage = staged(copy, staging);
    PackedVideo {
        storage,
        header,
        metadata,
        timestamp,
    }
}
fn staged<L: PackedLayout>(
    copy: impl PackedSource<Layout = L>,
    staging: &mut Option<PackedStorage<L>>,
) -> &PackedStorage<L> {
    let required = copy.layout().len().saturating_add(copy.tail());
    if staging
        .as_ref()
        .is_some_and(|storage| storage.elements.capacity() < required)
    {
        // A replacement frees old staging before allocating. Failed filling
        // leaves None, so discarded storage has no surviving descriptor.
        drop(staging.take());
    }
    match staging {
        Some(storage) => {
            storage.refill_from(copy);
            storage
        }
        None => staging.insert(PackedStorage::copied(copy)),
    }
}

/// Refill reusable storage with exactly the checked layout's active elements.
///
/// A failed write restores its previous initialized length, then repairs any
/// storage-specific invariant (the zero SDK tail for pixels). The write may
/// append elements but must not shrink the allocation.
fn refill<T: Copy>(
    data: &mut Vec<T>,
    expected_len: usize,
    zero: T,
    repair: impl FnMut(&mut Vec<T>),
    write: impl FnOnce(&mut Vec<T>),
) {
    struct Restore<'a, T: Copy, F: FnMut(&mut Vec<T>)> {
        data: &'a mut Vec<T>,
        len: usize,
        zero: T,
        repair: F,
        complete: bool,
    }
    impl<T: Copy, F: FnMut(&mut Vec<T>)> Drop for Restore<'_, T, F> {
        fn drop(&mut self) {
            if !self.complete {
                // Appending cannot shrink capacity, so the previous length
                // still fits even when the write or its exact-length check fails.
                self.data.resize(self.len, self.zero);
                (self.repair)(self.data);
            }
        }
    }
    let len = data.len();
    let mut restore = Restore {
        data,
        len,
        zero,
        repair,
        complete: false,
    };
    restore.data.clear();
    write(restore.data);
    assert_eq!(
        restore.data.len(),
        expected_len,
        "packed copy must initialize its complete layout"
    );
    restore.complete = true;
}

/// The readable bytes the SDK needs after the last pixel of a frame of
/// `layout` that it sends with `scan`.
///
/// It is the format's [room](crate::PixelFormat::send_tail_room), and two
/// rows more for an interlaced or single-field frame whose height is not a
/// multiple of four: the x86 SDK reads as far as the end of the row after
/// next past such UYVY frames, and never past a height that is a multiple of
/// four by more than the format's room. The conditions it does so under,
/// which include the frame's area, are not modeled: every such frame gets the
/// rows.
pub(crate) fn send_room(layout: &ValidatedVideoLayout, scan: ScanType) -> usize {
    let room = layout.pixel_format().send_tail_room();
    if room == 0 || scan == ScanType::Progressive || layout.height().is_multiple_of(4) {
        room
    } else {
        room + 2 * layout.line_stride_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(width: u32) -> ValidatedVideoLayout {
        ValidatedVideoLayout::new_uncompressed(crate::PixelFormat::Uyvy, width, 2, None).unwrap()
    }
    fn audio() -> ValidatedAudioLayout {
        ValidatedAudioLayout::new_uncompressed(crate::AudioFormat::Fltp, 48_000, 2, 4, None)
            .unwrap()
    }
    struct Fill<L, F> {
        layout: L,
        tail: usize,
        write: F,
    }
    impl<L, F> sealed::Source for Fill<L, F> {}
    impl<L: PackedLayout, F: FnOnce(&mut Vec<L::Element>)> PackedSource for Fill<L, F> {
        type Layout = L;
        fn layout(&self) -> L {
            self.layout
        }
        fn tail(&self) -> usize {
            self.tail
        }
        fn write(self, elements: &mut Vec<L::Element>) {
            (self.write)(elements)
        }
    }

    #[test]
    fn overflowing_tail_sizes_never_create_or_damage_paired_storage() {
        let layout = video(4);
        assert!(std::panic::catch_unwind(|| PackedStorage::zeroed(layout, usize::MAX)).is_err());
        let mut storage = PackedStorage::zeroed(layout, 64);
        let pointer = storage.as_ptr();
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            storage.refill_from(Fill {
                layout: video(2),
                tail: usize::MAX,
                write: |bytes: &mut Vec<_>| bytes.resize(video(2).len(), 0),
            });
        }));
        assert!(failed.is_err());
        assert_eq!(storage.as_ptr(), pointer);
        assert_eq!(storage.len(), layout.len());
        assert_eq!(storage.room_bytes(), vec![0; 64]);
    }

    #[test]
    fn every_storage_constructor_pairs_its_layout_and_initialized_elements() {
        fn check<L: PackedLayout>(layout: L, tail: usize) {
            let zeroed = PackedStorage::zeroed(layout, tail);
            assert_eq!(zeroed.as_slice().len(), zeroed.layout().len());
            assert_eq!(zeroed.room_bytes().len(), tail);
            let adopted = PackedStorage::adopt(layout, zeroed.into_vec(), tail).unwrap();
            assert_eq!(adopted.as_slice().len(), adopted.layout().len());
            let cloned = adopted.clone();
            assert_eq!(cloned.as_slice().len(), cloned.layout().len());
            assert_eq!(cloned.room_bytes().len(), tail);
            assert!(PackedStorage::adopt(layout, Vec::new(), tail).is_err());
        }
        check(video(4), 64);
        check(audio(), 0);
    }

    #[test]
    fn storage_adoption_and_refilling_reuse_capacity_without_allocating() {
        let layout = video(4);
        let mut elements = Vec::with_capacity(layout.len() + 64);
        elements.resize(layout.len(), 7);
        let pointer = elements.as_ptr();
        let (mut storage, allocations) = crate::test_alloc::allocation_count(|| {
            PackedStorage::adopt(layout, elements, 64).unwrap()
        });
        assert_eq!(allocations, 0);
        assert_eq!(storage.as_ptr(), pointer);
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            storage.refill_from(Fill {
                layout,
                tail: 64,
                write: |bytes: &mut Vec<_>| bytes.resize(layout.len(), 9),
            });
        });
        assert_eq!(allocations, 0);
        assert_eq!(storage.as_ptr(), pointer);
        assert_eq!(storage.as_slice(), vec![9; layout.len()]);
        assert_eq!(storage.room_bytes(), vec![0; 64]);
    }

    #[test]
    fn a_failed_refill_preserves_the_previous_pairing_for_both_media() {
        fn check<L: PackedLayout>(layout: L, tail: usize) {
            let mut storage = PackedStorage::zeroed(layout, tail);
            let pointer = storage.as_ptr();
            let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                storage.refill_from(Fill {
                    layout,
                    tail,
                    write: |data: &mut Vec<_>| data.push(L::Element::default()),
                });
            }));
            assert!(failed.is_err());
            assert_eq!(format!("{:?}", storage.layout()), format!("{layout:?}"));
            assert_eq!(storage.as_slice().len(), layout.len());
            assert_eq!(storage.as_ptr(), pointer);
        }
        check(video(4), 64);
        check(audio(), 0);
    }

    #[test]
    fn a_failed_larger_refill_keeps_the_previous_allocation_and_layout() {
        let previous = video(4);
        let larger = video(64);
        let mut storage = PackedStorage::zeroed(previous, 64);
        let pointer = storage.as_ptr();
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            storage.refill_from(Fill {
                layout: larger,
                tail: 64,
                write: |bytes: &mut Vec<_>| bytes.push(7),
            });
        }));
        assert!(failed.is_err());
        assert_eq!(format!("{:?}", storage.layout()), format!("{previous:?}"));
        assert_eq!(storage.as_slice().len(), previous.len());
        assert_eq!(storage.as_ptr(), pointer);
        assert_eq!(storage.room_bytes(), vec![0; 64]);
    }

    #[test]
    fn a_failed_staging_replacement_leaves_no_stale_storage() {
        let mut staging = Some(PackedStorage::zeroed(video(4), 64));
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = staged(
                Fill {
                    layout: video(64),
                    tail: 64,
                    write: |bytes: &mut Vec<_>| bytes.push(7),
                },
                &mut staging,
            );
        }));
        assert!(failed.is_err());
        assert!(staging.is_none());
    }

    #[test]
    fn an_unwinding_reused_fill_restores_the_previous_zero_tail() {
        let previous = video(4);
        let larger = video(8);
        let mut storage = PackedStorage::zeroed(previous, 64);
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            storage.refill_from(Fill {
                layout: larger,
                tail: 32,
                write: |bytes: &mut Vec<_>| {
                    bytes.resize(30, 255);
                    panic!("interrupted copy");
                },
            });
        }));
        assert!(failed.is_err());
        assert_eq!(format!("{:?}", storage.layout()), format!("{previous:?}"));
        assert_eq!(storage.as_slice().len(), previous.len());
        assert_eq!(storage.room_bytes(), vec![0; 64]);
    }
}
