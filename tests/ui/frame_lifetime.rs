// This test demonstrates that frames cannot outlive their receivers
// It should fail to compile

use grafton_ndi::{NDI, Receiver, RecvBandwidth, RecvColorFormat, Source, SourceAddress};

fn main() {
    let ndi = NDI::new().unwrap();
    
    // Create a frame that outlives its receiver - this should fail to compile
    let frame = {
        let recv = grafton_ndi::Recv::new(
            &ndi,
            Receiver {
                source_to_connect_to: Source {
                    name: "Test".to_string(),
                    address: SourceAddress::None,
                },
                color_format: RecvColorFormat::BGRX_BGRA,
                bandwidth: RecvBandwidth::Highest,
                allow_video_fields: true,
                ndi_recv_name: None,
            },
        )
        .unwrap();
        
        // Capture a frame
        recv.capture_video(1000).unwrap()
        // recv is dropped here
    };
    
    // This should fail because frame cannot outlive recv
    println!("{:?}", frame);
}