//! Required option parsing shared by command-line examples.

use grafton_ndi::Result;

/// Consume and parse a required option value, reporting malformed input.
pub fn option_value<T: std::str::FromStr>(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<T> {
    let value = arguments
        .next()
        .ok_or_else(|| grafton_ndi::Error::InvalidInput(format!("{option} needs a value")))?;
    value.parse().map_err(|_| {
        grafton_ndi::Error::InvalidInput(format!("invalid value for {option}: {value}"))
    })
}
