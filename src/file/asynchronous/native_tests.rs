//! Native races between submitted finishes and recorder shutdown.
use super::{
    super::{
        sdk_tests::{exact_video, LiveSource, Scratch},
        RecorderEventKind,
    },
    tests::block_on,
    *,
};

#[test]
fn a_native_finish_reports_confirmed_closure_when_another_clone_closes_it() {
    let Some((ndi, api)) = crate::test_support::file_api() else {
        return;
    };
    let scratch = Scratch::new("finish-close-race");
    let source = LiveSource::start(&ndi, "finish-close-race");
    let path = scratch.path("take.mkv");
    let recorder = block_on(AsyncFileRecorder::create(&api, &exact_video(&source))).unwrap();
    block_on(recorder.start(
        &RecordingTarget::new(&path).unwrap(),
        RecordingTimecode::Preserve,
    ))
    .unwrap();
    loop {
        let event = block_on(recorder.next_event(Duration::from_secs(5)))
            .unwrap()
            .unwrap();
        assert!(!matches!(
            event.kind(),
            RecorderEventKind::Error | RecorderEventKind::OpenFailed
        ));
        if event.kind() == RecorderEventKind::FirstFrame {
            break;
        }
    }
    let other = recorder.clone();
    let finishing = recorder.finish(Duration::from_secs(30)).submit();
    // Close immediately: the finish may still be queued when cleanup starts.
    let closing = other.close().submit();
    let outcome = block_on(finishing).unwrap();
    assert!(matches!(outcome, FinishStatus::Closed(_)), "{outcome:?}");
    block_on(closing).unwrap();
    assert!(crate::file::FilePlayer::probe(&api, &path)
        .unwrap()
        .duration()
        .is_final());
}
