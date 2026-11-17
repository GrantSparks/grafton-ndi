//! Pin the SDK behavior the queued scatter path uses as its completion key.

use super::*;
use crate::{
    advanced::{EnvelopeCodec, PacketEnvelope, PacketReceiver, PassthroughPreset, VideoCodec},
    sys::advanced::SendCompletion,
    Finder, FinderOptions, ReceiverOptions, SourceAddress, StreamTier, Timecode,
};
use std::{
    ffi::c_void,
    sync::Mutex,
    time::{Duration, Instant},
};

#[derive(Default)]
struct Completions(Mutex<Vec<(usize, i64, i32)>>);

unsafe extern "C" fn completed(opaque: *mut c_void, frame: *const NDIlib_video_frame_v2_t) {
    // SAFETY: registration keeps the state alive until the sender is destroyed;
    // the SDK supplies a readable descriptor for the callback's duration.
    let state = unsafe { &*opaque.cast::<Completions>() };
    let frame = unsafe { &*frame };
    state
        .0
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .push((frame.p_data as usize, frame.timecode, unsafe {
            frame.__bindgen_anon_1.data_size_in_bytes
        }));
}

#[test]
fn native_scatter_completions_preserve_the_submitted_data_key() {
    let Some(ndi) = crate::test_support::advanced_ndi() else {
        return;
    };
    let scatter = ndi.table::<ScatterSend>().unwrap();
    let completion = ndi.table::<SendCompletion>().unwrap();
    // Storage is declared before the sender so unwinding destroys the sender,
    // and therefore finishes every callback, before reclaiming any frame.
    let reported = Box::new(Completions(Mutex::new(Vec::with_capacity(33))));
    let mut frames = Vec::with_capacity(33);
    let mut expected = Vec::with_capacity(33);
    let sender: Sender<CompressedAttached> = Sender::open(
        &ndi,
        &SenderOptions::new(crate::test_support::unique_name("scatter-completion-key"))
            .clock_video(false)
            .clock_audio(false),
    )
    .unwrap();
    // SAFETY: this exclusively owned sender has no existing submissions. State
    // remains allocated until after sender destruction has drained callbacks.
    unsafe {
        completion.NDIlib_send_set_video_async_completion(
            sender.native_instance(),
            (&*reported as *const Completions).cast_mut().cast(),
            Some(completed),
        );
    }
    let submit =
        |number: i64, frames: &mut Vec<PendingVideo>, expected: &mut Vec<(usize, i64, i32)>| {
            let bytes = PacketEnvelope::encode(
                EnvelopeCodec::H264,
                number,
                number,
                true,
                &[0x65; 4000],
                &[0x67; 24],
            )
            .unwrap();
            let mut frame = ScatterVideoFrame::new(
                VideoPacketFormat::new(VideoCodec::H264, 1280, 720)
                    .unwrap()
                    .with_tier(StreamTier::Program),
                vec![
                    bytes[..44].to_vec(),
                    bytes[44..4044].to_vec(),
                    bytes[4044..].to_vec(),
                ],
            )
            .unwrap();
            frame.set_timecode(Timecode::from_ticks(number));
            let key = frame.storage.blocks[0].as_mut_ptr();
            let length = frame.storage.sizes[0].unwrap().to_sdk();
            frames.push(PendingVideo::Scatter(
                ScatterSendable::for_test(frame, Some(scatter)).unwrap(),
            ));
            let pending = frames.last_mut().unwrap();
            let Lent::Scatter(mut proof) = pending.lend() else {
                unreachable!("a scatter frame was prepared");
            };
            let PendingVideo::Scatter(frame) = pending else {
                unreachable!()
            };
            let (pointer, size) = frame.completion_key();
            proof.descriptor_mut().p_data = pointer;
            proof.descriptor_mut().__bindgen_anon_1.data_size_in_bytes = size;
            let frame = proof.frame.descriptor;
            let list = proof.frame.scatter;
            assert_eq!(frame.p_data, key);
            assert_eq!(unsafe { frame.__bindgen_anon_1.data_size_in_bytes }, length);
            expected.push((key as usize, number, length));
            // SAFETY: descriptor and scatter list describe the retained frame; no
            // payload is accessed again until sender destruction drains callbacks.
            unsafe {
                scatter.NDIlib_send_send_video_scatter_async(sender.native_instance(), &frame, list)
            };
        };
    for number in 0..8 {
        submit(number, &mut frames, &mut expected);
    }
    // A sender may expose only its name. Discover its actual port and route
    // the media over loopback, as the integration transport tests do.
    let name = sender.source().unwrap().name().to_owned();
    let finder = Finder::new(&ndi, &FinderOptions::new().extra_ips(["127.0.0.1"])).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut watch = finder.watch();
    let source = 'found: loop {
        for source in watch.current_sources().unwrap() {
            if source.name() == name {
                if let Some(address) = source.address().filter(|address| !address.is_url()) {
                    let address = address.as_str();
                    let port = address.rsplit(':').next().unwrap();
                    let endpoint = SourceAddress::new(format!("127.0.0.1:{port}")).unwrap();
                    break 'found source.with_address(endpoint);
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "local scatter sender was not discovered"
        );
        watch.wait_for_change(Duration::from_millis(100)).unwrap();
    };
    let receiver =
        PacketReceiver::new(&ndi, &ReceiverOptions::new(source), PassthroughPreset::H264).unwrap();
    assert!(sender
        .wait_for_connection(Duration::from_secs(15))
        .unwrap()
        .is_some());
    for number in 8..25 {
        submit(number, &mut frames, &mut expected);
    }
    // A final burst followed immediately by destruction covers outstanding
    // completions, whose keys must obey the same rule as earlier callbacks.
    for number in 25..33 {
        submit(number, &mut frames, &mut expected);
    }
    drop(sender);
    drop(receiver);
    let mut reported = reported.0.lock().unwrap();
    reported.sort_unstable_by_key(|entry| entry.1);
    assert_eq!(*reported, expected);
}
