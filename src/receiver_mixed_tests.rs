//! Allocation and initialized-active-region checks need no SDK and can run
//! under Miri: every pointer below belongs to a live test allocation.
use super::*;
use crate::{capture::Guard, test_alloc::allocation_count, Timecode};
use std::{mem::MaybeUninit, sync::atomic::AtomicUsize};

unsafe fn count(instance: NDIlib_recv_instance_t) {
    // SAFETY: each test lends its counter through every synthetic guard.
    unsafe { &*instance.cast::<AtomicUsize>() }.fetch_add(1, Ordering::SeqCst);
}
unsafe fn free_video(instance: NDIlib_recv_instance_t, _: &mut NDIlib_video_frame_v2_t) {
    unsafe { count(instance) };
}
unsafe fn free_audio(instance: NDIlib_recv_instance_t, _: &mut NDIlib_audio_frame_v3_t) {
    unsafe { count(instance) };
}
unsafe fn free_metadata(instance: NDIlib_recv_instance_t, _: &mut NDIlib_metadata_frame_t) {
    unsafe { count(instance) };
}

#[test]
fn borrowed_video_item_allocates_nothing_and_owned_copy_skips_uninitialized_padding() {
    let releases = AtomicUsize::new(0);
    let instance = ptr::from_ref(&releases).cast_mut().cast();
    let mut pixels = [MaybeUninit::uninit(); 48];
    for row in [0, 24] {
        for byte in &mut pixels[row..row + 16] {
            byte.write(37u8);
        }
    }
    let raw = NDIlib_video_frame_v2_t {
        xres: 4,
        yres: 2,
        FourCC: crate::FourCc::from(crate::PixelFormat::Bgra).raw(),
        frame_rate_N: 30_000,
        frame_rate_D: 1001,
        picture_aspect_ratio: 1.25,
        frame_format_type: crate::ScanType::Field1.to_sdk(),
        timecode: 1234,
        p_data: pixels.as_mut_ptr().cast(),
        __bindgen_anon_1: NDIlib_video_frame_v2_t__bindgen_ty_1 {
            line_stride_in_bytes: 24,
        },
        p_metadata: c"<video/>".as_ptr(),
        timestamp: 5678,
    };
    let (item, allocations) = allocation_count(|| {
        // SAFETY: active rows are initialized and immutable for the item;
        // padding stays uninitialized and the callback frees only the guard.
        let frame =
            unsafe { VideoKind::make_ref(Guard::with_test_free(instance, raw, free_video)) }
                .unwrap();
        ReceiverItemRef::Video(frame)
    });
    assert_eq!(allocations, 0);
    let (owned, allocations) = allocation_count(|| item.to_owned());
    assert_eq!(allocations, 2, "one pixel buffer and one metadata string");
    let ReceiverItem::Video(frame) = owned else {
        unreachable!()
    };
    assert_eq!(frame.data(), [37; 32]);
    assert_eq!(frame.picture_aspect_ratio(), 1.25);
    assert_eq!(frame.timecode(), Timecode::from_ticks(1234));
    assert_eq!(frame.metadata(), Some("<video/>"));
    assert_eq!(releases.load(Ordering::SeqCst), 0);
    drop(item);
    assert_eq!(releases.load(Ordering::SeqCst), 1);
}

#[test]
fn borrowed_audio_item_allocates_nothing_and_owned_copy_skips_uninitialized_channels() {
    let releases = AtomicUsize::new(0);
    let instance = ptr::from_ref(&releases).cast_mut().cast();
    let mut samples = [MaybeUninit::uninit(); 8];
    for (index, sample) in [(0, 0.25f32), (1, 0.5), (4, -0.25), (5, -0.5)] {
        samples[index].write(sample);
    }
    let raw = NDIlib_audio_frame_v3_t {
        sample_rate: 48_000,
        no_channels: 2,
        no_samples: 2,
        timecode: 2345,
        FourCC: NDIlib_FourCC_audio_type_e_NDIlib_FourCC_audio_type_FLTP,
        p_data: samples.as_mut_ptr().cast(),
        __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
            channel_stride_in_bytes: 16,
        },
        p_metadata: c"<audio/>".as_ptr(),
        timestamp: 6789,
    };
    let (item, allocations) = allocation_count(|| {
        // SAFETY: each active channel is aligned/initialized and remains
        // immutable through the guard. Channel padding is never initialized.
        let frame =
            unsafe { AudioKind::make_ref(Guard::with_test_free(instance, raw, free_audio)) }
                .unwrap();
        ReceiverItemRef::Audio(frame)
    });
    assert_eq!(allocations, 0);
    let (owned, allocations) = allocation_count(|| item.to_owned());
    assert_eq!(allocations, 2, "one sample buffer and one metadata string");
    let ReceiverItem::Audio(frame) = owned else {
        unreachable!()
    };
    assert_eq!(frame.data(), [0.25, 0.5, -0.25, -0.5]);
    assert_eq!(frame.metadata(), Some("<audio/>"));
    assert_eq!(frame.timecode(), Timecode::from_ticks(2345));
    drop(item);
    assert_eq!(releases.load(Ordering::SeqCst), 1);
}

#[test]
fn metadata_and_event_items_have_the_documented_allocation_costs() {
    let releases = AtomicUsize::new(0);
    let raw = NDIlib_metadata_frame_t {
        length: 0,
        timecode: 3456,
        p_data: c"<metadata/>".as_ptr().cast_mut(),
    };
    let (item, allocations) = allocation_count(|| {
        // SAFETY: static NUL-terminated text outlives the synthetic guard,
        // whose callback records release without freeing static storage.
        let frame = unsafe {
            MetadataKind::make_ref(Guard::with_test_free(
                ptr::from_ref(&releases).cast_mut().cast(),
                raw,
                free_metadata,
            ))
        }
        .unwrap();
        ReceiverItemRef::Metadata(frame)
    });
    assert_eq!(allocations, 0);
    let (owned, allocations) = allocation_count(|| item.to_owned());
    assert_eq!(allocations, 1);
    let ReceiverItem::Metadata(frame) = owned else {
        unreachable!()
    };
    assert_eq!(frame.data(), "<metadata/>");
    assert_eq!(frame.timecode(), Timecode::from_ticks(3456));
    drop(item);
    assert_eq!(releases.load(Ordering::SeqCst), 1);
    let (_, allocations) =
        allocation_count(|| ReceiverItemRef::Event(ReceiverEvent::SourceChanged).to_owned());
    assert_eq!(allocations, 0);
}
