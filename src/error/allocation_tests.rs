use super::*;
use crate::{frames, sys::bindings::*, FrameMetadataStatus};
use std::time::Duration;

#[test]
fn common_refusals_keep_typed_evidence_without_allocating() {
    let mut pixels = [0u8; 16];
    let raw = NDIlib_video_frame_v2_t {
        xres: 3,
        yres: 1,
        frame_rate_N: 60,
        frame_rate_D: 1,
        FourCC: NDIlib_FourCC_video_type_e_NDIlib_FourCC_video_type_BGRA,
        p_data: pixels.as_mut_ptr(),
        ..Default::default()
    };
    let (result, allocations) =
        crate::test_alloc::allocation_count(|| frames::validate_video_frame(&raw));
    assert!(matches!(
        result,
        Err(Error::InvalidFrame(FrameDefect::Layout(crate::LayoutDefect::VideoOddWidth(value)))) if value.get()==3
    ));
    assert_eq!(allocations, 0);

    let (result, allocations) = crate::test_alloc::allocation_count(|| {
        crate::PixelFormat::Bgra.send_buffer_len(4, 2, Some(7), crate::ScanType::Progressive)
    });
    assert!(matches!(
        result,
        Err(Error::InvalidFrame(FrameDefect::Layout(
            LayoutDefect::VideoShortStride {
                stride: 7,
                minimum: 16,
                format: crate::PixelFormat::Bgra,
                width: 4
            }
        )))
    ));
    assert_eq!(allocations, 0);

    let requested = crate::MAX_TIMEOUT + Duration::from_millis(1);
    let (result, allocations) =
        crate::test_alloc::allocation_count(|| crate::timeout::to_ms_checked(requested));
    assert!(
        matches!(result, Err(Error::InvalidInput(InputDefect::TimeoutTooLong {requested:found})) if found == requested)
    );
    assert_eq!(allocations, 0);

    let withheld = frames::FrameMetadataRef::Withheld(frames::MetadataWithheld::NotUtf8);
    let (result, allocations) =
        crate::test_alloc::allocation_count(|| crate::color_info::Metadata::new(withheld).read());
    assert!(matches!(
        result,
        Err(Error::InvalidFrame(FrameDefect::ColorMetadataWithheld {
            status: FrameMetadataStatus::NotUtf8
        }))
    ));
    assert_eq!(allocations, 0);
}

#[test]
#[cfg(feature = "image-encoding")]
fn refusing_hdr_before_encoding_allocates_nothing() {
    let frame = crate::VideoFrame::builder()
        .resolution(2, 2)
        .metadata("<ndi_color_info transfer=\"bt_2100_pq\"/>")
        .build()
        .unwrap();
    let options = crate::image_encoding::ImageOptions::new(crate::image_encoding::ImageFormat::Png);
    let (result, allocations) =
        crate::test_alloc::allocation_count(|| frame.encode_image(&options));
    assert!(matches!(
        result,
        Err(Error::InvalidFrame(FrameDefect::HdrRefused {
            transfer: crate::TransferFunction::Pq
        }))
    ));
    assert_eq!(allocations, 0);
}

#[test]
fn nested_causes_stay_within_the_hot_path_error_budget() {
    assert!(size_of::<FrameDefect>() <= 32);
    assert!(size_of::<InputDefect>() <= 32);
    assert!(size_of::<LayoutDefect>() <= 24);
    assert!(size_of::<SdkResultDefect>() <= 32);
    assert!(
        size_of::<Error>() <= size_of::<std::ffi::NulError>() + align_of::<std::ffi::NulError>()
    );
}

#[test]
fn wide_diagnostic_evidence_respects_the_platform_budget() {
    let (result, allocations) = crate::test_alloc::allocation_count(|| crate::FrameRate::new(0, 1));
    let Err(Error::InvalidInput(InputDefect::Layout(crate::LayoutDefect::FrameRate { detail }))) =
        result
    else {
        panic!("expected the invalid numerator")
    };
    assert_eq!((detail.numerator, detail.denominator), (0, 1));
    assert_eq!(allocations, usize::from(!cfg!(target_pointer_width = "64")));
    let kept = detail.into_inner();
    assert_eq!((kept.numerator, kept.denominator), (0, 1));
}

fn layout_cause(error: Error) -> LayoutDefect {
    match error {
        Error::InvalidInput(InputDefect::Layout(cause))
        | Error::InvalidFrame(FrameDefect::Layout(cause)) => cause,
        other => panic!("expected a shared layout refusal: {other}"),
    }
}

#[test]
fn layout_producer_pairs_choose_the_outer_classification_only() {
    assert_eq!(
        layout_cause(crate::FrameRate::new(0, 1).unwrap_err()),
        layout_cause(crate::FrameRate::from_sdk(0, 1).unwrap_err())
    );
    let supplied = crate::PictureAspectRatio::new(-1.5).unwrap_err();
    assert_eq!(
        supplied.to_string(),
        "Invalid input: Video frame has invalid picture aspect ratio: -1.5"
    );
    assert_eq!(
        layout_cause(supplied),
        layout_cause(crate::PictureAspectRatio::from_sdk(-1.5).unwrap_err())
    );
    assert_eq!(
        layout_cause(
            crate::PixelFormat::Bgra
                .buffer_len(2, 0, Some(8))
                .unwrap_err()
        ),
        layout_cause(crate::PixelFormat::Bgra.buffer_len(2, 0, None).unwrap_err())
    );
    assert_eq!(
        layout_cause(crate::PixelFormat::Bgra.buffer_len(2, 0, None).unwrap_err()),
        LayoutDefect::VideoHeight(0.into())
    );
}

#[test]
#[cfg(feature = "advanced")]
fn compressed_dimension_producer_pair_shares_its_layout_cause() {
    use crate::advanced::{VideoCodec, VideoPacketFormat};
    use crate::StreamTier;
    let raw = NDIlib_video_frame_v2_t {
        FourCC: VideoCodec::H264.outer(StreamTier::Program, false),
        xres: 0,
        yres: 2,
        frame_rate_N: 60,
        frame_rate_D: 1,
        ..Default::default()
    };
    assert_eq!(
        layout_cause(VideoPacketFormat::new(VideoCodec::H264, 0, 2).unwrap_err()),
        layout_cause(VideoPacketFormat::from_sdk(&raw).unwrap_err())
    );
}
