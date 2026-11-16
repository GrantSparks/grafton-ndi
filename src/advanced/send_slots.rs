//! Stable native descriptor storage with move-only initialization rights.

use std::{cell::UnsafeCell, mem::MaybeUninit, sync::Arc};

use super::aliasable::AliasableBox;
use crate::{sender::in_flight::Destroyed, sys::bindings::NDIlib_video_frame_v2_t};
#[cfg(any(test, ndi_has_advanced_send_completion, ndi_has_advanced_scatter_send))]
use crate::{Error, Result};

#[cfg(any(test, ndi_has_advanced_send_completion, ndi_has_advanced_scatter_send))]
pub(super) fn unavailable(_: std::collections::TryReserveError) -> Error {
    crate::error::allocation_failed(crate::AllocationKind::QueuedCompletions, None)
}

struct Slot {
    header: UnsafeCell<MaybeUninit<NDIlib_video_frame_v2_t>>,
}

pub(super) struct DescriptorSlots {
    // Derive every native address from this owner without borrowing the array
    // while any disjoint slot is lent to the SDK.
    storage: AliasableBox<[Slot]>,
}

// SAFETY: each slot has exactly one move-only key. Only consuming that key
// creates a lease that can initialize the slot, and a native-release proof is
// required to recover its key. Shared access never reads descriptor bytes.
unsafe impl Send for DescriptorSlots {}
// SAFETY: native loans use disjoint slots, and each slot is initialized only
// through its UnsafeCell by its sole key. Shared access cannot read descriptor
// bytes or duplicate initialization rights.
unsafe impl Sync for DescriptorSlots {}

/// Exclusive initialization rights to one slot of one storage allocation.
/// Deliberately neither Clone nor Copy; dropping it permanently retires a slot.
pub(super) struct SlotKey {
    owner: Arc<DescriptorSlots>,
    index: usize,
}

impl SlotKey {
    /// Consume this key and claim the storage allocation it keeps alive.
    pub(super) fn claim(self, descriptor: NDIlib_video_frame_v2_t) -> DescriptorLease {
        let slot = self.owner.slot(self.index);
        // SAFETY: this sole move-only key grants exclusive initialization of
        // its aligned slot. The resulting lease permits no further writes.
        unsafe {
            UnsafeCell::raw_get(std::ptr::addr_of!((*slot).header))
                .cast::<NDIlib_video_frame_v2_t>()
                .write(descriptor)
        };
        DescriptorLease { key: self }
    }
}

/// Evidence that native access has ended, supplied by the operation that
/// releases an entry. A completion stays with the entry the callback released.
pub(super) enum Released<'a> {
    Completed(super::queued::Completion),
    Destroyed(&'a Destroyed),
}

impl Released<'_> {
    /// Consume the authority supplied by the owning synchronization operation.
    fn consume(self) {
        match self {
            Self::Completed(_completion) => {}
            Self::Destroyed(_destroyed) => {}
        }
    }
}

impl DescriptorSlots {
    #[cfg(any(test, ndi_has_advanced_send_completion, ndi_has_advanced_scatter_send))]
    pub(super) fn allocate_keys(len: usize) -> Result<Vec<SlotKey>> {
        let mut storage = Vec::new();
        storage.try_reserve_exact(len).map_err(unavailable)?;
        storage.resize_with(len, || Slot {
            header: UnsafeCell::new(MaybeUninit::uninit()),
        });
        let owner = Arc::new(Self {
            storage: AliasableBox::new(storage.into_boxed_slice()),
        });
        let mut keys = Vec::new();
        keys.try_reserve_exact(len).map_err(unavailable)?;
        keys.extend((0..len).rev().map(|index| SlotKey {
            owner: Arc::clone(&owner),
            index,
        }));
        Ok(keys)
    }

    fn slot(&self, index: usize) -> *mut Slot {
        // SAFETY: only allocate_keys creates indices, within the allocated array, and
        // no reference to this slot or other live slots is formed.
        unsafe { self.storage.as_ptr().cast::<Slot>().cast_mut().add(index) }
    }
}

/// One slot's exclusive initialization rights until native release is proven.
/// A dropped lease loses its key; unwinding cannot make a live slot reusable.
pub(super) struct DescriptorLease {
    key: SlotKey,
}

impl DescriptorLease {
    pub(super) fn release(self, proof: Released<'_>) -> SlotKey {
        proof.consume();
        self.key
    }

    pub(super) fn as_ptr(&self) -> *const NDIlib_video_frame_v2_t {
        let slot = self.key.owner.slot(self.key.index);
        // SAFETY: claim initialized this lease's stable slot. Forming its
        // address borrows no descriptor bytes while native code reads them.
        unsafe {
            UnsafeCell::raw_get(std::ptr::addr_of!((*slot).header))
                .cast::<NDIlib_video_frame_v2_t>()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A safe shared view of the whole allocation may coexist with writes to
    /// another key's slot: only the UnsafeCell contents are initialized.
    #[test]
    fn shared_slot_storage_allows_another_key_to_initialize_its_cell() {
        #[inline(never)]
        fn claim_while_shared(slots: &[Slot], key: SlotKey) {
            let descriptor = NDIlib_video_frame_v2_t {
                xres: 1920,
                yres: 1080,
                ..Default::default()
            };
            let lease = key.claim(descriptor);
            // SAFETY: this test owns the lease, and has not lent it to native
            // code. The descriptor was initialized by claim and is live.
            assert_eq!(unsafe { (*lease.as_ptr()).xres }, 1920);
            assert_eq!(slots.len(), 2);
        }

        let mut keys = DescriptorSlots::allocate_keys(2).unwrap();
        let key = keys.pop().unwrap();
        let owner = Arc::clone(&keys[0].owner);
        claim_while_shared(&owner.storage, key);
    }

    #[test]
    fn impossible_descriptor_reservation_reports_storage_exhaustion() {
        assert!(matches!(
            DescriptorSlots::allocate_keys(usize::MAX),
            Err(Error::AllocationFailed(crate::AllocationFailure {
                kind: crate::AllocationKind::QueuedCompletions,
                bytes: None,
            }))
        ));
    }

    #[test]
    fn every_key_initializes_a_disjoint_reserved_header_without_allocating() {
        let keys = DescriptorSlots::allocate_keys(4).unwrap();
        let (pointers, count) = crate::test_alloc::allocation_count(|| {
            let mut pointers = [std::ptr::null(); 4];
            for (index, key) in keys.into_iter().enumerate() {
                let lease = key.claim(NDIlib_video_frame_v2_t::default());
                pointers[index] = lease.as_ptr();
            }
            pointers
        });
        assert_eq!(count, 0);
        for (index, pointer) in pointers.iter().enumerate() {
            assert!(!pointer.is_null());
            assert!(!pointers[..index].contains(pointer));
        }
    }
}

#[cfg(any(
    ndi_test_send_ownership,
    ndi_test_send_ownership_control,
    ndi_test_send_ownership_types
))]
#[path = "../../tests/ui/internal/send_slot_ownership.rs"]
mod ownership_contract;
