use grafton_ndi::{advanced::KvmSession, ReceiverMode, Source};

fn retarget<M: ReceiverMode>(session: &mut KvmSession<'_, M>, next: &Source) {
    let receiver = session.receiver();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let _ = receiver.connect_to(next);
            receiver.disconnect();
            receiver.reconnect()
        });
        session.key_down(0xffe1).unwrap();
    });
}

fn main() {}
