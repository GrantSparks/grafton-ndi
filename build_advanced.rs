//! Availability of Advanced declarations, derived from the installed headers.
//!
//! The table registry is shared with the native boundary. A family is usable
//! only when every function it needs is declared; newer families must not make
//! an older SDK fail to compile. Individual constants and types are selected by
//! the same mechanism, without guessing from an SDK version or file name.

use std::collections::BTreeSet;

// Only the families and their functions matter here, not the `RuntimeFeature`
// each is gated by.
macro_rules! advanced_tables {
    ($(
        $(#[$meta:meta])*
        struct $Table:ident in $field:ident: $feature:ident {
            $(fn $name:ident($($argument:ident: $type:ty),* $(,)?) $(-> $return:ty)?;)+
        }
    )+) => {
        const FAMILIES: &[(&str, &[&str])] = &[
            $((stringify!($Table), &[$(stringify!($name)),+])),+
        ];
    };
}

include!("src/sys/advanced_tables.rs");

/// The available declaration selector and whether every known family exists.
pub struct Selection {
    pub source: String,
    pub complete: bool,
}

/// Generate a token selector. An unavailable branch is discarded before Rust
/// resolves its names, so wrappers keep one public API even with partial SDKs.
pub fn selection(types: &str, functions: &str) -> Selection {
    let mut names = BTreeSet::new();
    for source in [types, functions] {
        for item in syn::parse_file(source)
            .expect("bindgen emitted valid Rust")
            .items
        {
            let name = match item {
                syn::Item::Const(item) => Some(item.ident),
                syn::Item::Enum(item) => Some(item.ident),
                syn::Item::Struct(item) => Some(item.ident),
                syn::Item::Type(item) => Some(item.ident),
                syn::Item::Union(item) => Some(item.ident),
                syn::Item::ForeignMod(block) => {
                    for item in block.items {
                        if let syn::ForeignItem::Fn(function) = item {
                            names.insert(function.sig.ident.to_string());
                        }
                    }
                    None
                }
                _ => None,
            };
            if let Some(name) = name {
                names.insert(name.to_string());
            }
        }
    }
    for (family, functions) in FAMILIES {
        if functions.iter().all(|name| names.contains(*name)) {
            names.insert((*family).to_owned());
        }
    }
    let complete = FAMILIES.iter().all(|(family, _)| names.contains(*family));
    let mut source = String::from("macro_rules! declared {\n");
    for name in names {
        source.push_str(&format!(
            "    ({name} {{ $($yes:tt)* }} else {{ $($no:tt)* }}) => {{ $($yes)* }};\n"
        ));
    }
    source.push_str("    ($name:ident { $($yes:tt)* } else { $($no:tt)* }) => { $($no)* };\n}\n");
    Selection { source, complete }
}
