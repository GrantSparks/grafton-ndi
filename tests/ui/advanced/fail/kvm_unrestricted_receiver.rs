use grafton_ndi::{advanced::KvmSession, Receiver, ReceiverMode};

fn recover<M: ReceiverMode>(session: &KvmSession<'_, M>) {
    let _: &Receiver<M> = session.receiver();
    let _: &Receiver<M> = &session.receiver();
    let _: &Receiver<M> = &*session.receiver();
    let _: &Receiver<M> = session.receiver().into();
    let _ = session.receiver().receiver();
    let _ = session.receiver().as_ref();
}

fn main() {}
