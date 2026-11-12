//! Storage for the structures the File library fills in through a pointer.

use crate::{Error, Result};

/// Bytes kept after every structure the File library writes.
const GUARD: usize = 4096;

/// The value every guard byte holds while the guard is intact.
const SENTINEL: u8 = 0xA5;

/// A zeroed `T` for the File library to fill in, followed by a guard region.
///
/// The library writes these structures through a bare pointer, with the size
/// they had in the headers it was built from. It is located at run time and
/// reports no version, so that size cannot be compared with the layout this
/// crate was built with before the call. Should a later library have grown a
/// structure, the extra bytes land in the guard rather than past the end of the
/// storage, and [`OutParam::read`] reports the mismatch instead of returning a
/// value decoded with the wrong layout.
#[repr(C)]
pub(super) struct OutParam<T> {
    value: T,
    guard: [u8; GUARD],
}

impl<T: Copy + Default> OutParam<T> {
    /// Fresh storage holding `T::default()`, which is all-zero for SDK structures.
    pub(super) fn new() -> Self {
        Self {
            value: T::default(),
            guard: [SENTINEL; GUARD],
        }
    }

    /// The pointer to pass to the File library.
    ///
    /// It is derived from the whole storage, so a write that runs past `T`
    /// stays inside the allocation the pointer is valid for.
    pub(super) fn as_mut_ptr(&mut self) -> *mut T {
        std::ptr::from_mut(self).cast()
    }

    /// The structure the library wrote.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedSdkResult`] when the library wrote more than this crate's
    /// layout of `T`, in which case nothing it wrote can be trusted.
    pub(super) fn read(&self) -> Result<T> {
        if self.guard.iter().any(|&byte| byte != SENTINEL) {
            return Err(Error::UnexpectedSdkResult(format!(
                "the File library wrote past the {} bytes this crate's SDK headers give {}",
                size_of::<T>(),
                std::any::type_name::<T>()
            )));
        }
        Ok(self.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::bindings::NDIlib_file_recorder_event_t;

    #[test]
    fn a_write_within_the_layout_is_returned() {
        let mut out = OutParam::<u64>::new();
        // SAFETY: the pointer is valid for one u64.
        unsafe { out.as_mut_ptr().write(7) };
        assert_eq!(out.read().unwrap(), 7);
    }

    #[test]
    fn a_library_that_writes_a_larger_structure_is_detected_not_trusted() {
        let mut out = OutParam::<NDIlib_file_recorder_event_t>::new();
        let larger = size_of::<NDIlib_file_recorder_event_t>() + 24;
        // SAFETY: the pointer is valid for the structure and its guard, and
        // `larger` stays within them.
        unsafe { out.as_mut_ptr().cast::<u8>().write_bytes(0, larger) };
        let error = out.read().unwrap_err();
        assert!(matches!(error, Error::UnexpectedSdkResult(_)), "{error}");
    }

    #[test]
    fn the_guard_starts_where_the_structure_ends() {
        let out = OutParam::<NDIlib_file_recorder_event_t>::new();
        let start = std::ptr::from_ref(&out) as usize;
        assert_eq!(
            out.guard.as_ptr() as usize - start,
            size_of::<NDIlib_file_recorder_event_t>()
        );
    }
}
