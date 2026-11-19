//! Storage that keeps each validated media layout with its initialized elements.

use std::mem::MaybeUninit;

use crate::{
    frames::{
        AudioChannels, AudioParts, FrameMetadataRef, Initialized, PackedAudioLayout,
        PackedVideoLayout, ValidatedVideoLayout, VideoHeader, VideoParts, VideoPlanes, VideoTiming,
    },
    ReceivedTimestamp, Rejected, Result, ScanType,
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

impl sealed::Layout for PackedVideoLayout {}
impl PackedLayout for PackedVideoLayout {
    type Element = u8;
    fn len(self) -> usize {
        self.as_layout().data_len_bytes()
    }
    fn length_error(self, actual: usize) -> crate::Error {
        crate::Error::InvalidFrame(crate::FrameDefect::VideoDataLength {
            actual,
            expected: self.as_layout().data_len_bytes(),
        })
    }
}
impl sealed::Layout for PackedAudioLayout {}
impl PackedLayout for PackedAudioLayout {
    type Element = f32;
    fn len(self) -> usize {
        self.as_layout().sample_count()
    }
    fn length_error(self, actual: usize) -> crate::Error {
        crate::Error::InvalidFrame(crate::FrameDefect::AudioDataLength {
            detail: crate::CompactDiagnostic::new(crate::AudioLengthDefect {
                actual,
                expected: self.as_layout().sample_count(),
                channels: self.as_layout().num_channels(),
                samples: self.as_layout().num_samples(),
            }),
        })
    }
}

/// A snapshot of one source layout and its active-element views.
pub(crate) trait PackedSource: sealed::Source {
    type Layout: PackedLayout;
    fn layout(&self) -> Self::Layout;
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
    fn check_len(layout: L, len: usize) -> Result<()> {
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

    /// Check the element count and any remaining input before changing the
    /// supplied allocation. On success, lend the remaining validation to the
    /// caller alongside paired storage.
    pub(crate) fn adopt<T>(
        layout: L,
        mut elements: Vec<L::Element>,
        tail: usize,
        validate: impl FnOnce() -> Result<T>,
    ) -> std::result::Result<(Self, T), Rejected<Vec<L::Element>>> {
        let checked = match Self::check_len(layout, elements.len()).and_then(|()| validate()) {
            Ok(checked) => checked,
            Err(error) => return Err(Rejected::new(error, elements)),
        };
        seal(&mut elements, tail);
        Ok((
            Self {
                layout,
                elements,
                tail,
            },
            checked,
        ))
    }

    pub(crate) fn swap(
        &mut self,
        elements: Vec<L::Element>,
        tail: usize,
    ) -> std::result::Result<Vec<L::Element>, Rejected<Vec<L::Element>>> {
        let (replacement, ()) = Self::adopt(self.layout, elements, tail, || Ok(()))?;
        Ok(std::mem::replace(self, replacement).into_vec())
    }

    pub(crate) fn copied(source: impl PackedSource<Layout = L>, tail: usize) -> Self {
        let layout = source.layout();
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
    pub(crate) fn refill_from(&mut self, source: impl PackedSource<Layout = L>, tail: usize) {
        let layout = source.layout();
        if self.elements.capacity() < layout.len().saturating_add(tail) {
            *self = Self::copied(source, tail);
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
    fn set_tail(&mut self, tail: usize) {
        if tail > self.tail {
            seal(&mut self.elements, tail);
        }
        self.tail = tail;
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
    layout: PackedVideoLayout,
    planes: VideoPlanes<'a>,
}
impl<'a> VideoCopy<'a> {
    pub(crate) fn new(source: &'a (impl VideoParts + ?Sized)) -> Self {
        let source_layout = source.layout();
        let layout = source_layout.packed();
        // SAFETY: VideoParts lends initialized active rows for the borrow.
        let planes = unsafe { source_layout.view_planes(source.pixels()) };
        Self { layout, planes }
    }
}
impl sealed::Source for VideoCopy<'_> {}
impl PackedSource for VideoCopy<'_> {
    type Layout = PackedVideoLayout;
    fn layout(&self) -> Self::Layout {
        self.layout
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
    layout: PackedAudioLayout,
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
    type Layout = PackedAudioLayout;
    fn layout(&self) -> Self::Layout {
        self.layout
    }
    fn write(self, elements: &mut Vec<f32>) {
        for channel in self.channels {
            elements.extend_from_slice(channel);
        }
    }
}

/// Packed pixels, their scan type, and the zeroed tail required to lend them
/// to the SDK. Changing the layout or scan type establishes the new room before
/// exposing either value.
#[derive(Clone, Debug)]
pub(crate) struct VideoStorage {
    packed: PackedStorage<PackedVideoLayout>,
    scan: ScanType,
}

impl VideoStorage {
    pub(crate) fn zeroed(layout: PackedVideoLayout, scan: ScanType) -> Self {
        Self {
            packed: PackedStorage::zeroed(layout, send_room(layout.as_layout(), scan)),
            scan,
        }
    }

    pub(crate) fn adopt<T>(
        layout: PackedVideoLayout,
        elements: Vec<u8>,
        scan: ScanType,
        validate: impl FnOnce() -> Result<T>,
    ) -> std::result::Result<(Self, T), Rejected<Vec<u8>>> {
        let (packed, checked) = PackedStorage::adopt(
            layout,
            elements,
            send_room(layout.as_layout(), scan),
            validate,
        )?;
        Ok((Self { packed, scan }, checked))
    }

    pub(crate) fn copied(copy: VideoCopy<'_>, scan: ScanType) -> Self {
        let tail = send_room(copy.layout.as_layout(), scan);
        Self {
            packed: PackedStorage::copied(copy, tail),
            scan,
        }
    }

    pub(crate) fn refill_from(&mut self, copy: VideoCopy<'_>, scan: ScanType) {
        let tail = send_room(copy.layout.as_layout(), scan);
        self.packed.refill_from(copy, tail);
        self.scan = scan;
    }

    pub(crate) fn set_scan_type(&mut self, scan: ScanType) {
        self.packed.set_tail(send_room(self.layout(), scan));
        self.scan = scan;
    }

    pub(crate) fn swap(
        &mut self,
        elements: Vec<u8>,
    ) -> std::result::Result<Vec<u8>, Rejected<Vec<u8>>> {
        self.packed.swap(elements, self.packed.room())
    }

    pub(crate) fn layout(&self) -> &ValidatedVideoLayout {
        self.packed.layout().as_layout()
    }

    pub(crate) const fn scan_type(&self) -> ScanType {
        self.scan
    }

    pub(crate) fn into_vec(self) -> Vec<u8> {
        self.packed.into_vec()
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        self.packed.as_slice()
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [u8] {
        self.packed.as_mut_slice()
    }

    pub(crate) fn as_ptr(&self) -> *const u8 {
        self.packed.as_ptr()
    }

    #[cfg(test)]
    pub(crate) fn room(&self) -> usize {
        self.packed.room()
    }

    #[cfg(any(test, feature = "advanced"))]
    pub(crate) fn capacity(&self) -> usize {
        self.packed.capacity()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.packed.len()
    }

    #[cfg(test)]
    pub(crate) fn room_bytes(&self) -> &[u8] {
        self.packed.room_bytes()
    }

    fn staged(
        copy: impl PackedSource<Layout = PackedVideoLayout>,
        scan: ScanType,
        staging: &mut Option<Self>,
    ) -> &Self {
        let layout = copy.layout();
        let tail = send_room(layout.as_layout(), scan);
        let required = layout.len().saturating_add(tail);
        if staging
            .as_ref()
            .is_some_and(|storage| storage.packed.elements.capacity() < required)
        {
            // A replacement frees old staging before allocating. Failed filling
            // leaves None, so discarded storage has no surviving descriptor.
            drop(staging.take());
        }
        match staging {
            Some(storage) => {
                storage.packed.refill_from(copy, tail);
                storage.scan = scan;
                storage
            }
            None => staging.insert(Self {
                packed: PackedStorage::copied(copy, tail),
                scan,
            }),
        }
    }
}

/// Packed video whose fields are private to the checked copy kernel.
pub(crate) struct PackedVideo<'a> {
    storage: &'a VideoStorage,
    timing: VideoTiming,
    metadata: FrameMetadataRef<'a>,
    timestamp: Option<ReceivedTimestamp>,
}
// SAFETY: private fields bind the pixels to their storage's own layout and
// initialized zero tail. Its timing is copied and its metadata remains borrowed.
unsafe impl VideoParts for PackedVideo<'_> {
    fn layout(&self) -> &ValidatedVideoLayout {
        self.storage.layout()
    }
    fn header(&self) -> VideoHeader {
        self.timing.with_scan_type(self.storage.scan_type())
    }
    fn pixels(&self) -> *const u8 {
        self.storage.as_ptr()
    }
    fn frame_metadata(&self) -> FrameMetadataRef<'_> {
        self.metadata
    }
    fn sdk_timestamp(&self) -> Option<ReceivedTimestamp> {
        self.timestamp
    }
    fn initialized(&self) -> Initialized {
        Initialized::PackedSendReady
    }
}

pub(crate) fn stage_video<'a>(
    source: &'a (impl VideoParts + ?Sized),
    staging: &'a mut Option<VideoStorage>,
) -> PackedVideo<'a> {
    let header = source.header();
    let metadata = source.frame_metadata();
    let timestamp = source.sdk_timestamp();
    let copy = VideoCopy::new(source);
    let storage = VideoStorage::staged(copy, header.scan_type(), staging);
    PackedVideo {
        storage,
        timing: header.timing,
        metadata,
        timestamp,
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
    #[expect(
        clippy::panic,
        reason = "sealed PackedSource implementations write their exact packed layout, which PackedVideo's VideoParts rely on"
    )]
    if restore.data.len() != expected_len {
        panic!("packed copy must initialize its complete layout");
    }
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

    fn video(width: u32) -> PackedVideoLayout {
        PackedVideoLayout::new(crate::PixelFormat::Uyvy, width, 2).unwrap()
    }
    fn audio() -> PackedAudioLayout {
        crate::frames::ValidatedAudioLayout::new_uncompressed(
            crate::AudioFormat::Fltp,
            48_000,
            2,
            4,
            None,
        )
        .unwrap()
        .packed()
    }
    struct Fill<L, F> {
        layout: L,
        write: F,
    }
    impl<L, F> sealed::Source for Fill<L, F> {}
    impl<L: PackedLayout, F: FnOnce(&mut Vec<L::Element>)> PackedSource for Fill<L, F> {
        type Layout = L;
        fn layout(&self) -> L {
            self.layout
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
            storage.refill_from(
                Fill {
                    layout: video(2),
                    write: |bytes: &mut Vec<_>| bytes.resize(video(2).len(), 0),
                },
                usize::MAX,
            );
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
            let adopted = PackedStorage::adopt(layout, zeroed.into_vec(), tail, || Ok(()))
                .unwrap()
                .0;
            assert_eq!(adopted.as_slice().len(), adopted.layout().len());
            let cloned = adopted.clone();
            assert_eq!(cloned.as_slice().len(), cloned.layout().len());
            assert_eq!(cloned.room_bytes().len(), tail);
            assert!(PackedStorage::adopt(layout, Vec::new(), tail, || Ok(())).is_err());
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
            PackedStorage::adopt(layout, elements, 64, || Ok(()))
                .unwrap()
                .0
        });
        assert_eq!(allocations, 0);
        assert_eq!(storage.as_ptr(), pointer);
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            storage.refill_from(
                Fill {
                    layout,
                    write: |bytes: &mut Vec<_>| bytes.resize(layout.len(), 9),
                },
                64,
            );
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
                storage.refill_from(
                    Fill {
                        layout,
                        write: |data: &mut Vec<_>| data.push(L::Element::default()),
                    },
                    tail,
                );
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
            storage.refill_from(
                Fill {
                    layout: larger,
                    write: |bytes: &mut Vec<_>| bytes.push(7),
                },
                64,
            );
        }));
        assert!(failed.is_err());
        assert_eq!(format!("{:?}", storage.layout()), format!("{previous:?}"));
        assert_eq!(storage.as_slice().len(), previous.len());
        assert_eq!(storage.as_ptr(), pointer);
        assert_eq!(storage.room_bytes(), vec![0; 64]);
    }

    #[test]
    fn a_failed_staging_replacement_leaves_no_stale_storage() {
        let mut staging = Some(VideoStorage::zeroed(video(4), ScanType::Progressive));
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = VideoStorage::staged(
                Fill {
                    layout: video(64),
                    write: |bytes: &mut Vec<_>| bytes.push(7),
                },
                ScanType::Progressive,
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
            storage.refill_from(
                Fill {
                    layout: larger,
                    write: |bytes: &mut Vec<_>| {
                        bytes.resize(30, 255);
                        panic!("interrupted copy");
                    },
                },
                32,
            );
        }));
        assert!(failed.is_err());
        assert_eq!(format!("{:?}", storage.layout()), format!("{previous:?}"));
        assert_eq!(storage.as_slice().len(), previous.len());
        assert_eq!(storage.room_bytes(), vec![0; 64]);
    }
}
