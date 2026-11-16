//! Classify a test's capability refusal through the shared skip policy.

use super::{
    api::{CapabilityUnavailable, Error, Result},
    policy::{skip, Requirement},
    unsupported,
};

/// Whether a capability check succeeded, visibly reporting why it did not.
///
/// Missing Advanced declarations or the wrong runtime edition are prerequisites;
/// other refusals describe limitations of the selected runtime. Unexpected
/// errors fail the test rather than hiding a defect as a skip.
pub fn require_feature(result: Result<()>) -> bool {
    match result {
        Ok(()) => true,
        Err(error @ Error::UnsupportedCapability { refusal, .. }) => {
            match refusal.reason() {
                CapabilityUnavailable::BuildAbiUnavailable
                | CapabilityUnavailable::WrongEdition => {
                    skip(Requirement::Advanced, &error.to_string());
                }
                _ => unsupported(&error.to_string()),
            }
            false
        }
        Err(error) => panic!("capability check failed unexpectedly: {error}"),
    }
}
