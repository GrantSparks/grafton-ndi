fn foreign<M: sender::SenderMode, L>(pair: &mut sender::in_flight::LoanPair<'_, M, L>) {
    // A callback must not receive separable loans and a raw sender handle.
    pair.with(|_, _| ());
}
fn main() {}
