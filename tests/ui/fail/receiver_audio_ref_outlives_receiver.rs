#![allow(unused)]

use std::time::Duration;

use grafton_ndi::{Ndi, Receiver, ReceiverOptions, Source};

fn receiver(ndi: &Ndi) -> Receiver {
    let source = Source::new("compile-contract");
    let options = ReceiverOptions::new(source);
    Receiver::new(ndi, &options).unwrap()
}

fn main() {
    let ndi = Ndi::new().unwrap();

    let frame_ref = {
        let receiver = receiver(&ndi);
        receiver
            .audio()
            .capture_ref(Duration::from_millis(1))
            .unwrap()
            .unwrap()
    };

    let _ = frame_ref.num_channels();
}
