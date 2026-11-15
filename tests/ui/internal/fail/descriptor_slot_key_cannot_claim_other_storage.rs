fn foreign_storage(slots: &Arc<send_slots::DescriptorSlots>, key: send_slots::SlotKey) {
    let _loan = slots.claim(key);
}
fn main() {}
