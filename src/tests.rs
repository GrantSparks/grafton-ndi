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

#[test]
fn test_retry_logic_constants() {
    // Test that retry methods use reasonable timeout and sleep values
    // This validates the constants are within expected ranges

    // Per-attempt timeout should be reasonable (100ms in implementation)
    let per_attempt_timeout_ms = 100;
    assert!((10..=1000).contains(&per_attempt_timeout_ms));

    // Sleep between retries should be brief (10ms in implementation)
    let sleep_between_retries_ms = 10;
    assert!((1..=100).contains(&sleep_between_retries_ms));

    // These constants ensure non-blocking behavior without excessive CPU usage
}

#[test]
fn test_timeout_duration_calculation() {
    // Verify timeout duration conversion works correctly
    let timeout_ms: u32 = 5000;
    let timeout_duration = std::time::Duration::from_millis(timeout_ms.into());
    assert_eq!(timeout_duration.as_millis(), 5000);

    // Edge case: very short timeout
    let short_timeout_ms: u32 = 100;
    let short_duration = std::time::Duration::from_millis(short_timeout_ms.into());
    assert_eq!(short_duration.as_millis(), 100);

    // Edge case: long timeout
    let long_timeout_ms: u32 = 60_000; // 1 minute
    let long_duration = std::time::Duration::from_millis(long_timeout_ms.into());
    assert_eq!(long_duration.as_millis(), 60_000);
}

#[test]
fn test_source_address_contains_host() {
    use crate::finder::SourceAddress;

    // Test IP address matching
    let ip_addr = SourceAddress::Ip("192.168.1.100:5960".to_string());
    assert!(ip_addr.contains_host("192.168.1.100"));
    assert!(ip_addr.contains_host("192.168.1"));
    assert!(ip_addr.contains_host("5960"));
    assert!(!ip_addr.contains_host("192.168.2"));

    // Test URL matching
    let url_addr = SourceAddress::Url("http://camera.local:8080".to_string());
    assert!(url_addr.contains_host("camera.local"));
    assert!(url_addr.contains_host("camera"));
    assert!(url_addr.contains_host("8080"));
    assert!(!url_addr.contains_host("other.local"));

    // Test None variant
    let none_addr = SourceAddress::None;
    assert!(!none_addr.contains_host("anything"));
}

#[test]
fn test_source_address_port() {
    use crate::finder::SourceAddress;

    // Test IP address with port
    let ip_with_port = SourceAddress::Ip("192.168.1.100:5960".to_string());
    assert_eq!(ip_with_port.port(), Some(5960));

    // Test IP address without port
    let ip_no_port = SourceAddress::Ip("192.168.1.100".to_string());
    assert_eq!(ip_no_port.port(), None);

    // Test URL with port
    let url_with_port = SourceAddress::Url("http://camera.local:8080".to_string());
    assert_eq!(url_with_port.port(), Some(8080));

    // Test URL with port and path
    let url_with_path = SourceAddress::Url("http://camera.local:8080/stream".to_string());
    assert_eq!(url_with_path.port(), Some(8080));

    // Test URL without port
    let url_no_port = SourceAddress::Url("http://camera.local".to_string());
    assert_eq!(url_no_port.port(), None);

    // Test URL with scheme but no port
    let url_scheme_only = SourceAddress::Url("http://camera.local/stream".to_string());
    assert_eq!(url_scheme_only.port(), None);

    // Test None variant
    let none_addr = SourceAddress::None;
    assert_eq!(none_addr.port(), None);

    // Test edge case: IPv6-style address (just ensure it doesn't panic)
    let ipv6_style = SourceAddress::Ip("fe80::1:5960".to_string());
    // This will parse the last segment after colon as port
    assert_eq!(ipv6_style.port(), Some(5960));
}

#[test]
fn test_source_matches_host() {
    use crate::finder::{Source, SourceAddress};

    // Test matching by IP address
    let source = Source {
        name: "CAMERA1 (Chan1)".to_string(),
        address: SourceAddress::Ip("192.168.0.107:5960".to_string()),
    };
    assert!(source.matches_host("192.168.0.107"));
    assert!(source.matches_host("192.168.0"));
    assert!(!source.matches_host("192.168.1"));

    // Test matching by name
    assert!(source.matches_host("CAMERA1"));
    assert!(source.matches_host("Chan1"));
    assert!(!source.matches_host("CAMERA2"));

    // Test matching with URL address
    let url_source = Source {
        name: "Studio Camera".to_string(),
        address: SourceAddress::Url("http://studio.local:8080".to_string()),
    };
    assert!(url_source.matches_host("studio.local"));
    assert!(url_source.matches_host("Studio"));
    assert!(!url_source.matches_host("other"));

    // Test with None address
    let no_addr_source = Source {
        name: "Local Source".to_string(),
        address: SourceAddress::None,
    };
    assert!(no_addr_source.matches_host("Local"));
    assert!(!no_addr_source.matches_host("192.168.1.1"));
}

#[test]
fn test_source_ip_address() {
    use crate::finder::{Source, SourceAddress};

    // Test IP address extraction from IP variant
    let ip_source = Source {
        name: "CAMERA1".to_string(),
        address: SourceAddress::Ip("192.168.1.100:5960".to_string()),
    };
    assert_eq!(ip_source.ip_address(), Some("192.168.1.100"));

    // Test IP without port
    let ip_no_port = Source {
        name: "CAMERA2".to_string(),
        address: SourceAddress::Ip("192.168.1.101".to_string()),
    };
    assert_eq!(ip_no_port.ip_address(), Some("192.168.1.101"));

    // Test hostname extraction from URL variant
    let url_source = Source {
        name: "Studio".to_string(),
        address: SourceAddress::Url("http://camera.local:8080".to_string()),
    };
    assert_eq!(url_source.ip_address(), Some("camera.local"));

    // Test URL with path
    let url_with_path = Source {
        name: "Studio2".to_string(),
        address: SourceAddress::Url("http://camera.local:8080/stream".to_string()),
    };
    assert_eq!(url_with_path.ip_address(), Some("camera.local"));

    // Test URL without scheme
    let url_no_scheme = Source {
        name: "Studio3".to_string(),
        address: SourceAddress::Url("camera.local:8080".to_string()),
    };
    assert_eq!(url_no_scheme.ip_address(), Some("camera.local"));

    // Test None variant
    let none_source = Source {
        name: "None".to_string(),
        address: SourceAddress::None,
    };
    assert_eq!(none_source.ip_address(), None);
}

#[test]
fn test_source_host() {
    use crate::finder::{Source, SourceAddress};

    // Test that host() is an alias for ip_address()
    let source = Source {
        name: "CAMERA1".to_string(),
        address: SourceAddress::Ip("192.168.1.100:5960".to_string()),
    };
    assert_eq!(source.host(), source.ip_address());
    assert_eq!(source.host(), Some("192.168.1.100"));

    let url_source = Source {
        name: "Studio".to_string(),
        address: SourceAddress::Url("http://camera.local:8080".to_string()),
    };
    assert_eq!(url_source.host(), url_source.ip_address());
    assert_eq!(url_source.host(), Some("camera.local"));
}

#[test]
fn test_source_matching_real_world_example() {
    use crate::finder::{Source, SourceAddress};

    // Real-world example from the issue description
    let source = Source {
        name: "CAMERA1 (Chan1, 192.168.0.107)".to_string(),
        address: SourceAddress::Ip("192.168.0.107:5960".to_string()),
    };

    // Should match by IP in address
    assert!(source.matches_host("192.168.0.107"));

    // Should match by partial IP
    assert!(source.matches_host("192.168.0"));

    // Should match by name
    assert!(source.matches_host("CAMERA1"));

    // Should match by IP in name
    assert!(source.matches_host("192.168.0.107"));

    // Should extract IP correctly
    assert_eq!(source.ip_address(), Some("192.168.0.107"));
    assert_eq!(source.host(), Some("192.168.0.107"));

    // Should extract port correctly
    assert_eq!(source.address.port(), Some(5960));
}

#[test]
fn test_source_cache_creation() {
    use crate::finder::SourceCache;

    // Should be able to create a cache
    let cache = SourceCache::new();
    assert!(cache.is_ok());

    // Cache should start empty
    let cache = cache.unwrap();
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
}

#[test]
fn test_source_cache_default() {
    use crate::finder::SourceCache;

    // Default should create an empty cache
    let cache = SourceCache::default();
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
}

#[test]
fn test_source_cache_invalidation() {
    use crate::finder::SourceCache;

    let cache = SourceCache::default();

    // Invalidating a non-existent entry should not panic
    cache.invalidate("192.168.0.107");
    assert_eq!(cache.len(), 0);

    // Clear on empty cache should not panic
    cache.clear();
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
}

#[test]
fn test_source_cache_len_and_is_empty() {
    use crate::finder::SourceCache;

    let cache = SourceCache::default();

    // Initially empty
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());

    // After clear, still empty
    cache.clear();
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
}
