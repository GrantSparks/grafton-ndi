//! Native cleanup when an executor releases handles from a File callback.
use super::*;
use std::sync::{mpsc, Mutex};

struct ReleaseHandle<T>(Mutex<Option<T>>, mpsc::Sender<()>);

impl<T: Send + 'static> Wake for ReleaseHandle<T> {
    fn wake(self: Arc<Self>) {
        drop(self);
    }
}

impl<T> Drop for ReleaseHandle<T> {
    fn drop(&mut self) {
        drop(self.0.get_mut().unwrap().take());
        let _ = self.1.send(());
    }
}

fn releasing<T: Send + 'static>(handle: T) -> (Waker, mpsc::Receiver<()>) {
    let (released, receive) = mpsc::channel();
    (
        Waker::from(Arc::new(ReleaseHandle(Mutex::new(Some(handle)), released))),
        receive,
    )
}

fn recording(api: &FileApi, source: &LiveSource, path: &Path) -> AsyncFileRecorder {
    let recorder = block_on(AsyncFileRecorder::create(api, &exact_video(source))).unwrap();
    block_on(recorder.start(
        &RecordingTarget::new(path).unwrap(),
        RecordingTimecode::Preserve,
    ))
    .unwrap();
    loop {
        let event = block_on(recorder.next_event(PATIENCE)).unwrap().unwrap();
        assert!(!matches!(
            event.kind(),
            RecorderEventKind::Error | RecorderEventKind::OpenFailed
        ));
        if event.kind() == RecorderEventKind::FirstFrame {
            break;
        }
    }
    while block_on(recorder.next_event(Duration::ZERO))
        .unwrap()
        .is_some()
    {}
    recorder
}

#[test]
fn timer_released_recordings_finalize_while_other_file_deadlines_continue() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = Scratch::new("timer-release");
    let source = LiveSource::start(&ndi, "timer-release");
    let path = scratch.path("take.mkv");
    let recorder = recording(&api, &source, &path);
    let player = block_on(AsyncFilePlayer::create(
        &api,
        &PlayerOptions::new("timer-release-wait"),
    ))
    .unwrap();
    let other = block_on(AsyncFilePlayer::create(
        &api,
        &PlayerOptions::new("timer-release-other"),
    ))
    .unwrap();
    let close_recorder = recorder.close(); // Lazy: keep only the shutdown control.
    let (waker, released) = releasing(recorder);
    let mut expiry = std::pin::pin!(player.next_event(Duration::from_millis(100)));
    assert!(expiry
        .as_mut()
        .poll(&mut Context::from_waker(&waker))
        .is_pending());
    drop(waker);
    // The timer wakes both waits; its first callback starts native finalization.
    assert!(block_on(other.next_event(Duration::from_millis(120)))
        .unwrap()
        .is_none());
    released.recv_timeout(PATIENCE).unwrap();
    block_on(close_recorder).unwrap();
    assert!(api.probe(&path).unwrap().duration().is_final());
    block_on(player.close()).unwrap();
    block_on(other.close()).unwrap();
}

#[test]
fn recorders_whose_callbacks_release_each_other_finalize_without_a_join_cycle() {
    let Some((ndi, api)) = file_api() else {
        return;
    };
    let scratch = Scratch::new("owner-release-cycle");
    let source = LiveSource::start(&ndi, "owner-release-cycle");
    let first_path = scratch.path("first.mkv");
    let second_path = scratch.path("second.mkv");
    let rolled_path = scratch.path("rolled.mkv");
    let first = recording(&api, &source, &first_path);
    let second = recording(&api, &source, &second_path);
    let trigger = first.clone();
    let close_first = first.close();
    let close_second = second.close();
    let mut first_event = std::pin::pin!(first.next_event(PATIENCE));
    let mut second_event = std::pin::pin!(second.next_event(PATIENCE));
    let (first_waker, second_released) = releasing(second);
    let (second_waker, first_released) = releasing(first);
    assert!(first_event
        .as_mut()
        .poll(&mut Context::from_waker(&first_waker))
        .is_pending());
    assert!(second_event
        .as_mut()
        .poll(&mut Context::from_waker(&second_waker))
        .is_pending());
    drop(first_waker);
    drop(second_waker);
    let rolling = trigger
        .rollover(&RecordingTarget::new(&rolled_path).unwrap())
        .submit();
    drop(trigger);
    // A's rollover wakes the task owning B. B's shutdown then wakes the task
    // owning A, so joining either owner from its callback would form a cycle.
    second_released.recv_timeout(PATIENCE).unwrap();
    first_released.recv_timeout(PATIENCE).unwrap();
    block_on(rolling).unwrap();
    block_on(close_first).unwrap();
    block_on(close_second).unwrap();
    // Both original files contain media. Shutdown may stop the rollover's
    // new file before its first frame, so it need not contain a duration.
    for path in [&first_path, &second_path] {
        assert!(
            api.probe(path).unwrap().duration().is_final(),
            "{}",
            path.display()
        );
    }
}
