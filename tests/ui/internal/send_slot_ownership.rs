//! Compiled against the actual slot module and the queue's real completion.
#![allow(dead_code)]
use super::*;

fn claim_once(key: SlotKey) {
    let _ = key.claim(Default::default());
}

#[cfg(ndi_test_send_ownership_types)]
fn claim_twice(key: SlotKey) {
    let _ = key.claim(Default::default());
    let _ = key.claim(Default::default());
}

#[cfg(ndi_test_send_ownership_types)]
fn clone_key(key: SlotKey) {
    let _ = key.clone();
}

#[cfg(ndi_test_send_ownership_types)]
fn foreign_storage(slots: &Arc<DescriptorSlots>, key: SlotKey) {
    let _ = slots.claim(key, Default::default());
}

#[cfg(ndi_test_send_ownership)]
fn forge_completion() {
    let _ = super::super::queued::Completion { _private: () };
}
