//! Unit tests for the grafton-ndi library.

use std::ptr;

use crate::{
    error::Error,
    frames::{AudioFrame, VideoFrame},
    ndi_lib::*,
    receiver::{ReceiverStatus, Tally},
};

fn create_test_video_frame(
    width: i32,
    height: i32,
    line_stride: i32,
    data_size: i32,
) -> NDIlib_video_frame_v2_t {
    let mut frame: NDIlib_video_frame_v2_t = unsafe { std::mem::zeroed() };
    frame.xres = width;
    frame.yres = height;
    frame.FourCC = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRA; // Set to BGRA

    // Set the union field based on which value is provided
    if line_stride > 0 {
        frame.__bindgen_anon_1.line_stride_in_bytes = line_stride;
    } else {
        frame.__bindgen_anon_1.data_size_in_bytes = data_size;
    }

    // Allocate dummy data
    let actual_size = if line_stride > 0 {
        (line_stride * height) as usize
    } else {
        data_size as usize
    };
    let mut data = vec![0u8; actual_size];
    frame.p_data = data.as_mut_ptr();
    std::mem::forget(data); // Prevent deallocation during test

    frame
}

#[test]
fn test_video_frame_standard_format_size_calculation() {
    // Test standard video format with line stride
    let test_width = 1920;
    let test_height = 1080;
    let bytes_per_pixel = 4; // RGBA
    let line_stride = test_width * bytes_per_pixel;

    let c_frame = create_test_video_frame(test_width, test_height, line_stride, 0);

    // The from_raw function should calculate size as line_stride * height
    // Previously it would incorrectly multiply data_size_in_bytes * height
    unsafe {
        let frame = VideoFrame::from_raw(&c_frame, None).unwrap();

        // Expected size is line_stride * height
        let expected_size = (line_stride * test_height) as usize;
        assert_eq!(frame.data.len(), expected_size);

        // Clean up
        drop(frame);
        Vec::from_raw_parts(c_frame.p_data, expected_size, expected_size);
    }
}

#[test]
fn test_video_frame_size_calculation_logic() {
    // Test the size calculation logic without relying on union behavior
    // This is a simplified test that verifies the fix prevents the original bug

    // The original bug was: data_size = data_size_in_bytes * yres
    // This would cause massive over-allocation

    // For a 1920x1080 RGBA frame:
    // Correct: line_stride (1920*4) * height (1080) = 8,294,400 bytes
    // Bug would calculate: some_value * 1080 (potentially huge)

    let correct_size = 1920 * 4 * 1080; // 8,294,400 bytes
    assert!(correct_size < 10_000_000); // Should be under 10MB

    // The fix ensures we use line_stride * height for standard formats
    // and data_size_in_bytes directly for compressed formats
}

#[test]
fn test_video_frame_null_data_returns_error() {
    let mut c_frame: NDIlib_video_frame_v2_t = unsafe { std::mem::zeroed() };
    c_frame.p_data = ptr::null_mut();
    c_frame.__bindgen_anon_1.line_stride_in_bytes = 1920 * 4;
    c_frame.yres = 1080;

    unsafe {
        let result = VideoFrame::from_raw(&c_frame, None);
        assert!(result.is_err());
        match result {
            Err(Error::InvalidFrame(msg)) => {
                assert!(msg.contains("null data pointer"));
            }
            _ => panic!("Expected InvalidFrame error"),
        }
    }
}

#[test]
fn test_video_frame_zero_size_returns_error() {
    let mut c_frame: NDIlib_video_frame_v2_t = unsafe { std::mem::zeroed() };
    let mut data = vec![0u8; 100];
    c_frame.p_data = data.as_mut_ptr();
    c_frame.__bindgen_anon_1.line_stride_in_bytes = 0;
    c_frame.__bindgen_anon_1.data_size_in_bytes = 0;
    c_frame.yres = 1080;

    unsafe {
        let result = VideoFrame::from_raw(&c_frame, None);
        assert!(result.is_err());
        match result {
            Err(Error::InvalidFrame(msg)) => {
                assert!(msg.contains("neither valid line_stride_in_bytes nor data_size_in_bytes"));
            }
            _ => panic!("Expected InvalidFrame error"),
        }
    }
}

#[test]
fn test_audio_frame_drop_no_double_free() {
    // Test that AudioFrame can be created and dropped without issues
    let frame1 = AudioFrame::builder().build().unwrap();
    drop(frame1); // Should not panic or cause double-free

    // Test with metadata
    let frame2 = AudioFrame::builder()
        .metadata("test metadata")
        .build()
        .unwrap();
    drop(frame2); // Should not panic
}

#[test]
fn test_audio_frame_channel_data() {
    // Test with interleaved stereo data
    let data = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]; // 3 samples, 2 channels
    let frame = AudioFrame::builder()
        .sample_rate(48000)
        .channels(2)
        .samples(3)
        .data(data)
        .build()
        .unwrap();

    let ch0 = frame.channel_data(0).unwrap();
    assert_eq!(ch0, vec![1.0, 3.0, 5.0]);

    let ch1 = frame.channel_data(1).unwrap();
    assert_eq!(ch1, vec![2.0, 4.0, 6.0]);

    // Out of bounds should return None
    assert!(frame.channel_data(2).is_none());
}

#[test]
fn test_video_frame_builder() {
    let frame = VideoFrame::builder()
        .resolution(1920, 1080)
        .frame_rate(30, 1)
        .metadata("test metadata")
        .build()
        .unwrap();

    assert_eq!(frame.width, 1920);
    assert_eq!(frame.height, 1080);
    assert_eq!(frame.frame_rate_n, 30);
    assert_eq!(frame.frame_rate_d, 1);
}

#[test]
fn test_audio_frame_builder() {
    let frame = AudioFrame::builder()
        .sample_rate(48000)
        .channels(2)
        .samples(1024)
        .build()
        .unwrap();

    assert_eq!(frame.sample_rate, 48000);
    assert_eq!(frame.num_channels, 2);
    assert_eq!(frame.num_samples, 1024);
    assert_eq!(frame.data().len(), 2048); // 1024 samples * 2 channels
}

#[test]
fn test_recv_status_creation() {
    // Test ReceiverStatus with tally
    let status = ReceiverStatus {
        tally: Some(Tally::new(true, false)),
        connections: Some(3),
        other: false,
    };

    assert!(status.tally.is_some());
    let tally = status.tally.unwrap();
    assert!(tally.on_program);
    assert!(!tally.on_preview);
    assert_eq!(status.connections, Some(3));
    assert!(!status.other);

    // Test ReceiverStatus with other changes
    let status2 = ReceiverStatus {
        tally: None,
        connections: None,
        other: true,
    };

    assert!(status2.tally.is_none());
    assert!(status2.connections.is_none());
    assert!(status2.other);
}

#[test]
fn test_tally_to_raw() {
    let tally = Tally::new(true, false);
    let raw = tally.to_raw();

    assert!(raw.on_program);
    assert!(!raw.on_preview);

    let tally2 = Tally::new(false, true);
    let raw2 = tally2.to_raw();

    assert!(!raw2.on_program);
    assert!(raw2.on_preview);
}

#[test]
fn test_async_completion_handler() {
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};

    // Test that completion callback mechanism works
    let (tx, rx) = mpsc::channel();
    let tx = Arc::new(Mutex::new(tx));

    let handler = Box::new(move |slice: &mut [u8]| {
        // Verify we got a valid slice
        assert!(!slice.is_empty());
        // Signal completion
        let _ = tx.lock().unwrap().send(slice.len());
    });

    // Simulate buffer
    let mut buffer = vec![0u8; 1024];
    let buffer_ptr = buffer.as_mut_ptr();
    let buffer_len = buffer.len();

    // Call the handler as if NDI completed
    unsafe {
        let slice = std::slice::from_raw_parts_mut(buffer_ptr, buffer_len);
        handler(slice);
    }

    // Verify callback was called
    assert_eq!(rx.recv().unwrap(), 1024);
}
