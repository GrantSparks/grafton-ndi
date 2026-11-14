//! Pixel storage the SDK's sender can read to the end of.
//!
//! The x86 builds of the SDK read past the last pixel of many UYVY and UYVA
//! frames they send;
//! [`PixelFormat::send_tail_room`](crate::PixelFormat::send_tail_room) has
//! the measurements. Every buffer of pixels the crate lends the SDK for
//! sending is therefore followed by the readable bytes [`send_room`] counts.
//! [`Pixels`] is the storage that keeps them: an owned frame's pixels, and
//! the copy a sender makes of a borrowed frame whose buffer has no room.

use std::mem::MaybeUninit;

use crate::{frames::ValidatedVideoLayout, ScanType};

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

/// A frame's pixels, followed in the same allocation by `tail` zero bytes.
///
/// The bytes after the pixels are zero, not merely readable, because the SDK
/// compresses what it reads there into the picture's last blocks: zeros keep
/// a frame's encoding a function of the frame alone.
///
/// Every method that changes the length or the allocation restores the room,
/// and nothing else can reach the `Vec`, so the room holds whenever the SDK
/// is given the pointer.
#[derive(Debug, Default)]
pub(crate) struct Pixels {
    /// The pixels. The spare capacity begins with `tail` zero bytes.
    bytes: Vec<u8>,
    tail: usize,
}

impl Pixels {
    /// `len` zero bytes with `tail` bytes of room.
    pub(crate) fn zeroed(len: usize, tail: usize) -> Self {
        // One zeroed allocation holds both: the room is cut from its end.
        let mut bytes = vec![0; len + tail];
        bytes.truncate(len);
        Self::adopt(bytes, tail)
    }

    /// `bytes` with `tail` bytes of room: kept as it is when its spare
    /// capacity holds the room, and otherwise grown once, which may move it.
    pub(crate) fn adopt(mut bytes: Vec<u8>, tail: usize) -> Self {
        seal(&mut bytes, tail);
        Self { bytes, tail }
    }

    /// Empty storage that [`Self::fill`] can fill with `len` bytes and
    /// `tail` bytes of room without allocating.
    pub(crate) fn with_room(len: usize, tail: usize) -> Self {
        Self::adopt(Vec::with_capacity(len + tail), tail)
    }

    /// Whether [`Self::fill`] can write `len` bytes with `tail` bytes of room
    /// into this allocation.
    pub(crate) fn holds(&self, len: usize, tail: usize) -> bool {
        self.bytes.capacity() >= len + tail
    }

    /// Replace the contents with the `len` bytes `write` appends to the
    /// emptied buffer, followed by `tail` bytes of room.
    ///
    /// The allocation is kept when it [holds](Self::holds) them, in which
    /// case nothing here allocates, and is otherwise replaced by one of
    /// exactly that size.
    pub(crate) fn fill(&mut self, len: usize, tail: usize, write: impl FnOnce(&mut Vec<u8>)) {
        if !self.holds(len, tail) {
            // Free the old allocation before making the new one.
            self.bytes = Vec::new();
            self.bytes.reserve_exact(len + tail);
        }
        self.bytes.clear();
        write(&mut self.bytes);
        debug_assert_eq!(self.bytes.len(), len);
        seal(&mut self.bytes, tail);
        self.tail = tail;
    }

    /// Recover pixels without shrinking their allocation or exposing tail bytes.
    pub(crate) fn into_vec(self) -> Vec<u8> {
        self.bytes
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    /// Give the pixels at least `tail` bytes of room, growing the allocation
    /// once when it holds less, which may move them.
    pub(crate) fn reserve_room(&mut self, tail: usize) {
        if tail > self.tail {
            seal(&mut self.bytes, tail);
            self.tail = tail;
        }
    }

    /// The first pixel. `len` bytes and then `tail` zero bytes
    /// are readable from it, until the storage is next changed or dropped.
    pub(crate) fn as_ptr(&self) -> *const u8 {
        self.bytes.as_ptr()
    }

    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }

    /// The size of the allocation, room included.
    #[cfg(any(test, feature = "advanced"))]
    pub(crate) fn capacity(&self) -> usize {
        self.bytes.capacity()
    }

    /// The number of zero bytes that follow the pixels.
    #[cfg(test)]
    pub(crate) fn tail_room(&self) -> usize {
        self.tail
    }

    /// The bytes after the pixels, read from the allocation as the SDK reads
    /// them.
    #[cfg(test)]
    pub(crate) fn room(&self) -> &[u8] {
        assert!(self.capacity() >= self.len() + self.tail);
        // SAFETY: `tail` bytes after the pixels are inside the allocation
        // and initialized, by `seal`, and nothing writes them while `self`
        // is borrowed.
        unsafe { std::slice::from_raw_parts(self.as_ptr().add(self.len()), self.tail) }
    }
}

impl Clone for Pixels {
    /// A copy in one allocation, with the same room. A derived clone would
    /// copy the `Vec` into an allocation without it.
    fn clone(&self) -> Self {
        let mut copy = Self::with_room(self.len(), self.tail);
        copy.fill(self.len(), self.tail, |bytes| {
            bytes.extend_from_slice(&self.bytes);
        });
        copy
    }
}

/// Give `bytes` `tail` zero bytes of spare capacity after its last element,
/// growing the allocation only when it has less.
fn seal(bytes: &mut Vec<u8>, tail: usize) {
    bytes.reserve_exact(tail);
    bytes.spare_capacity_mut()[..tail].fill(MaybeUninit::new(0));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zeroed_storage_is_one_allocation_with_its_room() {
        let (pixels, allocations) = crate::test_alloc::allocation_count(|| Pixels::zeroed(100, 64));
        assert_eq!(allocations, 1);
        assert_eq!(pixels.as_slice(), [0; 100]);
        assert_eq!((pixels.capacity(), pixels.tail_room()), (164, 64));
        assert_eq!(pixels.room(), [0; 64]);

        let exact = Pixels::zeroed(100, 0);
        assert_eq!(
            (exact.len(), exact.capacity(), exact.tail_room()),
            (100, 100, 0)
        );
    }

    #[test]
    fn an_adopted_vector_is_kept_when_it_has_the_room() {
        let mut roomy = Vec::with_capacity(200);
        roomy.extend_from_slice(&[7; 100]);
        // Stale bytes where the room will be: a longer frame once lived here.
        roomy.extend_from_slice(&[9; 100]);
        roomy.truncate(100);
        let address = roomy.as_ptr();
        let (pixels, allocations) =
            crate::test_alloc::allocation_count(|| Pixels::adopt(roomy, 64));
        assert_eq!(allocations, 0);
        assert_eq!(pixels.as_ptr(), address);
        assert_eq!(pixels.as_slice(), [7; 100]);
        assert_eq!(pixels.room(), [0; 64]);

        // Without a need for room an exact vector is untouched.
        let exact = vec![7; 100];
        let address = exact.as_ptr();
        let (pixels, allocations) = crate::test_alloc::allocation_count(|| Pixels::adopt(exact, 0));
        assert_eq!(allocations, 0);
        assert_eq!((pixels.as_ptr(), pixels.capacity()), (address, 100));
    }

    #[test]
    fn an_adopted_vector_without_room_grows_once() {
        let exact = vec![7; 100];
        let pixels = Pixels::adopt(exact, 64);
        assert_eq!(pixels.as_slice(), [7; 100]);
        assert_eq!((pixels.capacity(), pixels.tail_room()), (164, 64));
        assert_eq!(pixels.room(), [0; 64]);
    }

    #[test]
    fn filling_reuses_an_allocation_that_holds_the_frame_and_its_room() {
        let mut pixels = Pixels::with_room(100, 64);
        assert!(pixels.holds(100, 64) && !pixels.holds(101, 64));
        let address = pixels.as_ptr();
        for (len, tail, value) in [(100, 64, 1), (40, 64, 2), (164, 0, 3), (100, 64, 4)] {
            let ((), allocations) = crate::test_alloc::allocation_count(|| {
                pixels.fill(len, tail, |bytes| bytes.resize(len, value));
            });
            assert_eq!(allocations, 0);
            assert_eq!(pixels.as_ptr(), address);
            assert_eq!(pixels.as_slice(), vec![value; len]);
            // The room is zero even where an earlier, longer fill wrote.
            assert_eq!(pixels.room(), vec![0; tail]);
        }
    }

    #[test]
    fn filling_replaces_an_allocation_that_is_too_small_with_an_exact_one() {
        let mut pixels = Pixels::with_room(100, 64);
        let ((), allocations) = crate::test_alloc::allocation_count(|| {
            pixels.fill(101, 64, |bytes| bytes.resize(101, 5));
        });
        assert_eq!(allocations, 1);
        assert_eq!((pixels.len(), pixels.capacity()), (101, 165));
        assert_eq!(pixels.room(), [0; 64]);

        // Storage that never allocated is filled the same way.
        let mut empty = Pixels::default();
        assert_eq!((empty.capacity(), empty.tail_room()), (0, 0));
        empty.fill(8, 64, |bytes| bytes.extend_from_slice(&[6; 8]));
        assert_eq!((empty.as_slice(), empty.capacity()), (&[6; 8][..], 72));
        assert_eq!(empty.room(), [0; 64]);
    }

    #[test]
    fn a_clone_keeps_the_room_in_one_allocation() {
        let mut pixels = Pixels::zeroed(100, 64);
        pixels.as_mut_slice().fill(3);
        let (copy, allocations) = crate::test_alloc::allocation_count(|| pixels.clone());
        assert_eq!(allocations, 1);
        assert_ne!(copy.as_ptr(), pixels.as_ptr());
        assert_eq!(copy.as_slice(), [3; 100]);
        assert_eq!((copy.capacity(), copy.tail_room()), (164, 64));
        assert_eq!(copy.room(), [0; 64]);
    }
}
