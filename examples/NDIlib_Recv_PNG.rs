use grafton_ndi::{
    NDIlib, NDIlibFindInstance, NDIlibFrame, NDIlibRecvInstance,
    NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
    NDIlib_recv_color_format_e_NDIlib_recv_color_format_RGBX_RGBA,
};
use lodepng::encode32_file;
use std::path::Path;
use std::time::{Duration, Instant};

fn main() {
    // Initialize the NDI library
    if !NDIlib::initialize() {
        eprintln!("Failed to initialize NDI library.");
        return;
    }
    println!("NDI library initialized successfully.");

    // Generate the IP addresses within the additional range to include in our search
    let extra_ips: Vec<String> = (107..=111).map(|i| format!("192.168.0.{}", i)).collect();
    let extra_ips_cstr = extra_ips.join(",");

    println!("Creating NDI find instance...");
    let finder = NDIlibFindInstance::new(false, None, Some(&extra_ips_cstr));
    if !finder.is_initialized() {
        eprintln!("Failed to create NDI find instance.");
        NDIlib::destroy();
        return;
    }
    println!("NDI find instance created successfully.");

    let start = Instant::now();
    let target_source_name = "CAMERA4";
    let mut target_source = None;

    // Wait until there is a source named target_source_name
    while start.elapsed() < Duration::from_secs(60) {
        println!("Waiting for sources (timeout 5000 ms)...");
        if !finder.wait_for_sources(5000) {
            println!("No change to the sources found.");
            continue;
        }
        println!("Sources have changed.");

        let sources = finder.get_sources(5000);
        println!("Network sources ({} found).", sources.len());

        for (i, source) in sources.clone().iter().enumerate() {
            println!("{}. {}", i + 1, source.name);
            let name_parts: Vec<&str> = source.name.split_whitespace().collect();
            if name_parts[0] == target_source_name {
                target_source = Some(source.clone());
                break;
            }
        }

        if target_source.is_some() {
            break;
        }
    }

    if let Some(cloned_source) = target_source {
        println!("Found target source: {}", cloned_source.name);

        // Create a receiver for the target source
        let recv = NDIlibRecvInstance::new(
            &cloned_source,
            NDIlib_recv_color_format_e_NDIlib_recv_color_format_RGBX_RGBA,
            NDIlib_recv_bandwidth_e_NDIlib_recv_bandwidth_highest,
            false,
        );
        if !recv.is_initialized() {
            eprintln!("Failed to create NDI receiver.");
            NDIlib::destroy();
            return;
        }

        println!("NDI receiver created successfully.");

        // Delay for 100ms for the receiver to be ready
        std::thread::sleep(Duration::from_millis(1000));

        let mut frame_captured = false;
        let max_attempts = 5;
        for attempt in 1..=max_attempts {
            println!(
                "Attempting to capture video frame, attempt {}/{}...",
                attempt, max_attempts
            );
            let capture_result = recv.capture(60000);
            match capture_result {
                Some(NDIlibFrame::Video(video_frame)) => {
                    println!("Video frame captured.");

                    // Check stride and encode the image
                    assert_eq!(
                        unsafe { video_frame.__bindgen_anon_1.line_stride_in_bytes },
                        video_frame.xres * 4
                    );

                    // Use an absolute path for the image file
                    let image_path = Path::new("./CoolNDIImage.png");
                    println!("Saving image to {:?}", image_path);

                    if let Err(e) = encode32_file(
                        image_path,
                        unsafe {
                            std::slice::from_raw_parts(
                                video_frame.p_data as *const u8,
                                (video_frame.yres
                                    * video_frame.__bindgen_anon_1.line_stride_in_bytes)
                                    as usize,
                            )
                        },
                        video_frame.xres as usize,
                        video_frame.yres as usize,
                    ) {
                        eprintln!("Failed to save image: {}", e);
                    } else {
                        println!("Image saved successfully.");
                        frame_captured = true;
                    }

                    // Free the data
                    recv.free_video(&video_frame);

                    if frame_captured {
                        break;
                    }
                }
                _ => {
                    eprintln!("Failed to capture video frame.");
                }
            }
        }

        if !frame_captured {
            eprintln!(
                "Failed to capture video frame after {} attempts.",
                max_attempts
            );
        }

        // Destroy the receiver
        drop(recv);
    } else {
        eprintln!("Target source not found.");
    }

    // Destroy the NDI library
    println!("Destroying NDI library...");
    NDIlib::destroy();
    println!("NDI library destroyed. Program finished.");
}
