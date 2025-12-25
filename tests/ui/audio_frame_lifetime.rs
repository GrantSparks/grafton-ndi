// This test demonstrates that audio frames cannot outlive their receivers
// It should fail to compile with a lifetime error

use std::time::Duration;

use grafton_ndi::{Receiver, ReceiverOptions, Source, SourceAddress, NDI};

fn main() {
    let ndi = NDI::new().unwrap();
    let source = Source {
        name: "Test".to_string(),
        address: SourceAddress::None,
    };
    let options = ReceiverOptions::builder(source).build();

    // Create an audio frame that outlives its receiver - this should fail to compile
    let frame_ref = {
        let receiver = Receiver::new(&ndi, &options).unwrap();

        // Capture a borrowed audio frame
        // This should work fine within the scope
        match receiver.capture_audio_ref(Duration::from_millis(100)) {
            Ok(Some(f)) => f,
            _ => panic!("Test expects a frame"),
        }
        // receiver is dropped here
    };

    // This should fail to compile because frame_ref cannot outlive receiver
    // Error: `receiver` does not live long enough
    println!("{:?}", frame_ref.num_channels());
}
