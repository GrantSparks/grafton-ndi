use grafton_ndi::{FrameSync, Ndi, Receiver, ReceiverOptions, ScanType, Source};

fn receiver(ndi: &Ndi) -> Receiver {
    let source = Source::new("compile-contract");
    let options = ReceiverOptions::new(source);
    Receiver::new(ndi, &options).unwrap()
}

fn main() {
    let ndi = Ndi::new().unwrap();

    let frame_ref = {
        let framesync = FrameSync::new(receiver(&ndi)).unwrap();
        framesync
            .capture_video_ref(ScanType::Progressive)
            .unwrap()
            .unwrap()
    };

    let _ = frame_ref.width();
}
