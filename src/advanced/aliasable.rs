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
/// A shared reference only reads. [`as_mut_ptr`](Self::as_mut_ptr) needs an
/// exclusive one, so whoever lets the SDK write holds the allocation
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
    pub(crate) fn as_mut_ptr(&mut self) -> *mut T {
        self.pointer.as_ptr()
    }

    /// Give the allocation back to an ordinary box.
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

impl<T: ?Sized + fmt::Debug> fmt::Debug for AliasableBox<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        (**self).fmt(f)
    }
}

// SAFETY: the value owns its allocation exactly as a `Box<T>` does.
unsafe impl<T: ?Sized + Send> Send for AliasableBox<T> {}
// SAFETY: a shared reference gives only `&T`.
unsafe impl<T: ?Sized + Sync> Sync for AliasableBox<T> {}

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
