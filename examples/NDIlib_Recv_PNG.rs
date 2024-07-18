use std::fs::File;

use grafton_ndi::{
    Find, Finder, FrameType, Receiver, Recv, RecvBandwidth, RecvColorFormat, VideoFrame, NDI,
};

fn main() -> Result<(), &'static str> {
    // Initialize the NDI library and ensure it's properly cleaned up
    if let Ok(_ndi) = NDI::new() {
        // Create an NDI finder to locate sources on the network
        // let finder = Finder::default();
        let finder = Finder::new(false, None, Some("192.168.0.110"));
        let ndi_find = Find::new(finder)?;

        // Wait until we find a source named "CAMERA4"
        let source_name = "CAMERA4";
        let mut found_source = None;

        while found_source.is_none() {
            // Wait until the sources on the network have changed
            println!("Looking for sources ...");
            ndi_find.wait_for_sources(5000);
            let sources = ndi_find.get_sources(5000);

            for source in &sources {
                if source.name.contains(source_name) {
                    found_source = Some(source.clone());
                    break;
                }
            }
        }

        let source =
            found_source.unwrap_or_else(|| panic!("Failed to find source {}", source_name));

        println!("Found source: {:?}", source);

        // We now have the desired source, so we create a receiver to look at it.
        let receiver = Receiver::new(
            source,
            RecvColorFormat::RGBX_RGBA,
            RecvBandwidth::Highest,
            false,
            None,
        );
        let ndi_recv = Recv::new(receiver)?;

        // Wait until we have a video frame
        let mut video_frame: Option<VideoFrame> = None;
        while video_frame.is_none() {
            // Sleep for 3 seconds
            std::thread::sleep(std::time::Duration::from_secs(5));

            println!("Waiting for video frame ...");
            match ndi_recv.capture(60000) {
                Ok(FrameType::Video(frame)) => {
                    // Ensure that the stride matches the width
                    if unsafe { frame.line_stride_or_size.line_stride_in_bytes } == frame.xres * 4 {
                        video_frame = Some(frame);
                    } else {
                        println!(
                            "Stride does not match width, skipping frame with resolution: {}x{}",
                            frame.xres, frame.yres
                        );
                        ndi_recv.free_video(&frame);
                    }
                }
                _ => println!("Failed to capture a video frame or no video frame available."),
            }
        }

        if let Some(frame) = video_frame {
            // Save the frame as a PNG file
            if let Err(e) = save_frame_as_png(&frame) {
                eprintln!("Failed to save frame as PNG: {}", e);
            }

            // Free the data
            ndi_recv.free_video(&frame);
        }

        // The NDI receiver will be destroyed automatically when it goes out of scope
        // The NDI library will be destroyed automatically when `_ndi` goes out of scope
    } else {
        return Err("Failed to initialize NDI library");
    }

    Ok(())
}

fn save_frame_as_png(video_frame: &VideoFrame) -> Result<(), &'static str> {
    let path = "CoolNDIImage.png";
    let file = File::create(path).map_err(|_| "Failed to create file")?;
    let mut encoder = png::Encoder::new(file, video_frame.xres as u32, video_frame.yres as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);

    // Debugging info
    println!(
        "Saving frame with resolution: {}x{}, line stride: {}",
        video_frame.xres,
        video_frame.yres,
        unsafe { video_frame.line_stride_or_size.line_stride_in_bytes }
    );

    // Ensure the p_data pointer is valid
    if video_frame.p_data.is_null() {
        return Err("Frame data pointer is null");
    }

    let mut writer = encoder
        .write_header()
        .map_err(|_| "Failed to write PNG header")?;
    writer
        .write_image_data(unsafe {
            let data_len =
                (video_frame.yres * video_frame.line_stride_or_size.line_stride_in_bytes) as usize;
            println!("Data length: {}", data_len);
            std::slice::from_raw_parts(video_frame.p_data, data_len)
        })
        .map_err(|_| "Failed to write PNG data")?;
    Ok(())
}
