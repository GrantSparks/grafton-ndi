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
fn descriptor_uses_only_the_frame_bound_to_its_authorization() {
    let storage = vec![0u8; 512 * 32];
    let frame = crate::BorrowedVideoFrame::builder(&storage, 64, 32, PixelFormat::Bgra)
        .line_stride_bytes(512)
        .build()
        .unwrap();
    let checked = Sendable::for_test(&frame);
    let raw = checked.video_descriptor();
    assert_eq!((raw.xres, raw.yres), (64, 32));
    assert_extent(&raw, storage.as_ptr(), storage.len());
    let uyvy = VideoFrame::builder()
        .resolution(2, 2)
        .pixel_format(PixelFormat::Uyvy)
        .build()
        .unwrap();
    let other = Sendable::for_test(&uyvy).video_descriptor();
    assert_eq!(other.FourCC, FourCc::from(PixelFormat::Uyvy).raw());
    assert_eq!(other.p_data.cast_const(), uyvy.data().as_ptr());
    assert_eq!(checked.video_descriptor().p_data, raw.p_data);
}

/// Compile the actual private trait against a deliberately forbidden helper.
/// Stubs replace only accessor data types; method membership comes from the
/// declaration in frames.rs, so moving a helper back there breaks this check.
#[test]
fn video_parts_implementations_cannot_override_shared_helpers() {
    let source = include_str!("../frames.rs");
    let marker = "pub(crate) unsafe trait VideoParts {";
    let start = source.find(marker).unwrap();
    let body = &source[start..];
    let mut depth = 0;
    let mut end = 0;
    for (index, byte) in body.bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = index + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    let declaration = &body[..end];
    let fixture =
        include_str!("../../tests/ui/internal/fail/video_parts_helpers_cannot_be_overridden.rs");
    let stubs = r#"
#![allow(dead_code)]
struct ValidatedVideoLayout { payload_len_bytes: usize, data_len_bytes: usize }
struct VideoHeader { scan_type: () }
struct FrameMetadataRef<'a>(std::marker::PhantomData<&'a ()>);
struct SdkTimestamp;
mod pixels { pub fn send_room(_: &super::ValidatedVideoLayout, _: ()) -> usize { 0 } }
"#;
    let directory = std::env::temp_dir().join(format!(
        "grafton-video-parts-contract-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let permitted = directory.join("accessors_only.rs");
    std::fs::write(
        &permitted,
        format!(
            "{stubs}\n{declaration}\n{}",
            fixture.replace("    fn FORBIDDEN_HELPER(&self) {}\n", "")
        ),
    )
    .unwrap();
    let accepted = std::process::Command::new("rustc")
        .args(["--edition=2021", "--emit=metadata", "--out-dir"])
        .arg(&directory)
        .arg(&permitted)
        .output()
        .unwrap();
    assert!(
        accepted.status.success(),
        "accessor fixture must compile: {}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    for helper in [
        "owned_copy",
        "copy_into_owned",
        "copy_active_pixels",
        "video_descriptor",
    ] {
        let path = directory.join(format!("{helper}.rs"));
        std::fs::write(
            &path,
            format!(
                "{stubs}\n{declaration}\n{}",
                fixture.replace("FORBIDDEN_HELPER", helper)
            ),
        )
        .unwrap();
        let output = std::process::Command::new("rustc")
            .args(["--edition=2021", "--emit=metadata", "--out-dir"])
            .arg(&directory)
            .arg(&path)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{helper} became overridable");
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(
            error.contains("E0407") && error.contains(helper),
            "unexpected compiler refusal: {error}"
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
}
