fn claim_once(key: send_slots::SlotKey) {
    let _first = key.claim();
    let _second = key.claim();
}
fn main() {}
