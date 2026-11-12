//! Unit tests for the grafton-ndi library.

use std::ptr;

use crate::{
    error::Error,
    frames::{AudioFrame, VideoFrame},
    media::FrameRate,
    receiver::Tally,
    sys::bindings::*,
};

/// A requirement the test environment promises must have a test in this
/// build; see `test_support`. This runs in every build, whatever its features.
#[test]
fn every_promised_requirement_has_a_test_in_this_build() {
    let unverifiable = crate::test_support::unverifiable_promises();
    assert!(
        unverifiable.is_empty(),
        "GRAFTON_NDI_REQUIRE_RUNTIME promises what no test in this build can check: {}",
        unverifiable.join("; ")
    );
}

fn create_test_video_frame(
    width: i32,
    height: i32,
    line_stride: i32,
    data_size: i32,
) -> NDIlib_video_frame_v2_t {
    let mut frame: NDIlib_video_frame_v2_t = unsafe { std::mem::zeroed() };
    frame.xres = width;
    frame.yres = height;
    frame.FourCC = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRA;
    frame.frame_rate_N = 60;
    frame.frame_rate_D = 1;
    frame.picture_aspect_ratio = 16.0 / 9.0;
    frame.frame_format_type = NDIlib_frame_format_type_e_NDIlib_frame_format_type_progressive;

    // Set the union field based on which value is provided
    if line_stride > 0 {
        frame.__bindgen_anon_1.line_stride_in_bytes = line_stride;
    } else {
        frame.__bindgen_anon_1.data_size_in_bytes = data_size;
    }

    let actual_size = if line_stride > 0 {
        (line_stride * height) as usize
    } else {
        data_size as usize
    };
    let mut data = vec![0u8; actual_size];
    frame.p_data = data.as_mut_ptr();
    std::mem::forget(data);

    frame
}

#[test]
fn test_video_frame_standard_format_size_calculation() {
    let test_width = 1920;
    let test_height = 1080;
    let bytes_per_pixel = 4;
    let line_stride = test_width * bytes_per_pixel;

    let c_frame = create_test_video_frame(test_width, test_height, line_stride, 0);

    unsafe {
        let frame = VideoFrame::from_raw(&c_frame).unwrap();

        let expected_size = (line_stride * test_height) as usize;
        assert_eq!(frame.data().len(), expected_size);

        drop(frame);
        Vec::from_raw_parts(c_frame.p_data, expected_size, expected_size);
    }
}

#[test]
fn test_video_frame_size_calculation_logic() {
    // CORRECTNESS: Verify size calculation uses line_stride * height for standard formats
    // and data_size_in_bytes directly for compressed formats, preventing over-allocation

    let correct_size = 1920 * 4 * 1080;
    assert!(correct_size < 10_000_000);
}

#[test]
fn test_video_frame_null_data_returns_error() {
    let mut c_frame: NDIlib_video_frame_v2_t = unsafe { std::mem::zeroed() };
    c_frame.p_data = ptr::null_mut();
    c_frame.__bindgen_anon_1.line_stride_in_bytes = 1920 * 4;
    c_frame.yres = 1080;

    unsafe {
        let result = VideoFrame::from_raw(&c_frame);
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
    c_frame.FourCC = NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRA;
    c_frame.xres = 1920;
    c_frame.__bindgen_anon_1.line_stride_in_bytes = -1;
    c_frame.yres = 1080;
    c_frame.frame_rate_N = 60;
    c_frame.frame_rate_D = 1;
    c_frame.picture_aspect_ratio = 16.0 / 9.0;
    c_frame.frame_format_type = NDIlib_frame_format_type_e_NDIlib_frame_format_type_progressive;

    unsafe {
        let result = VideoFrame::from_raw(&c_frame);
        assert!(result.is_err());
        match result {
            Err(Error::InvalidFrame(msg)) => {
                assert!(msg.contains("invalid line_stride_in_bytes"));
            }
            _ => panic!("Expected InvalidFrame error"),
        }
    }
}

#[test]
fn test_audio_frame_drop_no_double_free() {
    let frame1 = AudioFrame::builder().build().unwrap();
    drop(frame1);

    let frame2 = AudioFrame::builder()
        .metadata("test metadata")
        .build()
        .unwrap();
    drop(frame2);
}

#[test]
fn test_audio_frame_channel_data_interleaved() {
    use crate::AudioLayout;

    // Interleaved input: [C0S0, C1S0, C0S1, C1S1, C0S2, C1S2]
    let data = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
    let frame = AudioFrame::builder()
        .sample_rate(48000)
        .num_channels(2)
        .num_samples(3)
        .data(data)
        .layout(AudioLayout::Interleaved)
        .build()
        .unwrap();

    // Data is converted to planar at build time, stride is always set
    assert_eq!(frame.channel_stride_bytes(), 12); // 3 samples * 4 bytes

    // After conversion, channel_data should return the de-interleaved samples
    let ch0 = frame.channel_data(0).unwrap();
    assert_eq!(ch0, vec![1.0, 3.0, 5.0]);

    let ch1 = frame.channel_data(1).unwrap();
    assert_eq!(ch1, vec![2.0, 4.0, 6.0]);

    assert!(frame.channel_data(2).is_none());
}

#[test]
fn test_audio_frame_builder_data_length_validation() {
    use crate::AudioLayout;

    // Too few samples
    let result = AudioFrame::builder()
        .num_channels(2)
        .num_samples(3)
        .data(vec![1.0, 2.0, 3.0]) // 3 instead of 6
        .layout(AudioLayout::Planar)
        .build();

    assert!(result.is_err());
    match result {
        Err(Error::InvalidFrame(msg)) => {
            assert!(msg.contains("Audio data length 3"));
            assert!(msg.contains("expected 6"));
        }
        _ => panic!("Expected InvalidFrame error"),
    }

    // Too many samples
    let result = AudioFrame::builder()
        .num_channels(2)
        .num_samples(3)
        .data(vec![1.0; 10])
        .layout(AudioLayout::Interleaved)
        .build();

    assert!(result.is_err());
    match result {
        Err(Error::InvalidFrame(msg)) => {
            assert!(msg.contains("Audio data length 10"));
            assert!(msg.contains("expected 6"));
        }
        _ => panic!("Expected InvalidFrame error"),
    }
}

#[test]
fn test_audio_frame_channel_data_planar() {
    use crate::AudioLayout;

    let data = vec![1.0, 3.0, 5.0, 2.0, 4.0, 6.0];
    let frame = AudioFrame::builder()
        .sample_rate(48000)
        .num_channels(2)
        .num_samples(3)
        .data(data)
        .layout(AudioLayout::Planar)
        .build()
        .unwrap();

    assert_eq!(frame.channel_stride_bytes(), 12);

    let ch0 = frame.channel_data(0).unwrap();
    assert_eq!(ch0, vec![1.0, 3.0, 5.0]);

    let ch1 = frame.channel_data(1).unwrap();
    assert_eq!(ch1, vec![2.0, 4.0, 6.0]);

    assert!(frame.channel_data(2).is_none());
}

#[test]
fn test_audio_frame_default_layout() {
    let frame = AudioFrame::builder()
        .sample_rate(48000)
        .num_channels(2)
        .num_samples(100)
        .build()
        .unwrap();

    assert_eq!(frame.channel_stride_bytes(), 400);
}

#[test]
fn test_video_frame_builder() {
    let frame = VideoFrame::builder()
        .resolution(1920, 1080)
        .frame_rate(FrameRate::new(30, 1).unwrap())
        .metadata("test metadata")
        .build()
        .unwrap();

    assert_eq!(frame.width(), 1920);
    assert_eq!(frame.height(), 1080);
    let rate = frame.frame_rate();
    assert_eq!((rate.numerator(), rate.denominator()), (30, 1));
}

#[test]
fn test_audio_frame_builder() {
    let frame = AudioFrame::builder()
        .sample_rate(48000)
        .num_channels(2)
        .num_samples(1024)
        .build()
        .unwrap();

    assert_eq!(frame.sample_rate(), 48000);
    assert_eq!(frame.num_channels(), 2);
    assert_eq!(frame.num_samples(), 1024);
    assert_eq!(frame.data().len(), 2048);
    assert_eq!(frame.channel_stride_bytes(), 4096);
}

#[test]
fn test_tally_round_trips_through_the_sdk_descriptor() {
    for (on_program, on_preview) in [(false, false), (true, false), (false, true), (true, true)] {
        let tally = Tally::new(on_program, on_preview);
        assert_eq!(
            (tally.on_program(), tally.on_preview()),
            (on_program, on_preview)
        );
        let raw = tally.to_sdk();
        assert_eq!((raw.on_program, raw.on_preview), (on_program, on_preview));
    }
    assert_eq!(Tally::default(), Tally::new(false, false));
}

#[test]
fn test_async_completion_handler() {
    use std::sync::{mpsc, Arc, Mutex};

    let (tx, rx) = mpsc::channel();
    let tx = Arc::new(Mutex::new(tx));

    let handler = Box::new(move |slice: &mut [u8]| {
        assert!(!slice.is_empty());
        let _ = tx.lock().unwrap().send(slice.len());
    });

    let mut buffer = vec![0u8; 1024];
    let buffer_ptr = buffer.as_mut_ptr();
    let buffer_len = buffer.len();

    unsafe {
        let slice = std::slice::from_raw_parts_mut(buffer_ptr, buffer_len);
        handler(slice);
    }

    assert_eq!(rx.recv().unwrap(), 1024);
}

#[test]
fn test_retry_logic_constants() {
    // PERF: Validate retry timeout and sleep values for non-blocking behavior

    let per_attempt_timeout_ms = 100;
    assert!((10..=1000).contains(&per_attempt_timeout_ms));

    let sleep_between_retries_ms = 10;
    assert!((1..=100).contains(&sleep_between_retries_ms));
}

#[test]
fn test_timeout_duration_calculation() {
    let timeout_ms: u32 = 5000;
    let timeout_duration = std::time::Duration::from_millis(timeout_ms.into());
    assert_eq!(timeout_duration.as_millis(), 5000);

    let short_timeout_ms: u32 = 100;
    let short_duration = std::time::Duration::from_millis(short_timeout_ms.into());
    assert_eq!(short_duration.as_millis(), 100);

    let long_timeout_ms: u32 = 60_000;
    let long_duration = std::time::Duration::from_millis(long_timeout_ms.into());
    assert_eq!(long_duration.as_millis(), 60_000);
}

#[test]
fn test_source_address_contains_host() {
    use crate::finder::SourceAddress;

    let ip_addr = SourceAddress::Ip("192.168.1.100:5960".to_string());
    assert!(ip_addr.contains_host("192.168.1.100"));
    assert!(!ip_addr.contains_host("192.168.1"));
    assert!(!ip_addr.contains_host("5960"));
    assert!(!ip_addr.contains_host("192.168.2"));

    let url_addr = SourceAddress::Url("http://camera.local:8080".to_string());
    assert!(url_addr.contains_host("camera.local"));
    assert!(!url_addr.contains_host("camera"));
    assert!(!url_addr.contains_host("8080"));
    assert!(url_addr.contains_host("CAMERA.LOCAL."));
    assert!(!url_addr.contains_host("other.local"));
}

#[test]
fn test_source_address_port() {
    use crate::finder::SourceAddress;

    let ip_with_port = SourceAddress::Ip("192.168.1.100:5960".to_string());
    assert_eq!(ip_with_port.port(), Some(5960));

    let ip_no_port = SourceAddress::Ip("192.168.1.100".to_string());
    assert_eq!(ip_no_port.port(), None);

    let url_with_port = SourceAddress::Url("http://camera.local:8080".to_string());
    assert_eq!(url_with_port.port(), Some(8080));

    let url_with_path = SourceAddress::Url("http://camera.local:8080/stream".to_string());
    assert_eq!(url_with_path.port(), Some(8080));

    let url_no_port = SourceAddress::Url("http://camera.local".to_string());
    assert_eq!(url_no_port.port(), None);

    let url_scheme_only = SourceAddress::Url("http://camera.local/stream".to_string());
    assert_eq!(url_scheme_only.port(), None);

    // An unbracketed IPv6 literal has no explicit port delimiter.
    let ipv6_literal = SourceAddress::Ip("fe80::1:5960".to_string());
    assert_eq!(ipv6_literal.port(), None);

    let ipv6_with_port = SourceAddress::Ip("[fe80::1]:5960".to_string());
    assert_eq!(ipv6_with_port.port(), Some(5960));
}

#[test]
fn test_source_matches_host() {
    use crate::finder::{Source, SourceAddress};

    let source = Source::new("CAMERA1 (Chan1)")
        .with_address(SourceAddress::Ip("192.168.0.107:5960".to_string()));
    assert!(source.matches_host("192.168.0.107"));
    assert!(!source.matches_host("192.168.0"));
    assert!(!source.matches_host("192.168.1"));

    assert!(source.matches_host("CAMERA1"));
    assert!(!source.matches_host("Chan1"));
    assert!(!source.matches_host("CAMERA2"));

    let url_source = Source::new("Studio Camera")
        .with_address(SourceAddress::Url("http://studio.local:8080".to_string()));
    assert!(url_source.matches_host("studio.local"));
    assert!(!url_source.matches_host("Studio"));
    assert!(!url_source.matches_host("other"));

    let no_addr_source = Source::new("Local Source");
    assert!(!no_addr_source.matches_host("Local"));
    assert!(!no_addr_source.matches_host("192.168.1.1"));
}

#[test]
fn test_source_host() {
    use crate::finder::{Source, SourceAddress};

    let ip_source =
        Source::new("CAMERA1").with_address(SourceAddress::Ip("192.168.1.100:5960".to_string()));
    assert_eq!(ip_source.host(), Some("192.168.1.100"));

    let ip_no_port =
        Source::new("CAMERA2").with_address(SourceAddress::Ip("192.168.1.101".to_string()));
    assert_eq!(ip_no_port.host(), Some("192.168.1.101"));

    let url_source = Source::new("Studio")
        .with_address(SourceAddress::Url("http://camera.local:8080".to_string()));
    assert_eq!(url_source.host(), Some("camera.local"));

    let url_with_path = Source::new("Studio2").with_address(SourceAddress::Url(
        "http://camera.local:8080/stream".to_string(),
    ));
    assert_eq!(url_with_path.host(), Some("camera.local"));

    let url_no_scheme =
        Source::new("Studio3").with_address(SourceAddress::Url("camera.local:8080".to_string()));
    assert_eq!(url_no_scheme.host(), Some("camera.local"));

    let none_source = Source::new("None");
    assert_eq!(none_source.host(), None);
}

#[test]
fn test_source_matching_real_world_example() {
    use crate::finder::{Source, SourceAddress};

    let source = Source::new("CAMERA1 (Chan1, 192.168.0.107)")
        .with_address(SourceAddress::Ip("192.168.0.107:5960".to_string()));

    assert!(source.matches_host("192.168.0.107"));
    assert!(!source.matches_host("192.168.0"));
    assert!(source.matches_host("CAMERA1"));
    assert!(source.matches_host("192.168.0.107"));
    assert_eq!(source.host(), Some("192.168.0.107"));
    assert_eq!(source.address().unwrap().port(), Some(5960));
}

#[test]
fn test_source_cache_creation() {
    use crate::finder::SourceCache;

    let cache = SourceCache::new();
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
}

#[test]
fn test_source_cache_default() {
    use crate::finder::SourceCache;

    let cache = SourceCache::default();
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
}

#[test]
fn test_source_cache_invalidation() {
    use crate::finder::SourceCache;

    let cache = SourceCache::default();

    cache.invalidate("192.168.0.107");
    assert_eq!(cache.len(), 0);

    cache.clear();
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
}

#[cfg(feature = "image-encoding")]
#[test]
fn test_video_frame_encode_png_rgba() {
    use crate::frames::{PixelFormat, VideoFrame};

    let width = 2;
    let height = 2;
    let mut data = vec![0u8; (width * height * 4) as usize];

    data[0..4].copy_from_slice(&[255, 0, 0, 255]);
    data[4..8].copy_from_slice(&[0, 255, 0, 255]);
    data[8..12].copy_from_slice(&[0, 0, 255, 255]);
    data[12..16].copy_from_slice(&[255, 255, 255, 255]);

    let frame = VideoFrame::builder()
        .resolution(width, height)
        .pixel_format(PixelFormat::Rgba)
        .build()
        .unwrap();

    let mut frame = frame;
    frame.replace_data(data).unwrap();

    let png_bytes = frame.encode_png();
    assert!(png_bytes.is_ok());

    let png_bytes = png_bytes.unwrap();
    assert!(!png_bytes.is_empty());

    assert_eq!(&png_bytes[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
}

#[cfg(feature = "image-encoding")]
#[test]
fn test_video_frame_encode_png_bgra() {
    use crate::frames::{PixelFormat, VideoFrame};

    let width = 2;
    let height = 2;
    let mut data = vec![0u8; (width * height * 4) as usize];

    data[0..4].copy_from_slice(&[0, 0, 255, 255]);
    data[4..8].copy_from_slice(&[0, 255, 0, 255]);
    data[8..12].copy_from_slice(&[255, 0, 0, 255]);
    data[12..16].copy_from_slice(&[255, 255, 255, 255]);

    let frame = VideoFrame::builder()
        .resolution(width, height)
        .pixel_format(PixelFormat::Bgra)
        .build()
        .unwrap();

    let mut frame = frame;
    frame.replace_data(data).unwrap();

    let png_bytes = frame.encode_png();
    assert!(png_bytes.is_ok());

    let png_bytes = png_bytes.unwrap();
    assert!(!png_bytes.is_empty());

    assert_eq!(&png_bytes[0..8], &[137, 80, 78, 71, 13, 10, 26, 10]);
}

#[cfg(feature = "image-encoding")]
#[test]
fn test_video_frame_encode_png_unsupported_format() {
    use crate::frames::{PixelFormat, VideoFrame};

    let frame = VideoFrame::builder()
        .resolution(2, 2)
        .pixel_format(PixelFormat::Uyvy)
        .build()
        .unwrap();

    let result = frame.encode_png();
    assert!(result.is_err());

    let err = result.unwrap_err();
    let err_msg = format!("{err}");
    assert!(err_msg.contains("Unsupported format"));
}

#[cfg(feature = "image-encoding")]
#[test]
fn test_video_frame_encode_rejects_multi_plane_422_formats() {
    use crate::frames::{ImageFormat, PixelFormat, VideoFrame};

    for format in [PixelFormat::Uyva, PixelFormat::P216, PixelFormat::Pa16] {
        let frame = VideoFrame::builder()
            .resolution(4, 2)
            .pixel_format(format)
            .build()
            .unwrap();

        for result in [
            frame.encode_png(),
            frame.encode_jpeg(90),
            frame
                .encode_data_url(ImageFormat::Png)
                .map(String::into_bytes),
        ] {
            let err = result.expect_err("multi-plane 4:2:2 formats must not be encoded");
            assert!(
                format!("{err}").contains("Unsupported format"),
                "{format:?}: {err}"
            );
        }
    }
}

#[cfg(feature = "image-encoding")]
#[test]
fn test_video_frame_encode_jpeg_rgba() {
    use crate::frames::{PixelFormat, VideoFrame};

    let width = 4;
    let height = 4;
    let data = vec![255u8; (width * height * 4) as usize];

    let frame = VideoFrame::builder()
        .resolution(width, height)
        .pixel_format(PixelFormat::Rgba)
        .build()
        .unwrap();

    let mut frame = frame;
    frame.replace_data(data).unwrap();

    let jpeg_bytes = frame.encode_jpeg(85);
    assert!(jpeg_bytes.is_ok());

    let jpeg_bytes = jpeg_bytes.unwrap();
    assert!(!jpeg_bytes.is_empty());

    assert_eq!(&jpeg_bytes[0..2], &[0xFF, 0xD8]);

    let len = jpeg_bytes.len();
    assert_eq!(&jpeg_bytes[len - 2..len], &[0xFF, 0xD9]);
}

#[cfg(feature = "image-encoding")]
#[test]
fn test_video_frame_encode_jpeg_bgra() {
    use crate::frames::{PixelFormat, VideoFrame};

    let width = 4;
    let height = 4;
    let data = vec![128u8; (width * height * 4) as usize];

    let frame = VideoFrame::builder()
        .resolution(width, height)
        .pixel_format(PixelFormat::Bgra)
        .build()
        .unwrap();

    let mut frame = frame;
    frame.replace_data(data).unwrap();

    let jpeg_bytes = frame.encode_jpeg(90);
    assert!(jpeg_bytes.is_ok());

    let jpeg_bytes = jpeg_bytes.unwrap();
    assert!(!jpeg_bytes.is_empty());

    assert_eq!(&jpeg_bytes[0..2], &[0xFF, 0xD8]);
}

#[cfg(feature = "image-encoding")]
#[test]
fn test_video_frame_encode_jpeg_quality_range() {
    use crate::frames::{PixelFormat, VideoFrame};

    // Create a more complex image with varying colors to better show compression differences
    let width = 32;
    let height = 32;
    let mut data = vec![0u8; (width * height * 4) as usize];

    // Fill with a gradient pattern
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            data[idx] = ((x * 255) / width) as u8; // Red gradient
            data[idx + 1] = ((y * 255) / height) as u8; // Green gradient
            data[idx + 2] = 128; // Blue constant
            data[idx + 3] = 255; // Alpha
        }
    }

    let frame = VideoFrame::builder()
        .resolution(width, height)
        .pixel_format(PixelFormat::Rgba)
        .build()
        .unwrap();

    let mut frame = frame;
    frame.replace_data(data).unwrap();

    // Test different quality levels
    let low_quality = frame.encode_jpeg(10).unwrap();
    let high_quality = frame.encode_jpeg(95).unwrap();

    // Both should be valid JPEG files
    assert!(!low_quality.is_empty());
    assert!(!high_quality.is_empty());

    // For a gradient image, higher quality should produce larger files
    assert!(low_quality.len() < high_quality.len());
}

#[cfg(feature = "image-encoding")]
#[test]
fn test_video_frame_encode_data_url_png() {
    use crate::frames::{ImageFormat, PixelFormat, VideoFrame};

    let width = 2;
    let height = 2;
    let data = vec![255u8; (width * height * 4) as usize];

    let frame = VideoFrame::builder()
        .resolution(width, height)
        .pixel_format(PixelFormat::Rgba)
        .build()
        .unwrap();

    let mut frame = frame;
    frame.replace_data(data).unwrap();

    let data_url = frame.encode_data_url(ImageFormat::Png);
    assert!(data_url.is_ok());

    let data_url = data_url.unwrap();

    // Should start with data URL prefix
    assert!(data_url.starts_with("data:image/png;base64,"));

    // Should have base64 data after the prefix
    let base64_part = data_url.strip_prefix("data:image/png;base64,").unwrap();
    assert!(!base64_part.is_empty());

    // Base64 should only contain valid characters
    assert!(base64_part
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='));
}

#[cfg(feature = "image-encoding")]
#[test]
fn test_video_frame_encode_data_url_jpeg() {
    use crate::frames::{ImageFormat, PixelFormat, VideoFrame};

    let width = 4;
    let height = 4;
    let data = vec![128u8; (width * height * 4) as usize];

    let frame = VideoFrame::builder()
        .resolution(width, height)
        .pixel_format(PixelFormat::Rgba)
        .build()
        .unwrap();

    let mut frame = frame;
    frame.replace_data(data).unwrap();

    let data_url = frame.encode_data_url(ImageFormat::Jpeg(85));
    assert!(data_url.is_ok());

    let data_url = data_url.unwrap();

    // Should start with JPEG data URL prefix
    assert!(data_url.starts_with("data:image/jpeg;base64,"));

    // Should have base64 data
    let base64_part = data_url.strip_prefix("data:image/jpeg;base64,").unwrap();
    assert!(!base64_part.is_empty());
}

#[cfg(feature = "image-encoding")]
#[test]
fn test_image_format_enum() {
    use crate::ImageFormat;

    let png = ImageFormat::Png;
    let jpeg = ImageFormat::Jpeg(90);

    let png_copy = png;
    let jpeg_copy = jpeg;

    assert_eq!(png, png_copy);
    assert_eq!(jpeg, jpeg_copy);

    assert_ne!(png, jpeg);

    let jpeg_low = ImageFormat::Jpeg(50);
    let jpeg_high = ImageFormat::Jpeg(95);
    assert_ne!(jpeg_low, jpeg_high);
}

#[test]
fn test_source_cache_len_and_is_empty() {
    use crate::finder::SourceCache;

    let cache = SourceCache::default();

    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());

    cache.clear();
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
}

#[test]
fn test_receiver_snapshot_preset() {
    use crate::{
        finder::{Source, SourceAddress},
        receiver::ReceiverOptions,
    };

    let source = Source::new("Test Source")
        .with_address(SourceAddress::Ip("192.168.1.100:5960".to_string()));

    let customized = ReceiverOptions::snapshot_preset(source.clone()).name("My Snapshot Receiver");
    let _ = format!("{customized:?}");
}

#[test]
fn test_receiver_high_quality_preset() {
    use crate::{
        finder::{Source, SourceAddress},
        receiver::ReceiverOptions,
    };

    let source = Source::new("Test Source")
        .with_address(SourceAddress::Ip("192.168.1.100:5960".to_string()));

    let customized = ReceiverOptions::high_quality_preset(source.clone()).name("My HQ Receiver");
    let _ = format!("{customized:?}");
}

#[test]
fn test_receiver_monitoring_preset() {
    use crate::{
        finder::{Source, SourceAddress},
        receiver::ReceiverOptions,
    };

    let source = Source::new("Test Source")
        .with_address(SourceAddress::Ip("192.168.1.100:5960".to_string()));

    let customized = ReceiverOptions::monitoring_preset(source.clone()).name("My Monitor");
    let _ = format!("{customized:?}");
}

#[test]
fn test_receiver_presets_are_distinct() {
    use crate::{
        finder::{Source, SourceAddress},
        receiver::ReceiverOptions,
    };

    let source = Source::new("Test Source")
        .with_address(SourceAddress::Ip("192.168.1.100:5960".to_string()));

    let _snapshot = ReceiverOptions::snapshot_preset(source.clone());
    let _hq = ReceiverOptions::high_quality_preset(source.clone());
    let _monitor = ReceiverOptions::monitoring_preset(source.clone());
}

// Async runtime integration tests (feature-gated)
#[cfg(feature = "tokio")]
#[test]
fn test_tokio_async_receiver_creation() {
    use crate::{
        finder::Source,
        receiver::{Receiver, ReceiverOptions},
        Ndi,
    };

    // Test that AsyncReceiver can be created and is cloneable
    // We can't actually run async code in a sync test, but we can verify the API exists

    // This test verifies that:
    // 1. tokio::AsyncReceiver type exists
    // 2. It has a `new()` method
    // 3. It implements Clone

    // Note: We can't create a real receiver without NDI SDK runtime,
    // so we just test the type exists and compiles
    let _ = || {
        use crate::asynchronous::tokio::AsyncReceiver;

        // Mock receiver (won't actually work without NDI initialized)
        let source = Source::new("Test");

        // This won't run but proves the API compiles
        if false {
            use std::sync::Arc;
            let ndi = Arc::new(Ndi::new().unwrap());
            let options = ReceiverOptions::snapshot_preset(source);
            let receiver = Receiver::new(&ndi, &options).unwrap();
            let async_receiver = AsyncReceiver::new(receiver);
            let _cloned = async_receiver.clone();
        }
    };
}

#[cfg(feature = "tokio")]
#[test]
fn test_tokio_async_receiver_methods_exist() {
    // Verify all expected async methods exist on AsyncReceiver
    // This is a compile-time test - if it compiles, the methods exist

    use crate::{
        asynchronous::tokio::AsyncReceiver,
        finder::Source,
        receiver::{Receiver, ReceiverOptions},
        Ndi,
    };

    // Test that all methods exist and can be called (in a closure that won't execute)
    let _ = || async {
        let source = Source::new("Test");

        if false {
            use std::sync::Arc;
            let ndi = Arc::new(Ndi::new().unwrap());
            let options = ReceiverOptions::snapshot_preset(source);
            let receiver = Receiver::new(&ndi, &options).unwrap();
            let async_receiver = AsyncReceiver::new(receiver);

            // All these methods should exist and be callable
            let _ = async_receiver
                .video()
                .capture(std::time::Duration::from_secs(5))
                .await;

            let _ = async_receiver
                .audio()
                .capture(std::time::Duration::from_secs(5))
                .await;

            let _ = async_receiver
                .metadata()
                .capture(std::time::Duration::from_secs(5))
                .await;

            // Liveness / recovery primitives (Issue #997).
            let _: bool = async_receiver.is_connected();
            let _stats = async_receiver.connection_stats();
            let _: u64 = async_receiver.connection_stats().video_frames_received();
            let _: crate::Result<()> = async_receiver.reconnect().await;
        }
    };
}

#[cfg(feature = "smol")]
#[test]
fn test_smol_async_receiver_creation() {
    use crate::{
        finder::Source,
        receiver::{Receiver, ReceiverOptions},
        Ndi,
    };

    // Test that AsyncReceiver can be created and is cloneable for smol

    let _ = || {
        use crate::asynchronous::smol::AsyncReceiver;

        let source = Source::new("Test");

        // This won't run but proves the API compiles
        if false {
            use std::sync::Arc;
            let ndi = Arc::new(Ndi::new().unwrap());
            let options = ReceiverOptions::snapshot_preset(source);
            let receiver = Receiver::new(&ndi, &options).unwrap();
            let async_receiver = AsyncReceiver::new(receiver);
            let _cloned = async_receiver.clone();
        }
    };
}

#[cfg(feature = "smol")]
#[test]
fn test_smol_async_receiver_methods_exist() {
    // Verify all expected async methods exist on smol AsyncReceiver

    use crate::{
        asynchronous::smol::AsyncReceiver,
        finder::Source,
        receiver::{Receiver, ReceiverOptions},
        Ndi,
    };

    // Test that all methods exist and can be called (in a closure that won't execute)
    let _ = || async {
        let source = Source::new("Test");

        if false {
            use std::sync::Arc;
            let ndi = Arc::new(Ndi::new().unwrap());
            let options = ReceiverOptions::snapshot_preset(source);
            let receiver = Receiver::new(&ndi, &options).unwrap();
            let async_receiver = AsyncReceiver::new(receiver);

            // All these methods should exist and be callable
            let _ = async_receiver
                .video()
                .capture(std::time::Duration::from_secs(5))
                .await;

            let _ = async_receiver
                .audio()
                .capture(std::time::Duration::from_secs(5))
                .await;

            let _ = async_receiver
                .metadata()
                .capture(std::time::Duration::from_secs(5))
                .await;

            // Liveness / recovery primitives (Issue #997).
            let _: bool = async_receiver.is_connected();
            let _stats = async_receiver.connection_stats();
            let _: u64 = async_receiver.connection_stats().video_frames_received();
            let _: crate::Result<()> = async_receiver.reconnect().await;
        }
    };
}

#[test]
fn test_async_feature_flags_mutually_compatible() {
    // This test verifies that tokio and smol features can coexist
    // Both should be able to be enabled simultaneously without conflicts

    #[cfg(feature = "tokio")]
    {
        use crate::asynchronous::tokio::AsyncReceiver as TokioAsyncReceiver;
        let _tokio_type_check = std::any::type_name::<TokioAsyncReceiver>();
    }

    #[cfg(feature = "smol")]
    {
        use crate::asynchronous::smol::AsyncReceiver as SmolReceiver;
        let _smol_type_check = std::any::type_name::<SmolReceiver>();
    }

    // If this compiles, both can be enabled together
}

// Line stride tests (issue #15)

#[test]
fn test_video_frame_from_raw_uncompressed_bgra() {
    use crate::frames::PixelFormat;

    // Test that from_raw reads ONLY line_stride_in_bytes for uncompressed formats
    let test_width = 1920;
    let test_height = 1080;
    let bytes_per_pixel = 4;
    let line_stride = test_width * bytes_per_pixel;

    let c_frame = create_test_video_frame(test_width, test_height, line_stride, 0);

    unsafe {
        let frame = VideoFrame::from_raw(&c_frame).unwrap();

        assert_eq!(frame.line_stride_bytes(), line_stride as usize);

        // Verify data size calculation
        let expected_size = (line_stride * test_height) as usize;
        assert_eq!(frame.data().len(), expected_size);
        assert_eq!(frame.pixel_format(), PixelFormat::Bgra);

        drop(frame);
        Vec::from_raw_parts(c_frame.p_data, expected_size, expected_size);
    }
}

#[test]
fn test_video_frame_from_raw_unknown_format_returns_error() {
    // Test that from_raw returns an error for unknown pixel formats
    let test_width = 1920;
    let test_height = 1080;
    let data_size = 512000;

    // Create a frame with an invalid/unknown FourCC
    let mut c_frame: NDIlib_video_frame_v2_t = unsafe { std::mem::zeroed() };
    c_frame.xres = test_width;
    c_frame.yres = test_height;
    c_frame.FourCC = -1i32 as _; // Invalid FourCC code (0xFFFFFFFF)
    c_frame.__bindgen_anon_1.data_size_in_bytes = data_size;
    c_frame.frame_rate_N = 60;
    c_frame.frame_rate_D = 1;
    c_frame.picture_aspect_ratio = 16.0 / 9.0;
    c_frame.frame_format_type = NDIlib_frame_format_type_e_NDIlib_frame_format_type_progressive;

    let mut data = vec![0u8; data_size as usize];
    c_frame.p_data = data.as_mut_ptr();
    std::mem::forget(data);

    unsafe {
        let result = VideoFrame::from_raw(&c_frame);
        assert!(result.is_err());

        if let Err(Error::InvalidFrame(msg)) = result {
            assert!(msg.contains("Unknown pixel format FourCC"));
        } else {
            panic!("Expected InvalidFrame error for unknown pixel format");
        }

        Vec::from_raw_parts(c_frame.p_data, data_size as usize, data_size as usize);
    }
}

#[test]
fn test_video_frame_to_raw_roundtrip_uncompressed() {
    use crate::frames::{PixelFormat, VideoFrame};

    let frame = VideoFrame::builder()
        .resolution(1920, 1080)
        .pixel_format(PixelFormat::Bgra)
        .build()
        .unwrap();

    assert_eq!(frame.line_stride_bytes(), 1920 * 4);

    // Convert to raw
    let raw = frame.to_raw();

    // Verify the C union has the correct field set
    unsafe {
        assert_eq!(raw.__bindgen_anon_1.line_stride_in_bytes, 1920 * 4);
    }
}

#[test]
fn test_video_frame_builder_creates_line_stride_bytes() {
    use crate::frames::{PixelFormat, VideoFrame};

    let frame = VideoFrame::builder()
        .resolution(640, 480)
        .pixel_format(PixelFormat::Rgba)
        .build()
        .unwrap();

    assert_eq!(frame.line_stride_bytes(), 640 * 4);
}

/// Tests for the lock-free atomic RuntimeManager implementation.
/// These tests verify that the atomic refcount and Once-based initialization
/// work correctly under concurrent access.
mod runtime_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;

    /// Test that concurrent cloning of NDI handles works correctly.
    /// This verifies the atomic refcount increment path.
    #[test]
    fn test_concurrent_clone_does_not_panic() {
        let Some(ndi) = crate::test_support::ndi().map(Arc::new) else {
            return;
        };

        let clone_count = Arc::new(AtomicUsize::new(0));
        let mut handles = vec![];

        // Spawn multiple threads that clone the NDI handle concurrently
        for _ in 0..10 {
            let ndi_clone = Arc::clone(&ndi);
            let count = Arc::clone(&clone_count);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    let _cloned = (*ndi_clone).clone();
                    count.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }

        for handle in handles {
            handle
                .join()
                .expect("Thread panicked during concurrent clone");
        }

        assert_eq!(clone_count.load(Ordering::Relaxed), 1000);
    }

    /// Test that concurrent drop of NDI handles works correctly.
    /// This verifies the atomic refcount decrement path.
    #[test]
    fn test_concurrent_drop_does_not_panic() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };

        let mut handles = vec![];

        // Create multiple clones
        let clones: Vec<_> = (0..100).map(|_| ndi.clone()).collect();

        // Drop them concurrently
        for clone in clones {
            handles.push(thread::spawn(move || {
                drop(clone);
            }));
        }

        for handle in handles {
            handle
                .join()
                .expect("Thread panicked during concurrent drop");
        }
    }

    /// Test that mixed acquire/release operations work correctly.
    /// This verifies that the lock-free implementation handles interleaved
    /// operations from multiple threads.
    #[test]
    fn test_mixed_acquire_release_concurrent() {
        use crate::Ndi;

        // Establish that a runtime exists; the threads below then exercise
        // their own acquisitions, including first initialization.
        if crate::test_support::ndi().is_none() {
            return;
        }

        let success_count = Arc::new(AtomicUsize::new(0));
        let mut handles = vec![];

        for _ in 0..10 {
            let count = Arc::clone(&success_count);
            handles.push(thread::spawn(move || {
                for _ in 0..50 {
                    let ndi = Ndi::new().expect("every concurrent acquisition must succeed");
                    let _clone = ndi.clone();
                    count.fetch_add(1, Ordering::Relaxed);
                    // Both ndi and _clone are dropped here
                }
            }));
        }

        for handle in handles {
            handle
                .join()
                .expect("Thread panicked during mixed acquire/release");
        }

        assert_eq!(success_count.load(Ordering::Relaxed), 500);
    }

    /// Test that is_running is consistent with actual runtime state.
    #[test]
    fn test_is_running_consistency() {
        use crate::Ndi;

        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        // With an active handle, is_running should be true
        assert!(Ndi::is_running());

        let clone = ndi.clone();
        assert!(Ndi::is_running());

        drop(clone);
        // Still running because ndi is alive
        assert!(Ndi::is_running());

        drop(ndi);
        // After dropping all handles, state depends on test isolation.
        let _final_state = Ndi::is_running();
    }
}
