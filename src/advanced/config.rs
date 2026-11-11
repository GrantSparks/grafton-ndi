use std::{ffi::CString, fmt, sync::Arc};

use crate::{Error, Result};

/// Immutable per-instance SDK JSON. Debug output deliberately redacts its contents.
#[derive(Clone)]
pub struct AdvancedConfig(Arc<CString>);

impl AdvancedConfig {
    /// Parse a JSON object, preserving unknown properties and the original text.
    pub fn parse(json: impl AsRef<str>) -> Result<Self> {
        Self::parse_with_limit(json, 1024 * 1024)
    }

    /// Parse with an explicit maximum byte size. Error messages never echo JSON.
    pub fn parse_with_limit(json: impl AsRef<str>, maximum_bytes: usize) -> Result<Self> {
        let json = json.as_ref();
        if json.len() > maximum_bytes {
            return Err(Error::InvalidConfiguration(
                "Advanced configuration exceeds its byte limit".into(),
            ));
        }
        let value: serde_json::Value = serde_json::from_str(json).map_err(|_| {
            Error::InvalidConfiguration("Invalid Advanced configuration JSON".into())
        })?;
        if !value.is_object() {
            return Err(Error::InvalidConfiguration(
                "Advanced configuration must be a JSON object".into(),
            ));
        }
        let json = CString::new(json).map_err(|_| {
            Error::InvalidConfiguration("Advanced configuration contains NUL".into())
        })?;
        Ok(Self(Arc::new(json)))
    }

    /// Explicitly expose JSON, which may contain vendor credentials or network details.
    pub fn expose_json(&self) -> &str {
        // Construction accepts UTF-8 and CString never changes those bytes.
        self.0.to_str().expect("validated configuration UTF-8")
    }

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
}
