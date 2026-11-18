use std::{fmt, sync::Arc};

use crate::{Error, Result};

/// The JSON configuration of one Advanced SDK object.
///
/// The SDK accepts a JSON document when it creates a sender, receiver, finder
/// and several other objects; the document's properties are defined by the
/// NDI Advanced SDK documentation. This type holds one such document: it is
/// checked to be a JSON object, kept byte for byte (unknown properties
/// included), immutable, and cheap to clone. Most objects take it through an
/// options value's `config` setter, such as
/// [`SenderOptions::config`](crate::SenderOptions::config).
/// Genlock uses [`GenlockOptions::config`](super::GenlockOptions::config).
///
/// A configuration can carry vendor credentials or network details, so its
/// `Debug` output shows no content and no error message quotes it.
///
/// # Examples
///
/// ```
/// use grafton_ndi::advanced::AdvancedConfig;
///
/// let config = AdvancedConfig::parse(r#"{"ndi": {"machinename": "studio-a"}}"#)?;
/// assert_eq!(format!("{config:?}"), "AdvancedConfig([REDACTED])");
/// # Ok::<(), grafton_ndi::Error>(())
/// ```
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct AdvancedConfig(Arc<str>);

impl AdvancedConfig {
    /// Accept a JSON object of at most one mebibyte.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when the text is too large, is not
    /// valid JSON (including a raw NUL byte), or is not an object.
    pub fn parse(json: impl AsRef<str>) -> Result<Self> {
        Self::parse_with_limit(json, 1024 * 1024)
    }

    /// Accept a JSON object of at most `maximum_bytes` bytes.
    ///
    /// # Errors
    ///
    /// As for [`parse`](Self::parse).
    pub fn parse_with_limit(json: impl AsRef<str>, maximum_bytes: usize) -> Result<Self> {
        let json = json.as_ref();
        if json.len() > maximum_bytes {
            return Err(Error::InvalidInput(
                crate::InputDefect::ConfigurationTooLarge,
            ));
        }
        let value: serde_json::Value = serde_json::from_str(json)
            .map_err(|_| Error::InvalidInput(crate::InputDefect::ConfigurationJson))?;
        if !value.is_object() {
            return Err(Error::InvalidInput(
                crate::InputDefect::ConfigurationNotObject,
            ));
        }
        let mut terminated = String::with_capacity(json.len() + 1);
        terminated.push_str(json);
        terminated.push('\0');
        Ok(Self(Arc::from(terminated)))
    }

    /// The JSON text exactly as it was given. It may contain vendor
    /// credentials or network details; the name is a reminder not to log it.
    pub fn expose_json(&self) -> &str {
        &self.0[..self.0.len() - 1]
    }

    /// Lend the exact buffer, or NULL for the SDK's default configuration.
    /// Retain the returned owner until the native instance is destroyed.
    pub(crate) fn lend<R>(
        config: Option<&Self>,
        call: impl FnOnce(*const std::ffi::c_char) -> R,
    ) -> (R, RetainedConfig) {
        let config = config.cloned();
        let result = call(config.as_ref().map_or(std::ptr::null(), Self::as_ptr));
        (result, RetainedConfig { _config: config })
    }

    /// The text as a C string. Every clone shares this one buffer, which
    /// neither moves nor is freed while any clone of `self` lives.
    fn as_ptr(&self) -> *const std::ffi::c_char {
        self.0.as_ptr().cast()
    }
}

/// The configuration that was lent to an SDK creation call.
#[derive(Default)]
pub(crate) struct RetainedConfig {
    _config: Option<AdvancedConfig>,
}

impl fmt::Debug for AdvancedConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AdvancedConfig([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_extensions_and_redacts_values() {
        let input = r#" {"ndi":{"vendor":{"id":"private-value"}},"future":[1,2]} "#;
        let config = AdvancedConfig::parse(input).unwrap();
        assert_eq!(config.expose_json(), input);
        assert!(!format!("{config:?}").contains("private-value"));
        assert!(AdvancedConfig::parse("[1]").is_err());
        assert!(AdvancedConfig::parse_with_limit(input, 3).is_err());
        let error = AdvancedConfig::parse("{\"private-value\":").unwrap_err();
        assert!(!error.to_string().contains("private-value"));
    }

    #[test]
    fn rust_and_native_views_share_the_exact_terminated_utf8() {
        let input = " {\"label\":\"カメラ\",\"escaped\":\"\\u0000\"} ";
        let config = AdvancedConfig::parse(input).unwrap();
        let clone = config.clone();
        assert_eq!(config.as_ptr(), clone.as_ptr());
        // SAFETY: construction appends the sole NUL to the shared UTF-8 buffer.
        let native = unsafe { std::ffi::CStr::from_ptr(config.as_ptr()) };
        assert_eq!(native.to_str().unwrap(), input);
        assert_eq!(native.to_bytes(), config.expose_json().as_bytes());
        assert_eq!(config.expose_json().as_ptr(), config.as_ptr().cast());
        // Raw NUL is invalid JSON at every position, including before and
        // after the object. An escaped \\u0000 remains valid above.
        for position in 0..=input.len() {
            if input.is_char_boundary(position) {
                let mut with_nul = input.to_owned();
                with_nul.insert(position, '\0');
                assert!(matches!(
                    AdvancedConfig::parse(with_nul),
                    Err(Error::InvalidInput(crate::InputDefect::ConfigurationJson))
                ));
            }
        }
    }

    #[test]
    fn lending_keeps_the_same_buffer_alive_and_passes_null_when_unset() {
        let (pointer, retained) = AdvancedConfig::lend(None, |pointer| pointer);
        assert!(pointer.is_null());
        assert!(retained._config.is_none());

        let input = r#"{"ndi":{}}"#;
        let given = AdvancedConfig::parse(input).unwrap();
        let (pointer, retained) = AdvancedConfig::lend(Some(&given), |pointer| pointer);
        assert_eq!(pointer, given.as_ptr());
        let buffer = Arc::downgrade(&given.0);
        drop(given);
        // SAFETY: the retained owner keeps this exact buffer alive.
        assert_eq!(
            unsafe { std::ffi::CStr::from_ptr(pointer) }
                .to_str()
                .unwrap(),
            input
        );
        assert!(buffer.upgrade().is_some());
        drop(retained);
        assert!(buffer.upgrade().is_none());
    }
}
