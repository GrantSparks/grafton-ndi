use std::fmt;

use crate::Error;

/// A failed operation that hands back the value it would have consumed.
///
/// Operations in this module that take ownership of a receiver, a sender or a
/// frame return this on failure, so the caller keeps the object and can use
/// it, retry, or drop it deliberately.
///
/// `Rejected<T>` converts into [`Error`] (dropping the value), so `?` works in
/// a function that returns [`crate::Result`].
///
/// # Examples
///
/// ```
/// use grafton_ndi::advanced::{CompressedVideoFrame, VideoCodec, VideoPacketFormat};
///
/// let format = VideoPacketFormat::new(VideoCodec::H264, 1920, 1080)?;
/// // Not an H.264 packet envelope: the bytes come back with the reason.
/// let rejected = CompressedVideoFrame::new(format, vec![0; 16]).unwrap_err();
/// println!("not sendable: {rejected}");
/// let bytes: Vec<u8> = rejected.into_inner();
/// assert_eq!(bytes.len(), 16);
/// # Ok::<(), grafton_ndi::Error>(())
/// ```
pub struct Rejected<T> {
    error: Error,
    value: T,
}

impl<T> Rejected<T> {
    pub(crate) fn new(error: Error, value: T) -> Self {
        Self { error, value }
    }

    /// Why the operation failed.
    pub fn error(&self) -> &Error {
        &self.error
    }

    /// The value the operation did not consume.
    pub fn into_inner(self) -> T {
        self.value
    }

    /// The reason, discarding the value.
    pub fn into_error(self) -> Error {
        self.error
    }

    /// The reason and the value.
    pub fn into_parts(self) -> (Error, T) {
        (self.error, self.value)
    }

    /// Replace the returned value, keeping the reason.
    pub(crate) fn map<U>(self, value: impl FnOnce(T) -> U) -> Rejected<U> {
        Rejected {
            error: self.error,
            value: value(self.value),
        }
    }
}

// The value is often a native object with no useful `Debug` output, and none
// is required of it: only its type is shown.
impl<T> fmt::Debug for Rejected<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rejected")
            .field("error", &self.error)
            .field("value", &std::any::type_name::<T>())
            .finish()
    }
}

impl<T> fmt::Display for Rejected<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

// Transparent: the message and the source are those of the reason.
impl<T> std::error::Error for Rejected<T> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        std::error::Error::source(&self.error)
    }
}

impl<T> From<Rejected<T>> for Error {
    fn from(rejected: Rejected<T>) -> Self {
        rejected.error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_the_value_and_converts_to_the_reason() {
        let rejected = Rejected::new(Error::InvalidFrame("too short".into()), vec![1u8, 2]);
        assert_eq!(rejected.to_string(), rejected.error().to_string());
        assert!(format!("{rejected:?}").contains("Vec<u8>"));

        let (error, value) = rejected.into_parts();
        assert!(matches!(error, Error::InvalidFrame(_)));
        assert_eq!(value, [1, 2]);

        fn through_question_mark() -> crate::Result<()> {
            Err(Rejected::new(Error::InvalidFrame("x".into()), ()))?
        }
        assert!(matches!(
            through_question_mark(),
            Err(Error::InvalidFrame(_))
        ));
    }
}
