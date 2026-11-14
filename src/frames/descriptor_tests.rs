use super::*;

/// Compute the native read independently from the descriptor's stride, planes
/// and scan type, then compare it with the storage that owns its pixel pointer.
pub(crate) fn assert_extent(raw: &NDIlib_video_frame_v2_t, start: *const u8, readable: usize) {
    assert_eq!(raw.p_data.cast_const(), start);
    let format = PixelFormat::try_from(FourCc::from_raw(raw.FourCC)).unwrap();
    // SAFETY: these uncompressed descriptors use the line-stride union member.
    let stride = usize::try_from(unsafe { raw.__bindgen_anon_1.line_stride_in_bytes }).unwrap();
    let height = usize::try_from(raw.yres).unwrap();
    let planes = match format {
        PixelFormat::Bgra | PixelFormat::Uyvy => 1,
        PixelFormat::P216 => 2,
        _ => panic!("unexpected regression-test format {format:?}"),
    };
    let mut needed = stride
        .checked_mul(height)
        .unwrap()
        .checked_mul(planes)
        .unwrap();
    let room = format.send_tail_room();
    needed += room;
    if room != 0
        && raw.frame_format_type != NDIlib_frame_format_type_e_NDIlib_frame_format_type_progressive
        && !height.is_multiple_of(4)
    {
        needed += 2 * stride;
    }
    assert!(
        needed <= readable,
        "native descriptor reads {needed} bytes from {readable} byte storage"
    );
}

#[test]
fn descriptor_uses_its_own_geometry_and_refuses_a_different_format() {
    let storage = vec![0u8; 512 * 32];
    let frame = crate::BorrowedVideoFrame::builder(&storage, 64, 32, PixelFormat::Bgra)
        .line_stride_bytes(512)
        .build()
        .unwrap();
    // Same-format authorization has no geometry, even when acquired from a
    // differently sized and packed frame.
    let other = VideoFrame::builder().resolution(2, 2).build().unwrap();
    let raw = frame
        .video_descriptor(SendableVideoLayout::for_test(*other.layout()))
        .unwrap();
    assert_eq!((raw.xres, raw.yres), (64, 32));
    assert_extent(&raw, storage.as_ptr(), storage.len());
    let uyvy = VideoFrame::builder()
        .resolution(2, 2)
        .pixel_format(PixelFormat::Uyvy)
        .build()
        .unwrap();
    let error = frame
        .video_descriptor(SendableVideoLayout::for_test(*uyvy.layout()))
        .err()
        .expect("different format must be refused");
    assert!(matches!(error, Error::InvalidFrame(_)));
}
