//! Packet storage and its checked extent, inseparable outside this module.
use super::{invalid_frame, PacketLen};
use crate::{advanced::aliasable::AliasableVec, Result};

/// Immutable packet bytes whose checked length belongs to this exact slice.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PacketSlice<'a> {
    bytes: &'a [u8],
    len: PacketLen,
}

impl<'a> PacketSlice<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Option<Self> {
        Some(Self {
            bytes,
            len: PacketLen::new(bytes.len())?,
        })
    }

    /// # Safety
    /// A non-null `data` is readable for `size` bytes for the returned borrow.
    pub(crate) unsafe fn from_sdk(data: *const u8, size: i32) -> Result<Self> {
        let len = PacketLen::from_sdk(size)
            .filter(|_| !data.is_null())
            .ok_or_else(|| invalid_frame(crate::FrameDefect::CompressedBuffer))?;
        // SAFETY: the positive, in-range extent and non-null pointer were
        // checked; the caller guarantees initialized storage for this borrow.
        let bytes = unsafe { std::slice::from_raw_parts(data, len.to_usize()) };
        Ok(Self { bytes, len })
    }

    /// # Safety
    /// `data` is non-null and readable for this checked extent for `'a`.
    pub(crate) unsafe fn from_validated(data: *const u8, len: PacketLen) -> Self {
        // SAFETY: the owner that validated this frame retains its initialized
        // packet allocation, including its extent, through the returned borrow.
        Self {
            bytes: unsafe { std::slice::from_raw_parts(data, len.to_usize()) },
            len,
        }
    }

    pub(crate) fn as_slice(self) -> &'a [u8] {
        self.bytes
    }
    pub(crate) fn len(self) -> PacketLen {
        self.len
    }
}

/// Every constructor and refill keeps `bytes.len() == len.to_usize()`.
#[derive(Clone, Debug)]
pub(crate) struct PacketBytes {
    bytes: AliasableVec<u8>,
    len: PacketLen,
}

impl PacketBytes {
    pub(crate) fn new(bytes: Vec<u8>) -> std::result::Result<Self, Vec<u8>> {
        match PacketSlice::new(&bytes).map(PacketSlice::len) {
            Some(len) => Ok(Self {
                bytes: AliasableVec::new(bytes),
                len,
            }),
            None => Err(bytes),
        }
    }

    pub(crate) fn copied(source: PacketSlice<'_>) -> Self {
        Self {
            bytes: AliasableVec::new(source.bytes.to_vec()),
            len: source.len,
        }
    }

    pub(crate) fn refill(&mut self, source: PacketSlice<'_>) {
        if self.bytes.capacity() < source.bytes.len() {
            // Complete the allocation before replacing the paired destination.
            *self = Self::copied(source);
            return;
        }
        // Capacity already covers the copy. No operation between taking and
        // reinstalling the allocation can allocate or invoke application code.
        let mut bytes =
            std::mem::replace(&mut self.bytes, AliasableVec::new(Vec::new())).into_vec();
        bytes.clear();
        bytes.extend_from_slice(source.bytes);
        self.bytes = AliasableVec::new(bytes);
        self.len = source.len;
    }

    pub(crate) fn with_len(len: PacketLen, fill: impl FnOnce(&mut Vec<u8>)) -> Self {
        let mut bytes = Vec::with_capacity(len.to_usize());
        fill(&mut bytes);
        // The fixed checked extent remains true even if an internal join
        // accidentally supplies too few or too many initialized bytes.
        bytes.resize(len.to_usize(), 0);
        Self {
            bytes: AliasableVec::new(bytes),
            len,
        }
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn view(&self) -> PacketSlice<'_> {
        PacketSlice {
            bytes: &self.bytes,
            len: self.len,
        }
    }
    pub(crate) fn capacity(&self) -> usize {
        self.bytes.capacity()
    }
    pub(crate) fn into_vec(self) -> Vec<u8> {
        self.bytes.into_vec()
    }
    pub(crate) fn lend(&mut self) -> (*mut u8, i32) {
        (self.bytes.as_mut_ptr(), self.len.to_sdk())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_alloc::allocation_count;

    fn paired(packet: &mut PacketBytes) {
        assert_eq!(packet.lend().1 as usize, packet.as_slice().len());
        assert_eq!(packet.lend().0.cast_const(), packet.as_slice().as_ptr());
    }

    #[test]
    fn every_constructor_and_refill_pairs_storage_with_its_extent() {
        assert!(PacketBytes::new(Vec::new()).is_err());
        let mut packet = PacketBytes::new(vec![3; 8]).unwrap();
        paired(&mut packet);
        let mut copied = PacketBytes::copied(packet.view());
        paired(&mut copied);
        let mut cloned = packet.clone();
        paired(&mut cloned);
        let len = PacketLen::new(8).unwrap();
        for written in [4, 8, 16] {
            let mut joined = PacketBytes::with_len(len, |bytes| bytes.resize(written, 4));
            paired(&mut joined);
            assert_eq!(joined.as_slice().len(), 8);
        }
        let pointer = packet.as_slice().as_ptr();
        let (_, allocations) =
            allocation_count(|| packet.refill(PacketSlice::new(&[5; 4]).unwrap()));
        assert_eq!(allocations, 0);
        assert_eq!(packet.as_slice().as_ptr(), pointer);
        paired(&mut packet);
        packet.refill(PacketSlice::new(&[6; 32]).unwrap());
        paired(&mut packet);
    }
}
