//! Native races whose barriers need access to the owner's command admission.
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
    // Only the owner removes the command's admission place. Closing after
    // that point races an operation that has started, rather than refusing
    // an operation that was still queued when shutdown began.
    let entered_by = Instant::now() + Duration::from_secs(5);
    while recorder.worker.admission.state.load(Ordering::Acquire) >> 1 != 0 {
        assert!(Instant::now() < entered_by, "the finish never started");
        thread::sleep(Duration::from_millis(1));
    }
    let closing = other.close().submit();
    let outcome = block_on(finishing).unwrap();
    assert!(matches!(outcome, FinishStatus::Closed(_)), "{outcome:?}");
    block_on(closing).unwrap();
    assert!(api.probe(&path).unwrap().duration().is_final());
}
