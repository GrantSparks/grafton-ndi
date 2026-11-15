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
        Err(Error::InvalidFrame(FrameDefect::VideoOddWidth(3)))
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
    let (result, allocations) = crate::test_alloc::allocation_count(|| frame.encode_png());
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
    assert!(size_of::<SdkResultDefect>() <= 32);
    assert!(
        size_of::<Error>() <= size_of::<std::ffi::NulError>() + align_of::<std::ffi::NulError>()
    );
}

#[test]
fn wide_diagnostic_evidence_respects_the_platform_budget() {
    let (result, allocations) = crate::test_alloc::allocation_count(|| crate::FrameRate::new(0, 1));
    let Err(Error::InvalidInput(InputDefect::FrameRate { detail })) = result else {
        panic!("expected the invalid numerator")
    };
    assert_eq!((detail.numerator, detail.denominator), (0, 1));
    assert_eq!(allocations, usize::from(!cfg!(target_pointer_width = "64")));
    let kept = detail.into_inner();
    assert_eq!((kept.numerator, kept.denominator), (0, 1));
}
