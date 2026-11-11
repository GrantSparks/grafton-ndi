use std::fmt;

use crate::Error;

/// A failed operation that hands back the value it would have consumed.
///
/// Operations that take ownership of a value the caller cannot recreate — a
/// receiver, a sender, a frame, a registration — return this on failure, so
/// the caller keeps the value and can use it, retry, or drop it deliberately.
///
/// `Rejected<T>` converts into [`Error`] (dropping the value), so `?` works in
/// a function that returns [`crate::Result`].
///
/// # Examples
///
/// ```no_run
/// use grafton_ndi::{
///     discovery::{AdvertiserOptions, SendAdvertisement, SendAdvertiser},
///     Sender, SenderOptions, NDI,
/// };
///
/// # fn main() -> Result<(), grafton_ndi::Error> {
/// let ndi = NDI::new()?;
/// let server = AdvertiserOptions::default().servers("127.0.0.1");
/// let mut studio = SendAdvertiser::new(&ndi, &server)?;
/// let mut backup = SendAdvertiser::new(&ndi, &server)?;
/// let sender = Sender::new(&ndi, &SenderOptions::builder("Camera 1").build())?;
/// let registration = studio.register(&sender, SendAdvertisement::default())?;
///
/// // Only the advertiser that issued a registration can remove it. Another
/// // one hands it back with the reason instead of consuming it.
/// let rejected = backup.unregister(registration).unwrap_err();
/// println!("still registered: {rejected}");
/// studio.unregister(rejected.into_inner())?;
/// # Ok(())
/// # }
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
    #[cfg(feature = "advanced")]
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
