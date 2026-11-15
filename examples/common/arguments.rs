//! Required option parsing shared by command-line examples.

/// Consume and parse a required option value, reporting malformed input.
pub fn option_value<T: std::str::FromStr>(
    arguments: &mut impl Iterator<Item = String>,
    option: &str,
) -> Result<T, Box<dyn std::error::Error>> {
    let value = arguments
        .next()
        .ok_or_else(|| format!("{option} needs a value"))?;
    value
        .parse()
        .map_err(|_| format!("invalid value for {option}: {value}").into())
}
