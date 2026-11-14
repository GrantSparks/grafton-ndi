//! Select the runtime required by a native test.

use super::{
    api::{Error, Ndi},
    identity::assert_expected_identity,
    policy::{skip, Requirement},
};

/// An initialized runtime, or `None` when no NDI runtime library can be loaded.
///
/// A test that needs the real SDK starts with
/// `let Some(ndi) = ndi() else { return };`. Every failure other than a missing
/// runtime library panics.
pub fn ndi() -> Option<Ndi> {
    match Ndi::new() {
        Ok(ndi) => {
            assert_expected_identity(ndi.info());
            Some(ndi)
        }
        Err(Error::RuntimeLoadFailed(reason)) if !reason.loaded_any() => {
            skip(
                Requirement::Runtime,
                &format!("no NDI runtime could be loaded: {reason}"),
            );
            None
        }
        Err(error) => panic!("the NDI runtime loaded but did not initialize: {error}"),
    }
}
