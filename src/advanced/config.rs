use std::{ffi::CString, fmt, sync::Arc};

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
#[derive(Clone)]
pub struct AdvancedConfig(Arc<CString>);

impl AdvancedConfig {
    /// Accept a JSON object of at most one mebibyte.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when the text is too large, is not
    /// valid JSON, is not an object, or contains a NUL.
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
                "Advanced configuration exceeds its byte limit".into(),
            ));
        }
        let value: serde_json::Value = serde_json::from_str(json)
            .map_err(|_| Error::InvalidInput("Invalid Advanced configuration JSON".into()))?;
        if !value.is_object() {
            return Err(Error::InvalidInput(
                "Advanced configuration must be a JSON object".into(),
            ));
        }
        let json = CString::new(json)
            .map_err(|_| Error::InvalidInput("Advanced configuration contains NUL".into()))?;
        Ok(Self(Arc::new(json)))
    }

    /// The JSON text exactly as it was given. It may contain vendor
    /// credentials or network details; the name is a reminder not to log it.
    pub fn expose_json(&self) -> &str {
        // Construction accepts UTF-8 and CString never changes those bytes.
        self.0.to_str().expect("validated configuration UTF-8")
    }

    /// `config`, or the empty configuration `{}` without one: the
    /// configuration of an object whose only creation function takes one.
    pub(crate) fn or_empty(config: Option<&Self>) -> Self {
        config
            .cloned()
            .unwrap_or_else(|| Self(Arc::new(c"{}".to_owned())))
    }

    /// The text as a C string. Every clone shares this one buffer, which
    /// neither moves nor is freed while any clone of `self` lives.
    pub(crate) fn as_ptr(&self) -> *const std::ffi::c_char {
        self.0.as_ptr()
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
