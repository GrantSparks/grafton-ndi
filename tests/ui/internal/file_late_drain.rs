//! Compile inside the actual ledger module so refusal proves the parameter type.
#![allow(dead_code)]
use super::*;

fn a_wait_without_expiry_evidence_keeps_waiting(ledger: &Ledger) -> Option<WaitOutcome> {
    ledger.settle(None)
}

#[cfg(ndi_test_file_late_drain_without_proof)]
fn a_boolean_cannot_authorize_a_timeout(ledger: &Ledger) -> Option<WaitOutcome> {
    ledger.settle(Some(&true))
}
