//! Native configuration strings whose absence inherits the SDK configuration.

use std::ffi::CString;

use crate::{Error, InputDefect, Result};

/// Validate and join a configured list only at native-owner construction.
/// Every element must be representable without changing list boundaries.
pub(crate) fn list_cstring(
    values: Option<&[String]>,
    setting: &'static str,
) -> Result<Option<CString>> {
    values
        .map(|values| {
            if values.is_empty() {
                return Err(Error::InvalidInput(InputDefect::Empty { what: setting }));
            }
            let invalid = |index| Error::InvalidInput(InputDefect::ListElement { setting, index });
            let boundary = values
                .iter()
                .position(|value| value.is_empty() || value.contains(','));
            // Only earlier elements can take precedence over a boundary error.
            // CString supplies the sole NUL check; every comma before that NUL
            // is a separator, so its count is the original element index.
            let prefix = &values[..boundary.unwrap_or(values.len())];
            let joined = CString::new(prefix.join(",")).map_err(|error| {
                let nul = error.nul_position();
                invalid(
                    error.into_vec()[..nul]
                        .iter()
                        .filter(|&&byte| byte == b',')
                        .count(),
                )
            })?;
            if let Some(index) = boundary {
                return Err(invalid(index));
            }
            Ok(joined)
        })
        .transpose()
}

/// Check a single explicitly configured Discovery Server endpoint.
pub(crate) fn endpoint_cstring(text: &str) -> Result<CString> {
    if text.is_empty() {
        return Err(Error::InvalidInput(InputDefect::Empty { what: "server" }));
    }
    // Preserve the public refusal order: NUL before a server list.
    let endpoint = CString::new(text)?;
    if text.contains(',') {
        return Err(Error::InvalidInput(InputDefect::MultipleDiscoveryServers));
    }
    Ok(endpoint)
}
