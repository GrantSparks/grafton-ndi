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
/// [`Genlock::new`](super::Genlock::new) takes it directly as an argument.
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

    /// `config`, or the empty configuration `{}` without one: the
    /// configuration of an object whose only creation function takes one.
    pub(crate) fn or_empty(config: Option<&Self>) -> Self {
        config.cloned().unwrap_or_else(|| Self(Arc::from("{}\0")))
    }

    /// The text as a C string. Every clone shares this one buffer, which
    /// neither moves nor is freed while any clone of `self` lives.
    pub(crate) fn as_ptr(&self) -> *const std::ffi::c_char {
        self.0.as_ptr().cast()
    }
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
    fn a_missing_configuration_becomes_the_empty_object() {
        let empty = AdvancedConfig::or_empty(None);
        assert_eq!(empty.expose_json(), "{}");
        assert_eq!(
            empty.expose_json(),
            AdvancedConfig::parse("{}").unwrap().expose_json()
        );
        let given = AdvancedConfig::parse(r#"{"ndi":{}}"#).unwrap();
        let kept = AdvancedConfig::or_empty(Some(&given));
        // The same shared buffer, not a copy of the text.
        assert_eq!(kept.as_ptr(), given.as_ptr());
    }
}
