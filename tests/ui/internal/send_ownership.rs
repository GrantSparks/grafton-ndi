//! Compiled inside sender.rs, outside the native loan owner module.
#![allow(dead_code)]
use super::*;
use in_flight::{InFlight, LoanPair, Stream};

fn retained_storage(slot: &mut InFlight<Sendable<VideoFrame>>) {
    let _ = slot.is_in_flight();
    let _ = slot.take_retained();
}

#[cfg(ndi_test_send_ownership)]
fn construct_foreign_stream(slot: &mut InFlight<Sendable<VideoFrame>>, sender: SendHandle<'_>) {
    let _ = Stream { slot, sender };
}

#[cfg(ndi_test_send_ownership_types)]
fn synchronize_foreign_slot(slot: &mut InFlight<Sendable<VideoFrame>>, sender: SendHandle<'_>) {
    slot.flush(sender);
}

#[cfg(ndi_test_send_ownership)]
fn construct_foreign_pair<M: SenderMode, L>(sender: &Sender<M>, loans: L) {
    let _ = LoanPair { sender, loans };
}

#[cfg(ndi_test_send_ownership_types)]
fn extract_foreign_dispatch<M: SenderMode, L>(pair: &LoanPair<'_, M, L>) {
    let _ = pair.handle();
}

#[cfg(ndi_test_send_ownership)]
fn forge_destruction() {
    let _ = in_flight::Destroyed { _private: () };
}

#[cfg(ndi_test_send_ownership_types)]
fn extract_sender_instance(resource: &SenderResource) {
    let _ = resource.instance;
}
