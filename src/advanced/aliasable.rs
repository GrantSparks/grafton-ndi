//! Heap storage whose address the SDK holds.

use std::{fmt, mem::ManuallyDrop, ops::Deref, ptr::NonNull};

/// A heap allocation owned through the raw pointer it was created with.
///
/// The SDK keeps the address of an asynchronously sent frame after the call
/// returns, and it writes into the buffers it sends. A [`Box`] asserts that it
/// is the only path to its allocation, an assertion that a pointer held by the
/// SDK contradicts once the box is moved or borrowed again. This type makes no
/// such assertion: its reads, the pointers it hands out and the final release
/// all derive from the one pointer the allocation was created with, so moving
/// the owner changes nothing about what the SDK's copy of that pointer may do.
///
/// A shared reference only reads. Deriving a writable pointer requires an
/// exclusive borrow, so whoever lets the SDK write holds the allocation
/// exclusively, or by value, for as long as the SDK keeps the pointer; no
/// reference from [`Deref`] can then be alive to observe the write.
pub(crate) struct AliasableBox<T: ?Sized> {
    pointer: NonNull<T>,
}

impl<T: ?Sized> AliasableBox<T> {
    /// Take over a boxed allocation.
    pub(crate) fn new(value: Box<T>) -> Self {
        Self {
            pointer: NonNull::from(Box::leak(value)),
        }
    }

    /// The allocation's address, for the SDK to read.
    pub(crate) fn as_ptr(&self) -> *const T {
        self.pointer.as_ptr()
    }

    /// The allocation's address, for the SDK to read and write.
    #[cfg(test)]
    pub(crate) fn as_mut_ptr(&mut self) -> *mut T {
        self.pointer.as_ptr()
    }

    /// Give the allocation back to an ordinary box.
    #[cfg(test)]
    pub(crate) fn into_box(self) -> Box<T> {
        let this = ManuallyDrop::new(self);
        // SAFETY: the pointer came from a box in `new`, and skipping this
        // value's destructor makes the returned box the only owner.
        unsafe { Box::from_raw(this.pointer.as_ptr()) }
    }
}

impl<T: ?Sized> Deref for AliasableBox<T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the allocation lives until this value is dropped, and it is
        // written only through `as_mut_ptr`, whose exclusive borrow cannot
        // overlap the shared one this reference is tied to.
        unsafe { self.pointer.as_ref() }
    }
}

impl<T: ?Sized> Drop for AliasableBox<T> {
    fn drop(&mut self) {
        // SAFETY: the pointer came from a box in `new` and is released once.
        drop(unsafe { Box::from_raw(self.pointer.as_ptr()) });
    }
}

impl<T: Clone> Clone for AliasableBox<[T]> {
    /// A new allocation with a copy of the elements, read through a shared
    /// reference, which no write by the SDK can overlap.
    fn clone(&self) -> Self {
        Self::new(Box::from(&**self))
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for AliasableBox<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

// SAFETY: the value owns its allocation exactly as a `Box<T>` does.
unsafe impl<T: ?Sized + Send> Send for AliasableBox<T> {}
// SAFETY: a shared reference gives only `&T`.
unsafe impl<T: ?Sized + Sync> Sync for AliasableBox<T> {}

/// Vector allocation held without an exclusive-reference owning pointer.
///
/// Like [`AliasableBox`], SDK pointers remain valid when this owner moves.
/// Unlike a boxed slice, it preserves the caller's spare capacity for pooling.
pub(crate) struct AliasableVec<T> {
    pointer: NonNull<T>,
    len: usize,
    capacity: usize,
}

impl<T> AliasableVec<T> {
    pub(crate) fn new(value: Vec<T>) -> Self {
        let mut value = ManuallyDrop::new(value);
        Self {
            pointer: NonNull::new(value.as_mut_ptr()).expect("Vec pointers are non-null"),
            len: value.len(),
            capacity: value.capacity(),
        }
    }

    pub(crate) fn as_mut_ptr(&mut self) -> *mut T {
        self.pointer.as_ptr()
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    pub(crate) fn into_vec(self) -> Vec<T> {
        let this = ManuallyDrop::new(self);
        // SAFETY: all three values came from one Vec in `new`, its elements
        // and allocation remain live, and the original destructor was skipped.
        unsafe { Vec::from_raw_parts(this.pointer.as_ptr(), this.len, this.capacity) }
    }
}

impl<T> Deref for AliasableVec<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        // SAFETY: `len` initialized elements remain live until Drop. Native
        // writes require exclusive frame ownership, excluding this borrow.
        unsafe { std::slice::from_raw_parts(self.pointer.as_ptr(), self.len) }
    }
}

impl<T> Drop for AliasableVec<T> {
    fn drop(&mut self) {
        // SAFETY: as in `into_vec`; Drop runs exactly once for this owner.
        drop(unsafe { Vec::from_raw_parts(self.pointer.as_ptr(), self.len, self.capacity) });
    }
}

impl<T: Clone> Clone for AliasableVec<T> {
    fn clone(&self) -> Self {
        Self::new(self.to_vec())
    }
}

impl<T: fmt::Debug> fmt::Debug for AliasableVec<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

// SAFETY: the allocation has the same ownership and shared access as Vec<T>.
unsafe impl<T: Send> Send for AliasableVec<T> {}
// SAFETY: shared access only yields &[T]; native writes require exclusive ownership.
unsafe impl<T: Sync> Sync for AliasableVec<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the SDK does to a frame it was lent: it writes through its copy
    /// of the pointer after the owner has moved, and the owner reads the
    /// result once it has the allocation to itself again.
    #[test]
    fn writes_through_a_lent_pointer_survive_moving_the_owner() {
        let mut bytes = AliasableBox::new(vec![1u8, 2, 3].into_boxed_slice());
        let lent = bytes.as_mut_ptr().cast::<u8>();
        let moved = Box::new(bytes);
        // SAFETY: the allocation is alive, three bytes long, and no reference
        // into it exists.
        unsafe { lent.add(1).write(9) };
        assert_eq!(**moved, [1, 9, 3]);
        assert_eq!(moved.as_ptr().cast::<u8>(), lent.cast_const());
        assert_eq!(*moved.into_box(), [1, 9, 3]);
    }

    /// Capacity remains available after an SDK write and a move of its owner.
    #[test]
    fn vector_storage_preserves_pointer_capacity_and_native_writes() {
        let mut original = Vec::with_capacity(1024);
        original.extend_from_slice(&[1u8, 2, 3]);
        let (pointer, capacity) = (original.as_ptr(), original.capacity());
        let mut storage = AliasableVec::new(original);
        let lent = storage.as_mut_ptr();
        let moved = Box::new(storage);
        // SAFETY: the Vec allocation is live; the owner has moved but no
        // element reference is alive, and element 1 is initialized.
        unsafe { lent.add(1).write(9) };
        assert_eq!(**moved, [1, 9, 3]);
        let recovered = moved.into_vec();
        assert_eq!(recovered, [1, 9, 3]);
        assert_eq!(
            (recovered.as_ptr(), recovered.capacity()),
            (pointer, capacity)
        );
    }

    #[test]
    fn vector_elements_are_dropped_once_including_after_recovery() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        struct Counted(Arc<AtomicUsize>);
        impl Drop for Counted {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        drop(AliasableVec::new(vec![
            Counted(drops.clone()),
            Counted(drops.clone()),
        ]));
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        let recovered = AliasableVec::new(vec![Counted(drops.clone())]).into_vec();
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        drop(recovered);
        assert_eq!(drops.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn vector_storage_round_trips_empty_and_zero_sized_allocations() {
        let empty: Vec<u8> = Vec::new();
        let pointer = empty.as_ptr();
        let recovered = AliasableVec::new(empty).into_vec();
        assert_eq!(
            (recovered.as_ptr(), recovered.len(), recovered.capacity()),
            (pointer, 0, 0)
        );
        let spare = Vec::<u8>::with_capacity(64);
        let (pointer, capacity) = (spare.as_ptr(), spare.capacity());
        let recovered = AliasableVec::new(spare).into_vec();
        assert_eq!(
            (recovered.as_ptr(), recovered.len(), recovered.capacity()),
            (pointer, 0, capacity)
        );
        let zero_sized = vec![(); 3];
        let recovered = AliasableVec::new(zero_sized).into_vec();
        assert_eq!(recovered, [(); 3]);
        assert_eq!(recovered.capacity(), usize::MAX);
        drop(AliasableVec::new(Vec::<()>::new()));
    }

    #[test]
    fn the_allocation_is_released_exactly_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static DROPS: AtomicUsize = AtomicUsize::new(0);
        struct Counted;
        impl Drop for Counted {
            fn drop(&mut self) {
                DROPS.fetch_add(1, Ordering::Relaxed);
            }
        }
        drop(AliasableBox::new(Box::new(Counted)));
        assert_eq!(DROPS.load(Ordering::Relaxed), 1);
        drop(AliasableBox::new(Box::new(Counted)).into_box());
        assert_eq!(DROPS.load(Ordering::Relaxed), 2);
    }
}
