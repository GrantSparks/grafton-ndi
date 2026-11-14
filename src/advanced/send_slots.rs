//! Stable native descriptor storage, owned independently of moving entries.

use std::{mem::MaybeUninit, sync::Arc};

use super::aliasable::AliasableBox;
#[cfg(ndi_has_advanced_scatter_send)]
use crate::sys::advanced::FrameScatter;
use crate::sys::bindings::NDIlib_video_frame_v2_t;
#[cfg(any(test, ndi_has_advanced_send_completion, ndi_has_advanced_scatter_send))]
use crate::{Error, Result};

#[cfg(any(test, ndi_has_advanced_send_completion, ndi_has_advanced_scatter_send))]
pub(super) fn unavailable(_: std::collections::TryReserveError) -> Error {
    Error::InvalidInput("unable to allocate bounded completion storage".into())
}

struct Slot {
    #[cfg(debug_assertions)]
    leases: std::sync::atomic::AtomicUsize,
    header: MaybeUninit<NDIlib_video_frame_v2_t>,
    #[cfg(ndi_has_advanced_scatter_send)]
    scatter: MaybeUninit<FrameScatter>,
}

pub(super) struct DescriptorSlots {
    // All native addresses derive from this raw owning pointer. Access never
    // constructs a reference to the whole array while any slot is lent.
    storage: AliasableBox<[Slot]>,
    len: usize,
}

// SAFETY: a fresh exclusive lease is required to initialize a slot. Queue
// admission serializes claims and collection/destruction ends each old loan;
// compressed streams alternate their two slots only after synchronization.
// Shared access reads only allocation metadata, never native descriptor bytes.
unsafe impl Send for DescriptorSlots {}
// SAFETY: as above; concurrent native reads concern disjoint initialized slots,
// and neither moving nor sharing the owner creates references into the array.
unsafe impl Sync for DescriptorSlots {}

impl DescriptorSlots {
    #[cfg(any(test, ndi_has_advanced_send_completion, ndi_has_advanced_scatter_send))]
    pub(super) fn new(len: usize) -> Result<Arc<Self>> {
        let mut storage = Vec::new();
        storage.try_reserve_exact(len).map_err(unavailable)?;
        storage.resize_with(len, || Slot {
            #[cfg(debug_assertions)]
            leases: std::sync::atomic::AtomicUsize::new(0),
            header: MaybeUninit::uninit(),
            #[cfg(ndi_has_advanced_scatter_send)]
            scatter: MaybeUninit::uninit(),
        });
        Ok(Arc::new(Self {
            storage: AliasableBox::new(storage.into_boxed_slice()),
            len,
        }))
    }

    /// # Safety
    /// `index` has no live lease and the SDK has finished every previous loan
    /// of it. No further claim of that index is allowed until this lease and
    /// every scatter lease it creates have been released after completion.
    pub(super) unsafe fn claim(self: &Arc<Self>, index: usize) -> DescriptorLease {
        assert!(index < self.len);
        #[cfg(debug_assertions)]
        assert!(
            self.leases(index)
                .compare_exchange(
                    0,
                    1,
                    std::sync::atomic::Ordering::AcqRel,
                    std::sync::atomic::Ordering::Acquire
                )
                .is_ok(),
            "claimed descriptor slot is not free"
        );
        DescriptorLease {
            owner: Arc::clone(self),
            index,
            header_written: false,
            #[cfg(ndi_has_advanced_scatter_send)]
            scatter_written: false,
        }
    }

    #[cfg(debug_assertions)]
    fn leases(&self, index: usize) -> &std::sync::atomic::AtomicUsize {
        let slot = self.slot(index);
        // SAFETY: this is the disjoint Rust-only atomic, never native descriptor
        // bytes. No reference to the array or any header/list is constructed.
        unsafe { &*std::ptr::addr_of!((*slot).leases) }
    }

    fn slot(&self, index: usize) -> *mut Slot {
        // SAFETY: only claim constructs indices, after checking the bound. No
        // reference into this array is formed, including to other live slots.
        unsafe { self.storage.as_ptr().cast::<Slot>().cast_mut().add(index) }
    }
}

/// Exclusive initialization rights and strong ownership of one stable slot.
/// Returning the index to a free list is separate, and requires completion or
/// destruction; dropping a lease never locks queue state or runs user code.
pub(super) struct DescriptorLease {
    owner: Arc<DescriptorSlots>,
    index: usize,
    header_written: bool,
    #[cfg(ndi_has_advanced_scatter_send)]
    scatter_written: bool,
}

impl DescriptorLease {
    pub(super) fn index(&self) -> usize {
        self.index
    }

    pub(super) fn header(
        &mut self,
        descriptor: NDIlib_video_frame_v2_t,
    ) -> *const NDIlib_video_frame_v2_t {
        assert!(
            !self.header_written,
            "a descriptor lease is initialized once"
        );
        let slot = self.owner.slot(self.index);
        // SAFETY: claim grants this lease exclusive initialization of a slot
        // with no native loan, and the flag prevents rewriting its live header.
        let pointer =
            unsafe { std::ptr::addr_of_mut!((*slot).header).cast::<NDIlib_video_frame_v2_t>() };
        // SAFETY: this aligned slot has space for the complete descriptor.
        unsafe { pointer.write(descriptor) };
        self.header_written = true;
        pointer
    }

    #[cfg(ndi_has_advanced_scatter_send)]
    pub(super) fn scatter(&mut self, descriptor: FrameScatter) -> ScatterLease {
        assert!(!self.scatter_written, "a scatter lease is initialized once");
        let slot = self.owner.slot(self.index);
        // SAFETY: the exclusive fresh lease owns initialization rights, and
        // no reference to this or any other slot is created by this write.
        unsafe {
            std::ptr::addr_of_mut!((*slot).scatter)
                .cast::<FrameScatter>()
                .write(descriptor)
        };
        self.scatter_written = true;
        #[cfg(debug_assertions)]
        self.owner
            .leases(self.index)
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        ScatterLease {
            owner: Arc::clone(&self.owner),
            index: self.index,
        }
    }
}

/// Read access to a stable initialized list, with independent ownership of the
/// backing allocation. PendingVideo retains it until its native loan ends.
#[cfg(ndi_has_advanced_scatter_send)]
pub(super) struct ScatterLease {
    owner: Arc<DescriptorSlots>,
    index: usize,
}

#[cfg(ndi_has_advanced_scatter_send)]
impl ScatterLease {
    pub(super) fn as_ptr(&self) -> *const FrameScatter {
        let slot = self.owner.slot(self.index);
        // SAFETY: scatter constructs this lease only after initializing the
        // slot. Its Arc keeps that allocation alive without borrowing bytes.
        unsafe { std::ptr::addr_of!((*slot).scatter).cast::<FrameScatter>() }
    }
}

#[cfg(debug_assertions)]
impl Drop for DescriptorLease {
    fn drop(&mut self) {
        let previous = self
            .owner
            .leases(self.index)
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        debug_assert!(previous > 0);
    }
}

#[cfg(all(debug_assertions, ndi_has_advanced_scatter_send))]
impl Drop for ScatterLease {
    fn drop(&mut self) {
        let previous = self
            .owner
            .leases(self.index)
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        debug_assert!(previous > 0);
    }
}
