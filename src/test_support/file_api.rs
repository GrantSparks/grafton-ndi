//! Require the selected runtime and its File library.

use super::{
    api::{Error, Ndi},
    ndi,
    policy::{skip, Requirement},
};

/// The selected runtime and its File library, or a reported prerequisite skip.
///
/// Runtime identity and `GRAFTON_NDI_REQUIRE_RUNTIME` use the same policy as
/// [`ndi`]. Missing File libraries and incompatible runtime editions report
/// [`Requirement::File`]; every other File-loading error fails the test.
#[cfg(feature = "file-io")]
pub fn file_api() -> Option<(Ndi, super::api::file::FileApi)> {
    use super::api::file::{FileApi, FileRuntimeOptions};
    let ndi = ndi()?;
    match FileApi::open(&ndi, &FileRuntimeOptions::default()) {
        Ok(api) => Some((ndi, api)),
        Err(Error::RuntimeLoadFailed(reason) | Error::RuntimeConflict(reason)) => {
            skip(
                Requirement::File,
                &format!("the NDI File library is unavailable: {reason}"),
            );
            None
        }
        Err(error) => panic!("the NDI File library could not be selected: {error}"),
    }
}
