//! Bounded reads of source and version strings supplied by the SDK.

use std::{ffi::c_char, slice};

use crate::{Error, Result};

/// One native string, including its trailing NUL (the metadata limit too).
pub(crate) const MAX_SDK_STRING_BYTES: usize = 4 * 1024 * 1024;

/// Read native bytes without changing the caller's UTF-8 policy.
///
/// # Safety
/// `pointer` must be non-null and readable through its first NUL or through
/// `MAX_SDK_STRING_BYTES` bytes, whichever comes first. That storage must remain
/// live and immutable for the returned borrow. A bound cannot validate pointers.
pub(crate) unsafe fn read<'a>(pointer: *const c_char, operation: &str) -> Result<&'a [u8]> {
    let bytes = pointer.cast::<u8>();
    for length in 0..MAX_SDK_STRING_BYTES {
        // SAFETY: the caller guarantees readable bytes until NUL or the limit.
        if unsafe { bytes.add(length).read() } == 0 {
            // SAFETY: these bytes were just read and the caller retains storage.
            return Ok(unsafe { slice::from_raw_parts(bytes, length) });
        }
    }
    Err(Error::UnexpectedSdkResult(format!(
        "{operation}: string has no NUL terminator within 4 MiB"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_only_to_the_first_nul_and_enforces_the_exact_limit() {
        // A short allocation must not be read as a full 4 MiB slice.
        for text in [c"".to_bytes_with_nul(), b"text\0ignored"] {
            let expected = text.split(|&byte| byte == 0).next().unwrap();
            // SAFETY: text is live and has a terminator within its allocation.
            assert_eq!(
                unsafe { read(text.as_ptr().cast(), "test") }.unwrap(),
                expected
            );
        }
        let mut text = vec![b'x'; MAX_SDK_STRING_BYTES + 1];
        text[MAX_SDK_STRING_BYTES - 1] = 0;
        // SAFETY: text stays live, including through the scan limit.
        assert_eq!(
            unsafe { read(text.as_ptr().cast(), "test") }.unwrap().len(),
            MAX_SDK_STRING_BYTES - 1
        );
        text[MAX_SDK_STRING_BYTES - 1] = b'x';
        text[MAX_SDK_STRING_BYTES] = 0;
        // SAFETY: the whole scan limit is readable even without a NUL in it.
        assert!(matches!(
            unsafe { read(text.as_ptr().cast(), "test") },
            Err(Error::UnexpectedSdkResult(_))
        ));
    }
}
