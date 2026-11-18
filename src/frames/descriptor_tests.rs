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
    let other = uyvy.video_descriptor();
    assert_eq!(other.FourCC, FourCc::from(PixelFormat::Uyvy).raw());
    assert_eq!(other.p_data.cast_const(), uyvy.data().as_ptr());
    assert_eq!(checked.video_descriptor().p_data, raw.p_data);
}

/// Compile the actual private trait against a deliberately forbidden helper.
/// Stubs replace only accessor data types; method membership comes from the
/// declaration in frames.rs, so moving a helper back there breaks this check.
#[cfg(not(miri))]
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
enum Initialized { ActiveRows, Storage { room_after: usize } }
mod pixels { pub fn send_room(_: &super::ValidatedVideoLayout, _: ()) -> usize { 0 } }
"#;
    let directory = crate::test_support::FixtureDir::create("video-parts-contract");
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
        .arg(directory.path())
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
        "can_send_direct",
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
            .arg(directory.path())
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
}

/// Pin the accessor boundary against the declarations compiled by the crate.
#[cfg(not(miri))]
#[test]
fn audio_and_metadata_parts_implementations_cannot_override_shared_helpers() {
    let source = include_str!("../frames.rs");
    let syntax = syn::parse_file(source).unwrap();
    for (name, accessors, helpers, fixture) in [
        (
            "AudioParts",
            &[
                "layout",
                "audio_timecode",
                "sdk_timestamp",
                "samples",
                "frame_metadata",
            ][..],
            &[
                "audio_descriptor",
                "channel_view",
                "channel_views",
                "contiguous_samples",
                "owned_copy",
                "copy_into_owned",
            ][..],
            include_str!(
                "../../tests/ui/internal/fail/audio_parts_helpers_cannot_be_overridden.rs"
            ),
        ),
        (
            "MetadataParts",
            &["metadata_text_for_send", "metadata_timecode"][..],
            &["metadata_raw"][..],
            include_str!(
                "../../tests/ui/internal/fail/metadata_parts_helpers_cannot_be_overridden.rs"
            ),
        ),
    ] {
        let item = syntax
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Trait(item) if item.ident == name => Some(item),
                _ => None,
            })
            .unwrap();
        let declared: Vec<_> = item
            .items
            .iter()
            .map(|item| match item {
                syn::TraitItem::Fn(method) => {
                    assert!(method.default.is_none());
                    method.sig.ident.to_string()
                }
                _ => panic!("{name} must declare only accessors"),
            })
            .collect();
        assert_eq!(declared, accessors);
        let marker = format!("pub(crate) unsafe trait {name} {{");
        let body = &source[source.find(&marker).unwrap()..];
        let mut depth = 0;
        let end = body
            .bytes()
            .enumerate()
            .find_map(|(index, byte)| {
                match byte {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(index + 1);
                        }
                    }
                    _ => {}
                }
                None
            })
            .unwrap();
        let declaration = &body[..end];
        let stubs = r#"
#![allow(dead_code)]
struct ValidatedAudioLayout;
struct FrameMetadataRef<'a>(std::marker::PhantomData<&'a ()>);
struct MetadataText<'a>(std::marker::PhantomData<&'a ()>);
struct SdkTimestamp;
enum Initialized { ActiveRows, Storage { room_after: usize } }
struct Timecode;
"#;
        let directory = crate::test_support::FixtureDir::create(&format!("{name}-contract"));
        for helper in std::iter::once(None).chain(helpers.iter().copied().map(Some)) {
            let path = directory.join(format!("{}.rs", helper.unwrap_or("accessors")));
            let fixture = match helper {
                Some(helper) => fixture.replace("FORBIDDEN_HELPER", helper),
                None => fixture.replace("    fn FORBIDDEN_HELPER(&self) {}\n", ""),
            };
            std::fs::write(&path, format!("{stubs}\n{declaration}\n{fixture}")).unwrap();
            let output = std::process::Command::new("rustc")
                .args(["--edition=2021", "--emit=metadata", "--out-dir"])
                .arg(directory.path())
                .arg(&path)
                .output()
                .unwrap();
            let error = String::from_utf8(output.stderr).unwrap();
            match helper {
                Some(helper) => assert!(
                    !output.status.success() && error.contains("E0407") && error.contains(helper),
                    "unexpected compiler result: {error}"
                ),
                None => assert!(output.status.success(), "accessors must compile: {error}"),
            }
        }
    }
}

/// Every pointer returned by these deliberately inconsistent test doubles is
/// readable for either layout. A copy must use only its first layout observation.
#[test]
fn a_growing_video_layout_is_read_once_and_keeps_the_destination_paired() {
    use std::{
        cell::Cell,
        panic::{catch_unwind, AssertUnwindSafe},
    };
    struct ChangingVideo {
        frame: VideoFrame,
        smaller: ValidatedVideoLayout,
        calls: Cell<usize>,
    }
    // SAFETY: both layouts fit the initialized allocation; no views outlive it.
    unsafe impl VideoParts for ChangingVideo {
        fn layout(&self) -> &ValidatedVideoLayout {
            let count = self.calls.get();
            self.calls.set(count + 1);
            if count == 0 {
                &self.smaller
            } else {
                self.frame.layout()
            }
        }
        fn header(&self) -> VideoHeader {
            self.frame.header()
        }
        fn pixels(&self) -> *const u8 {
            self.frame.data.as_ptr()
        }
        fn frame_metadata(&self) -> FrameMetadataRef<'_> {
            self.frame.metadata.view()
        }
        fn sdk_timestamp(&self) -> Option<SdkTimestamp> {
            None
        }
    }
    let build = |width| {
        VideoFrame::builder()
            .resolution(width, 4)
            .pixel_format(PixelFormat::Uyvy)
            .build()
            .unwrap()
    };
    let source = ChangingVideo {
        frame: build(32),
        smaller: *build(16).layout(),
        calls: Cell::new(0),
    };
    let mut destination = build(16);
    assert!(catch_unwind(AssertUnwindSafe(|| source.copy_into_owned(&mut destination))).is_ok());
    assert_eq!(source.calls.get(), 1);
    assert_eq!(
        destination.data().len(),
        destination.layout().data_len_bytes()
    );
    assert!(destination.data.room_bytes().iter().all(|&byte| byte == 0));
    assert_eq!(
        destination.to_owned().data().len(),
        destination.data().len()
    );
}

#[test]
fn a_growing_audio_layout_is_read_once_and_keeps_the_destination_paired() {
    use std::{
        cell::Cell,
        panic::{catch_unwind, AssertUnwindSafe},
    };
    struct ChangingAudio {
        frame: AudioFrame,
        smaller: ValidatedAudioLayout,
        calls: Cell<usize>,
    }
    // SAFETY: both layouts fit the initialized, aligned sample allocation.
    unsafe impl AudioParts for ChangingAudio {
        fn layout(&self) -> &ValidatedAudioLayout {
            let count = self.calls.get();
            self.calls.set(count + 1);
            if count == 0 {
                &self.smaller
            } else {
                self.frame.layout()
            }
        }
        fn audio_timecode(&self) -> Timecode {
            self.frame.timecode
        }
        fn sdk_timestamp(&self) -> Option<SdkTimestamp> {
            None
        }
        fn samples(&self) -> *const f32 {
            self.frame.data.as_ptr()
        }
        fn frame_metadata(&self) -> FrameMetadataRef<'_> {
            self.frame.metadata.view()
        }
    }
    let build = |samples| {
        AudioFrame::builder()
            .num_channels(2)
            .num_samples(samples)
            .build()
            .unwrap()
    };
    let source = ChangingAudio {
        frame: build(32),
        smaller: *build(16).layout(),
        calls: Cell::new(0),
    };
    let mut destination = build(16);
    assert!(catch_unwind(AssertUnwindSafe(|| source.copy_into_owned(&mut destination))).is_ok());
    assert_eq!(source.calls.get(), 1);
    assert_eq!(
        destination.data().len(),
        destination.layout().sample_count()
    );
    assert_eq!(
        destination.to_owned().data().len(),
        destination.data().len()
    );
}

#[test]
fn a_video_accessor_panic_leaves_the_reused_destination_whole() {
    use std::{
        cell::Cell,
        panic::{catch_unwind, AssertUnwindSafe},
    };
    struct PanickingMetadata {
        frame: VideoFrame,
        reads: Cell<usize>,
    }
    // SAFETY: every successful accessor lends this owned frame's valid parts.
    unsafe impl VideoParts for PanickingMetadata {
        fn layout(&self) -> &ValidatedVideoLayout {
            self.frame.layout()
        }
        fn header(&self) -> VideoHeader {
            self.frame.header()
        }
        fn pixels(&self) -> *const u8 {
            self.frame.data.as_ptr()
        }
        fn frame_metadata(&self) -> FrameMetadataRef<'_> {
            let count = self.reads.get();
            self.reads.set(count + 1);
            assert_ne!(count, 0, "test accessor panic");
            self.frame.metadata.view()
        }
        fn sdk_timestamp(&self) -> Option<SdkTimestamp> {
            None
        }
    }
    let build = |width| {
        VideoFrame::builder()
            .resolution(width, 4)
            .pixel_format(PixelFormat::Uyvy)
            .build()
            .unwrap()
    };
    let source = PanickingMetadata {
        frame: build(16),
        reads: Cell::new(0),
    };
    let mut destination = build(32);
    assert!(catch_unwind(AssertUnwindSafe(|| source.copy_into_owned(&mut destination))).is_err());
    assert_eq!(
        destination.data().len(),
        destination.layout().data_len_bytes()
    );
    assert!(destination.data.room_bytes().iter().all(|&byte| byte == 0));
    assert_eq!(
        destination.to_owned().data().len(),
        destination.data().len()
    );
}
