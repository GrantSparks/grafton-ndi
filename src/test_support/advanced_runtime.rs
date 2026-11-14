//! Require the Advanced runtime for a native test.

use super::{
    api::{capabilities::RuntimeFeature, Ndi},
    ndi,
    policy::{skip, Requirement},
};

/// An initialized Advanced SDK runtime, or `None` when the selected runtime is
/// not one or this build lacks the Advanced SDK declarations.
pub fn advanced_ndi() -> Option<Ndi> {
    let ndi = ndi()?;
    match ndi.require_feature(RuntimeFeature::AdvancedSenderConfiguration) {
        Ok(()) => Some(ndi),
        Err(error) => {
            skip(
                Requirement::Advanced,
                &format!("the Advanced SDK cannot be used: {error}"),
            );
            None
        }
    }
}
