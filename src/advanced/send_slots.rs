//! Stable native descriptor storage with move-only initialization rights.

use std::{mem::MaybeUninit, sync::Arc};

use super::aliasable::AliasableBox;
use crate::sender::in_flight::Synchronized;
#[cfg(ndi_has_advanced_scatter_send)]
use crate::sys::advanced::FrameScatter;
use crate::{sender::in_flight::Destroyed, sys::bindings::NDIlib_video_frame_v2_t};
#[cfg(any(test, ndi_has_advanced_send_completion, ndi_has_advanced_scatter_send))]
use crate::{Error, Result};

#[cfg(any(test, ndi_has_advanced_send_completion, ndi_has_advanced_scatter_send))]
pub(super) fn unavailable(_: std::collections::TryReserveError) -> Error {
    Error::InvalidInput("unable to allocate bounded completion storage".into())
}

struct Slot {
    header: MaybeUninit<NDIlib_video_frame_v2_t>,
    #[cfg(ndi_has_advanced_scatter_send)]
    scatter: MaybeUninit<FrameScatter>,
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
// SAFETY: native loans use disjoint slots; moving/sharing the owner does not
// create references into descriptor storage or duplicate initialization rights.
unsafe impl Sync for DescriptorSlots {}

/// Exclusive initialization rights to one slot of one storage allocation.
/// Deliberately neither Clone nor Copy; dropping it permanently retires a slot.
pub(super) struct SlotKey {
    owner: Arc<DescriptorSlots>,
    index: usize,
}

impl SlotKey {
    /// Consume this key and claim the storage allocation it keeps alive.
    pub(super) fn claim(self) -> DescriptorLease {
        DescriptorLease {
            key: self,
            header_written: false,
            #[cfg(ndi_has_advanced_scatter_send)]
            scatter_written: false,
        }
    }
}

/// Evidence that native access has ended, supplied by the operation that
/// releases an entry. A completion stays with the entry the callback released.
pub(super) enum Released<'a> {
    Completed(super::queued::Completion),
    Synchronized(&'a Synchronized),
    Destroyed(&'a Destroyed),
}

impl Released<'_> {
    /// Consume the authority supplied by the owning synchronization operation.
    fn consume(self) {
        match self {
            Self::Completed(_completion) => {}
            Self::Synchronized(_synchronized) => {}
            Self::Destroyed(_destroyed) => {}
        }
    }
}

impl DescriptorSlots {
    #[cfg(any(test, ndi_has_advanced_send_completion, ndi_has_advanced_scatter_send))]
    pub(super) fn new(len: usize) -> Result<Vec<SlotKey>> {
        let mut storage = Vec::new();
        storage.try_reserve_exact(len).map_err(unavailable)?;
        storage.resize_with(len, || Slot {
            header: MaybeUninit::uninit(),
            #[cfg(ndi_has_advanced_scatter_send)]
            scatter: MaybeUninit::uninit(),
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
        // SAFETY: only new creates indices, within the allocated array, and
        // no reference to this slot or other live slots is formed.
        unsafe { self.storage.as_ptr().cast::<Slot>().cast_mut().add(index) }
    }
}

/// One slot's exclusive initialization rights until native release is proven.
/// A dropped lease loses its key; unwinding cannot make a live slot reusable.
pub(super) struct DescriptorLease {
    key: SlotKey,
    header_written: bool,
    #[cfg(ndi_has_advanced_scatter_send)]
    scatter_written: bool,
}

impl DescriptorLease {
    pub(super) fn release(self, proof: Released<'_>) -> SlotKey {
        proof.consume();
        self.key
    }

    pub(super) fn header(
        &mut self,
        descriptor: NDIlib_video_frame_v2_t,
    ) -> *const NDIlib_video_frame_v2_t {
        assert!(
            !self.header_written,
            "a descriptor lease is initialized once"
        );
        let slot = self.key.owner.slot(self.key.index);
        // SAFETY: this sole key grants exclusive initialization of an aligned
        // slot; the flag prevents rewriting a header already lent to the SDK.
        let pointer =
            unsafe { std::ptr::addr_of_mut!((*slot).header).cast::<NDIlib_video_frame_v2_t>() };
        // SAFETY: the reserved slot has room for the complete descriptor.
        unsafe { pointer.write(descriptor) };
        self.header_written = true;
        pointer
    }

    #[cfg(ndi_has_advanced_scatter_send)]
    pub(super) fn scatter(&mut self, descriptor: FrameScatter) -> ScatterLease {
        assert!(!self.scatter_written, "a scatter lease is initialized once");
        let slot = self.key.owner.slot(self.key.index);
        // SAFETY: this fresh lease exclusively initializes the scatter list.
        unsafe {
            std::ptr::addr_of_mut!((*slot).scatter)
                .cast::<FrameScatter>()
                .write(descriptor)
        };
        self.scatter_written = true;
        ScatterLease {
            owner: Arc::clone(&self.key.owner),
            index: self.key.index,
            key: None,
        }
    }

    /// Compressed streams retain only their scatter list. Transfer the key to
    /// that owner so it cannot be recovered until the stream is synchronized.
    #[cfg(ndi_has_advanced_scatter_send)]
    pub(super) fn into_scatter(mut self, descriptor: FrameScatter) -> ScatterLease {
        let mut scatter = self.scatter(descriptor);
        scatter.key = Some(self.key);
        scatter
    }
}

/// A stable initialized scatter list. Queue entries retain its descriptor
/// lease; compressed stream lists instead own that lease's move-only key.
#[cfg(ndi_has_advanced_scatter_send)]
pub(super) struct ScatterLease {
    owner: Arc<DescriptorSlots>,
    index: usize,
    key: Option<SlotKey>,
}

#[cfg(ndi_has_advanced_scatter_send)]
impl ScatterLease {
    pub(super) fn as_ptr(&self) -> *const FrameScatter {
        let slot = self.owner.slot(self.index);
        // SAFETY: construction initialized the list, and the owner keeps its
        // allocation live through the native loan.
        unsafe { std::ptr::addr_of!((*slot).scatter).cast::<FrameScatter>() }
    }

    pub(super) fn owns_key(&self) -> bool {
        self.key.is_some()
    }

    pub(super) fn release(self, proof: Released<'_>) -> Option<SlotKey> {
        proof.consume();
        self.key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile the actual storage module and release-proof declarations.
    /// The accepted fixture rules out unrelated imports or privacy failures;
    /// rejected fixtures pin move-only keys and owner-private proof constructors.
    #[test]
    fn slot_keys_cannot_be_reused_or_cloned() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let proof_declaration = |name: &str| {
            let source = include_str!("../sender.rs");
            let marker = format!("pub(crate) struct {name} {{");
            let start = source.find(&marker).unwrap();
            let declaration = &source[start..];
            &declaration[..declaration.find("\n    }").unwrap() + 6]
        };
        let prelude = format!(
            r#"
#![allow(dead_code, unused_imports, non_camel_case_types, non_snake_case, non_upper_case_globals)]
use std::sync::Arc;
#[path = {bindings:?}]
pub(crate) mod bindings;
mod sys {{ pub(crate) use crate::bindings; }}
mod sender {{ pub mod in_flight {{
    {synchronized}
    {destroyed}
}} }}
mod queued {{
    {completion}
}}
#[path = {aliasable:?}]
mod aliasable;
#[path = {slots:?}]
mod send_slots;
"#,
            bindings = root.join("src/sys/bindings.rs"),
            aliasable = root.join("src/advanced/aliasable.rs"),
            slots = root.join("src/advanced/send_slots.rs"),
            synchronized = proof_declaration("Synchronized"),
            destroyed = proof_declaration("Destroyed"),
            completion = {
                let source = include_str!("queued.rs");
                let start = source.find("pub(super) struct Completion {").unwrap();
                let declaration = &source[start..];
                &declaration[..declaration.find("\n}").unwrap() + 2]
            }
        );
        let moved = include_str!(
            "../../tests/ui/internal/fail/descriptor_slot_key_is_consumed_by_claim.rs"
        );
        let cloned =
            include_str!("../../tests/ui/internal/fail/descriptor_slot_key_cannot_be_cloned.rs");
        let directory =
            std::env::temp_dir().join(format!("grafton-slot-key-contract-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        for (name, fixture, expected) in [
            (
                "once",
                moved.replace("    let _second = key.claim();\n", ""),
                None,
            ),
            ("moved", moved.to_owned(), Some("E0382")),
            ("cloned", cloned.to_owned(), Some("E0599")),
            ("synchronized_private", include_str!("../../tests/ui/internal/fail/synchronization_requires_its_owner.rs").to_owned(), Some("E0451")),
            ("destroyed_private", include_str!("../../tests/ui/internal/fail/destruction_requires_its_owner.rs").to_owned(), Some("E0451")),
            ("completion_private", include_str!("../../tests/ui/internal/fail/completion_requires_its_queue.rs").to_owned(), Some("E0451")),
            ("foreign_storage", include_str!("../../tests/ui/internal/fail/descriptor_slot_key_cannot_claim_other_storage.rs").to_owned(), Some("E0599")),
        ] {
            let path = directory.join(format!("{name}.rs"));
            std::fs::write(&path, format!("{prelude}\n{fixture}")).unwrap();
            let output = std::process::Command::new("rustc")
                .args(["--edition=2021", "--emit=metadata", "--out-dir"])
                .arg(&directory)
                .arg(&path)
                .output()
                .unwrap();
            let error = String::from_utf8(output.stderr).unwrap();
            if let Some(expected) = expected {
                assert!(
                    !output.status.success() && error.contains(expected),
                    "{name}: {error}"
                );
            } else {
                assert!(output.status.success(), "accepted fixture: {error}");
            }
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn every_key_initializes_a_disjoint_reserved_header_without_allocating() {
        let keys = DescriptorSlots::new(4).unwrap();
        let (pointers, count) = crate::test_alloc::allocation_count(|| {
            let mut pointers = [std::ptr::null(); 4];
            for (index, key) in keys.into_iter().enumerate() {
                let mut lease = key.claim();
                pointers[index] = lease.header(NDIlib_video_frame_v2_t::default());
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
