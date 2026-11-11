//! The private native boundary: vendored SDK declarations, typed function
//! tables and the loader that fills them.
//!
//! Nothing outside this module converts an address into a function pointer or
//! looks a symbol up by name.

#[allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code,
    unused_imports,
    clippy::all,
    clippy::pedantic
)]
pub(crate) mod bindings;

/// The foreign functions the vendored headers declare.
///
/// These exist to give every table entry a signature to be checked against and
/// to name the symbols a statically linked archive defines. They are kept apart
/// from the types so that importing [`bindings`] never brings a callable foreign
/// function into scope: on dynamically loaded targets nothing links them.
#[allow(non_snake_case, dead_code, clippy::all, clippy::pedantic)]
pub(crate) mod declarations;

/// Types from the Advanced-only headers, generated at build time from the
/// user's installed SDK because those headers may not be redistributed.
#[cfg(ndi_has_advanced_header)]
#[allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    dead_code,
    clippy::all,
    clippy::pedantic
)]
pub(crate) mod advanced_bindings {
    use super::bindings::*;
    include!(concat!(env!("OUT_DIR"), "/ndi_advanced.rs"));
}

/// The foreign functions the Advanced-only headers declare; see [`declarations`].
#[cfg(ndi_has_advanced_header)]
#[allow(non_snake_case, dead_code, clippy::all, clippy::pedantic)]
pub(crate) mod advanced_declarations {
    use super::{advanced_bindings::*, bindings::*};
    include!(concat!(env!("OUT_DIR"), "/ndi_advanced_declarations.rs"));
}

mod api;
mod loader;
mod selection;
pub(crate) mod table;

#[cfg(test)]
mod coverage_tests;

pub(crate) use api::*;
pub(crate) use loader::{select, AdvancedEntryPoints, Runtime};
