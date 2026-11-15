fn claim_once(slots: &Arc<send_slots::DescriptorSlots>, key: send_slots::SlotKey) {
    let _first = slots.claim(key);
    let _second = slots.claim(key);
}
fn main() {}
