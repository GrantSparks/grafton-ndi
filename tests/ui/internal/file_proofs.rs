//! Compile outside both minting modules, beside the recorder facade.
#![allow(dead_code)]
use super::*;

#[cfg(ndi_test_file_proofs)]
fn forge_the_native_stop() -> ledger::StopOwed {
    ledger::StopOwed { _private: () }
}

#[cfg(ndi_test_file_proofs)]
fn forge_the_late_drain() -> settle::LateDrain {
    settle::LateDrain { _private: () }
}

#[cfg(ndi_test_file_proofs_discarded)]
fn discard_the_stop(ledger: &mut Ledger) {
    ledger.stop_requested(true);
}
