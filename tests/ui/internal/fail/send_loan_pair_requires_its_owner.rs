fn forge<'a, M: sender::SenderMode, L>(sender: &'a sender::Sender<M>, loans: L) {
    let _pair = sender::in_flight::LoanPair { sender, loans };
}
fn main() {}
