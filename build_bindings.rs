//! Inspect generated declarations independently of optional source formatting.

use std::collections::{BTreeSet, HashSet};

pub fn foreign_functions(source: &str) -> Result<BTreeSet<String>, syn::Error> {
    let syntax = syn::parse_file(source)?;
    let mut names = BTreeSet::new();
    for item in syntax.items {
        let syn::Item::ForeignMod(block) = item else {
            continue;
        };
        for item in block.items {
            let syn::ForeignItem::Fn(function) = item else {
                continue;
            };
            let name = function.sig.ident.to_string();
            if matches!(function.vis, syn::Visibility::Public(_))
                && name.starts_with("NDIlib_")
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                names.insert(name);
            }
        }
    }
    Ok(names)
}

pub fn static_functions(
    source: &str,
    exports: &HashSet<String>,
) -> Result<BTreeSet<String>, String> {
    let declared = foreign_functions(source)
        .map_err(|error| format!("generated NDI declarations are invalid Rust: {error}"))?;
    let mapped: BTreeSet<_> = declared
        .into_iter()
        .filter(|name| exports.contains(name))
        .collect();
    // These three entry points are fundamental to every provider. The full
    // typed baseline remains defined once in src/dispatch.rs.
    let missing: Vec<_> = ["NDIlib_initialize", "NDIlib_destroy", "NDIlib_version"]
        .into_iter()
        .filter(|name| !mapped.contains(*name))
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "static NDI address map is missing essential functions: {}",
            missing.join(", ")
        ));
    }
    Ok(mapped)
}
