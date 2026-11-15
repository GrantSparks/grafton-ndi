fn claim_once(key: send_slots::SlotKey) {
    let _first = key.claim(Default::default());
    let _second = key.claim(Default::default());
}
fn main() {}
