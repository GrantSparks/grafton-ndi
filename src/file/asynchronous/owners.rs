//! Native owners and their non-blocking command interface. Native event reads
//! and finalization waits remain private to this module's event-pump impls.

use super::*;
use crate::file::recorder::{CleanupRequest, FinishReply, FinishWaiters, Settlement};

/// Playback's native owner is private to the event-pump module. Only
/// commands that cannot consume native events are forwarded to the handles.
pub(super) struct PlayerOwner(FilePlayer);

impl PlayerOwner {
    #[cfg(test)]
    pub(super) fn new(player: FilePlayer) -> Self {
        Self(player)
    }

    pub(super) fn start_checked(&mut self, path: &crate::file::types::FileText) -> Result<()> {
        self.0.start_checked(path)
    }
    pub(super) fn stop(&mut self) {
        self.0.stop();
    }
    pub(super) fn is_running(&self) -> bool {
        self.0.is_running()
    }
    pub(super) fn stats(&self) -> Result<Option<PlayerStats>> {
        self.0.stats()
    }
    pub(super) fn seek(&mut self, position: FilePosition) -> Result<()> {
        self.0.seek(position)
    }
    pub(super) fn seek_frame_approximate(&mut self, frame: u64) -> Result<()> {
        self.0.seek_frame_approximate(frame)
    }
    pub(super) fn set_paused(&mut self, paused: bool) {
        self.0.set_paused(paused);
    }
    pub(super) fn is_paused(&self) -> bool {
        self.0.is_paused()
    }
    pub(super) fn set_looping(&mut self, looping: bool) {
        self.0.set_looping(looping);
    }
    pub(super) fn is_looping(&self) -> bool {
        self.0.is_looping()
    }
    pub(super) fn duration(&self) -> Option<FileDuration> {
        self.0.duration()
    }
    pub(super) fn position(&self) -> Option<FilePosition> {
        self.0.position()
    }
    pub(super) fn source_timecode(&self) -> Option<Timecode> {
        self.0.source_timecode()
    }
    pub(super) fn advertised_source(&self) -> Result<Source> {
        self.0.advertised_source()
    }
}

impl Owner for PlayerOwner {
    type Finish = std::convert::Infallible;
    fn finish_request(&mut self, finish: Self::Finish) {
        match finish {}
    }
    type Event = PlayerEvent;
    fn poll_native(&mut self) -> Result<Option<Self::Event>> {
        self.0.next_event(Duration::ZERO)
    }
}

/// Recorder state that belongs to its owner thread, never to the native type.
/// Failure teardown explicitly resolves replies before native cleanup.
pub(super) struct RecorderOwner {
    recorder: FileRecorder,
    waiters: FinishWaiters<FinishResponse>,
}

/// A recorder finish command retains its submission budget and response.
pub(super) struct RecorderFinish {
    pub(super) deadline: Deadline,
    pub(super) stop: bool,
    pub(super) reply: FinishResponse,
}

/// Preserve the finish driver's single registry while condition waits expose
/// only settled outcomes. Mapping a reply never starts or queues another job.
pub(super) enum FinishResponse {
    Finish(CommandReply<FinishStatus>),
    Wait(CommandReply<Option<FinishStatus>>),
}

impl FinishReply for FinishResponse {
    fn cancelled(&self) -> bool {
        match self {
            Self::Finish(reply) => reply.shared.cancelled.load(Ordering::Acquire),
            Self::Wait(reply) => reply.shared.cancelled.load(Ordering::Acquire),
        }
    }
    fn complete(self, result: Result<FinishStatus>) {
        match self {
            Self::Finish(reply) => reply.complete(result),
            Self::Wait(reply) => reply.complete(result.map(FinishStatus::into_wait_outcome)),
        }
    }
}

impl RecorderOwner {
    #[cfg(test)]
    pub(super) fn pending_finishes(&self) -> usize {
        self.waiters.len()
    }

    pub(super) fn new(recorder: FileRecorder) -> Self {
        Self {
            waiters: FinishWaiters::new(),
            recorder,
        }
    }

    fn register_finish(&mut self, deadline: Deadline, stop: bool, reply: FinishResponse) {
        if stop {
            self.recorder.request_stop();
        }
        self.waiters.register(&self.recorder, deadline, reply);
    }

    pub(super) fn start_notifying(
        &mut self,
        target: &RecordingTarget,
        timecode: RecordingTimecode,
        events: &EventQueue<RecorderEvent>,
    ) -> Result<u64> {
        self.recorder
            .start_with_sink(target, timecode, &mut |event| events.push(event))
    }

    pub(super) fn reset(&mut self, options: CheckedRecorderOptions) -> Result<RecordingReport> {
        let report = self.recorder.reset_checked(options)?;
        self.waiters.close();
        Ok(report)
    }

    fn settle_at(
        &mut self,
        drained: bool,
        events: &EventQueue<RecorderEvent>,
        control: &Control,
        cleanup_expired: bool,
        at: Instant,
    ) -> bool {
        self.waiters.settle_at(
            &mut self.recorder,
            Settlement {
                drained,
                cleanup_expired,
                at,
            },
            &mut |event| events.push(event),
            || control.shutdown_requested(),
            None,
        )
    }
}

// The private recorder field is visible only here. Command closures can reach
// these non-blocking forwards, never native event waits or blocking finish.
impl RecorderOwner {
    pub(super) fn request_stop(&mut self) -> StopRequest {
        self.recorder.request_stop()
    }
    pub(super) fn state(&self) -> RecorderState {
        self.recorder.state()
    }
    pub(super) fn report(&self) -> RecordingReport {
        self.recorder.report()
    }
    pub(super) fn is_recording(&self) -> bool {
        self.recorder.is_recording()
    }
    pub(super) fn set_source(&mut self, source: &SourceSpec) -> Result<()> {
        self.recorder.set_source(source)
    }
    pub(super) fn last_accepted_source(&self) -> Option<&SourceSpec> {
        self.recorder.last_accepted_source()
    }
    pub(super) fn reported_current_file(&self) -> Option<&EventText> {
        self.recorder.reported_current_file()
    }
    pub(super) fn rollover(&mut self, target: &RecordingTarget) -> Result<()> {
        self.recorder.rollover(target)
    }
    pub(super) fn inject_metadata<S: MetadataSource + ?Sized>(
        &mut self,
        metadata: &S,
    ) -> Result<()> {
        self.recorder.inject_metadata(metadata)
    }
    pub(super) fn stats(&self) -> Result<Option<RecorderStats>> {
        self.recorder.stats()
    }
}

impl Owner for RecorderOwner {
    type Event = RecorderEvent;
    type Finish = RecorderFinish;

    fn poll_native(&mut self) -> Result<Option<Self::Event>> {
        self.recorder.next_event(Duration::ZERO)
    }

    fn after_events(&mut self, drained: bool, events: &EventQueue<Self::Event>, control: &Control) {
        self.settle_at(drained, events, control, false, Instant::now());
    }

    fn wait_timeout(&self) -> Duration {
        self.waiters.wait_timeout(TICK)
    }

    fn fail_pending(&mut self) {
        self.waiters.fail(StateConflict::OwnerFailed);
    }

    fn finish_request(&mut self, finish: RecorderFinish) {
        RecorderOwner::register_finish(self, finish.deadline, finish.stop, finish.reply);
    }

    fn shutdown(
        &mut self,
        requested: Instant,
        events: &EventQueue<Self::Event>,
        control: &Control,
        queued: &mut [Job<Self>],
    ) {
        self.waiters.retain();
        let notify =
            self.waiters.notifies_cleanup() || queued.iter().any(|job| job.finish().is_some());
        if !notify {
            events.close(EventEnd::Stopped);
        }
        self.recorder.drain_cleanup(
            requested,
            &mut self.waiters,
            queued
                .iter_mut()
                .filter_map(Job::take_finish)
                .map(|(place, finish)| {
                    drop(place);
                    (finish.deadline, finish.reply)
                }),
            TICK,
            &mut |event| {
                if notify {
                    events.push(event);
                }
            },
            || {
                if control.skip_cleanup.load(Ordering::Acquire) {
                    CleanupRequest::Abandon
                } else {
                    CleanupRequest::Close
                }
            },
        );
        let _ = control.final_report.set(self.recorder.report());
    }
}

impl HandleOwner for PlayerOwner {
    type Options = FilePlayerOptions;
    const THREAD_NAME: &'static str = "ndi-file-player";

    type Prepared = crate::file::player::PreparedPlayer;

    fn prepare(api: FileApi, options: &Self::Options) -> Result<Self::Prepared> {
        FilePlayer::prepare(api, options)
    }
    fn create(prepared: Self::Prepared) -> Result<Self> {
        prepared.create().map(Self)
    }

    fn dropped_event_count(&self) -> u64 {
        self.0.dropped_event_count()
    }

    fn source_format(&self) -> Result<Option<SourceFormat>> {
        self.0.source_format()
    }
}

impl HandleOwner for RecorderOwner {
    type Options = FileRecorderOptions;
    const THREAD_NAME: &'static str = "ndi-file-recorder";

    type Prepared = crate::file::recorder::PreparedRecorder;

    fn prepare(api: FileApi, options: &Self::Options) -> Result<Self::Prepared> {
        FileRecorder::prepare(api, options)
    }
    fn create(prepared: Self::Prepared) -> Result<Self> {
        prepared.create().map(Self::new)
    }

    fn dropped_event_count(&self) -> u64 {
        self.recorder.dropped_event_count()
    }

    fn source_format(&self) -> Result<Option<SourceFormat>> {
        self.recorder.source_format()
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::block_on;
    use super::*;
    use crate::file::mock::{self, Closing};
    use std::task::Wake;

    #[test]
    fn a_dequeued_finish_is_preserved_when_close_wins_the_command_wakeup() {
        let device = mock::Recorder::new(Closing::AtStop);
        device.install();
        let mut recorder = FileRecorder::new(
            &mock::recorder_api(),
            &FileRecorderOptions::new(SourceSpec::new("mock").unwrap()),
        )
        .unwrap();
        recorder
            .start(
                &RecordingTarget::new(std::env::temp_dir().join("mock-dequeued-finish")).unwrap(),
                RecordingTimecode::Preserve,
            )
            .unwrap();
        let mut owner = RecorderOwner::new(recorder);
        let control = Arc::new(Control::new());
        let events = EventQueue::new(16);
        let (reply, future) = control.command();
        let mut finish = Some(Job::Finish {
            finish: RecorderFinish {
                deadline: Deadline::after(Duration::from_secs(5)).unwrap(),
                stop: true,
                reply: FinishResponse::Finish(reply),
            },
            place: Arc::new(Admission::new(1)).admit(true).unwrap(),
        });
        let dequeued = run_owner_wait(&mut owner, &control, &events, |_, _| {
            control.request_shutdown();
            Ok(finish.take().unwrap())
        });
        assert!(dequeued.is_some());
        owner.shutdown(
            control.shutdown_requested_at().unwrap(),
            &events,
            &control,
            &mut dequeued.into_iter().collect::<Vec<_>>(),
        );
        assert!(matches!(
            block_on(FileOperation::live(future)).unwrap(),
            FinishStatus::Closed(_)
        ));
        drop(owner);
        assert_eq!(device.stops.load(Ordering::Acquire), 1);
        assert!(device.closed_before_destroy.load(Ordering::Acquire));
    }

    #[test]
    fn a_finish_waiter_services_a_status_command_in_its_next_owner_iteration() {
        let device = mock::Recorder::new(Closing::Never);
        device.install();
        let mut recorder = crate::file::FileRecorder::new(
            &mock::recorder_api(),
            &FileRecorderOptions::new(SourceSpec::new("mock").unwrap()),
        )
        .unwrap();
        recorder
            .start(
                &RecordingTarget::new(std::env::temp_dir().join("mock-responsive")).unwrap(),
                RecordingTimecode::Preserve,
            )
            .unwrap();
        let mut owner = RecorderOwner::new(recorder);
        let (reply, finishing) = Arc::new(Control::new()).command();
        let finishing = FileOperation::live(finishing);
        owner.register_finish(
            Deadline::after(Duration::from_secs(2)).unwrap(),
            true,
            FinishResponse::Finish(reply),
        );
        let control = Arc::new(Control::new());
        let mut commands = 0;
        run_owner_wait(
            &mut owner,
            &control,
            &EventQueue::new(16),
            |wait, timeout| {
                assert!(wait);
                assert!(timeout <= TICK);
                commands += 1;
                assert_eq!(commands, 1, "a pending finish blocked a command");
                let control = Arc::clone(&control);
                Ok(Job::Run(Box::new(move |owner: &mut RecorderOwner| {
                    assert_eq!(owner.state(), RecorderState::Stopping);
                    assert!(owner.stats().is_ok());
                    assert_eq!(owner.waiters.len(), 1);
                    control.request_shutdown();
                })))
            },
        );
        assert!(!finishing.is_completed());
        assert_eq!(device.waits.load(Ordering::Acquire), 0);
        drop(finishing);
        owner.recorder.abandon();
    }

    #[test]
    fn an_idle_recorder_pump_makes_only_capture_calls_and_allocates_nothing() {
        for completed_session in [false, true] {
            let device = mock::Recorder::new(Closing::AtStop);
            device.install();
            let mut recorder = crate::file::FileRecorder::new(
                &mock::recorder_api(),
                &FileRecorderOptions::new(SourceSpec::new("mock").unwrap()),
            )
            .unwrap();
            if completed_session {
                recorder
                    .start(
                        &RecordingTarget::new(std::env::temp_dir().join("mock-idle-pump")).unwrap(),
                        RecordingTimecode::Preserve,
                    )
                    .unwrap();
                assert!(matches!(
                    recorder.finish(Duration::ZERO).unwrap(),
                    FinishStatus::Closed(_)
                ));
                assert!(recorder.report().last_closed_path().is_some());
            }
            let mut owner = RecorderOwner::new(recorder);
            let events = EventQueue::new(16);
            let control = Control::new();
            // Exclude the mock SDK's lazy native-mutex initialization while
            // retaining the wrapper's very first pump in the measurement.
            device.prepare_capture();
            let before = device.captures.load(Ordering::Acquire);
            device.recording_queries.store(0, Ordering::Release);
            device.dropped_queries.store(0, Ordering::Release);
            let (_, allocations) = crate::test_alloc::allocation_count(|| {
                for _ in 0..100 {
                    assert!(owner.poll_native().unwrap().is_none());
                    owner.after_events(true, &events, &control);
                }
            });
            assert_eq!(allocations, 0);
            assert_eq!(device.captures.load(Ordering::Acquire) - before, 100);
            assert_eq!(device.recording_queries.load(Ordering::Acquire), 0);
            assert_eq!(device.dropped_queries.load(Ordering::Acquire), 0);
        }
    }

    #[test]
    fn recording_and_stopping_pumps_keep_sampling_live_session_evidence() {
        for stopping in [false, true] {
            let device = mock::Recorder::new(Closing::WhenTold);
            device.install();
            let mut recorder = crate::file::FileRecorder::new(
                &mock::recorder_api(),
                &FileRecorderOptions::new(SourceSpec::new("mock").unwrap()),
            )
            .unwrap();
            recorder
                .start(
                    &RecordingTarget::new(std::env::temp_dir().join("mock-active-pump")).unwrap(),
                    RecordingTimecode::Preserve,
                )
                .unwrap();
            recorder.next_event(Duration::ZERO).unwrap();
            if stopping {
                recorder.request_stop();
            }
            let mut owner = RecorderOwner::new(recorder);
            let events = EventQueue::new(16);
            let control = Control::new();
            let before = device.captures.load(Ordering::Acquire);
            device.recording_queries.store(0, Ordering::Release);
            device.dropped_queries.store(0, Ordering::Release);
            for _ in 0..100 {
                assert!(owner.poll_native().unwrap().is_none());
                owner.after_events(true, &events, &control);
            }
            assert_eq!(device.captures.load(Ordering::Acquire) - before, 100);
            assert_eq!(device.recording_queries.load(Ordering::Acquire), 100);
            assert_eq!(device.dropped_queries.load(Ordering::Acquire), 100);
            owner.recorder.abandon();
        }
    }

    #[test]
    fn an_expired_close_spends_its_final_drain_once_including_drop() {
        let device = mock::Recorder::new(Closing::Never);
        device.install();
        let mut recorder = crate::file::FileRecorder::new(
            &mock::recorder_api(),
            &FileRecorderOptions::new(SourceSpec::new("mock").unwrap()).drop_wait(Duration::ZERO),
        )
        .unwrap();
        let target =
            RecordingTarget::new(std::env::temp_dir().join("mock-close-drain-once")).unwrap();
        recorder
            .start(&target, RecordingTimecode::Preserve)
            .unwrap();
        for _ in 0..1024 {
            device.push(mock::FIRST_FRAME, mock::TAKE);
        }
        let mut owner = RecorderOwner::new(recorder);
        let (reply, finishing) = Arc::new(Control::new()).command();
        let finishing = FileOperation::live(finishing);
        owner.register_finish(
            Deadline::after(Duration::from_secs(30)).unwrap(),
            true,
            FinishResponse::Finish(reply),
        );
        let control = Control::new();
        control.request_shutdown();
        let before = device.captures.load(Ordering::Acquire);
        owner.shutdown(Instant::now(), &EventQueue::new(16), &control, &mut []);
        assert!(matches!(
            block_on(finishing),
            Err(Error::InvalidState(StateConflict::Closed))
        ));
        let after_close = device.captures.load(Ordering::Acquire);
        assert_eq!(
            after_close - before,
            crate::file::recorder::STALE_EVENT_LIMIT
        );
        drop(owner);
        assert_eq!(device.captures.load(Ordering::Acquire), after_close);
        assert_eq!(device.destroys.load(Ordering::Acquire), 1);
    }

    #[test]
    fn start_forwards_stale_events_through_the_recorder_pump() {
        let device = mock::Recorder::new(Closing::AtStop);
        device.install();
        let mut recorder = crate::file::FileRecorder::new(
            &mock::recorder_api(),
            &FileRecorderOptions::new(SourceSpec::new("mock").unwrap()),
        )
        .unwrap();
        let target =
            RecordingTarget::new(std::env::temp_dir().join("mock-start-stale-events")).unwrap();
        device.fail_next_start.store(true, Ordering::Release);
        assert!(recorder
            .start(&target, RecordingTimecode::Preserve)
            .is_err());
        device.push(mock::OPEN_FAILED, "another-old-event.mkv");
        device.oversize_next_event.store(true, Ordering::Release);
        let mut owner = RecorderOwner::new(recorder);
        let events = EventQueue::new(16);
        owner
            .start_notifying(&target, RecordingTimecode::Preserve, &events)
            .unwrap();
        let mut notifications = lock(&events.0);
        assert!(matches!(
            notifications.queue.pop_front().unwrap(),
            Err(Error::UnexpectedSdkResult(_))
        ));
        let event = notifications.queue.pop_front().unwrap().unwrap();
        assert_eq!(event.kind(), crate::file::RecorderEventKind::OpenFailed);
        assert_eq!(owner.recorder.report().error_events(), 0);
    }

    #[test]
    fn readable_stale_events_require_an_empty_poll_before_either_start_path() {
        for asynchronous in [false, true] {
            for stale in [255, 300] {
                let device = mock::Recorder::new(Closing::AtStop);
                device.install();
                let recorder = crate::file::FileRecorder::new(
                    &mock::recorder_api(),
                    &FileRecorderOptions::new(SourceSpec::new("mock").unwrap()),
                )
                .unwrap();
                let target =
                    RecordingTarget::new(std::env::temp_dir().join("mock-readable-stale-events"))
                        .unwrap();
                for _ in 0..stale {
                    device.push(mock::OPEN_FAILED, "earlier.mkv");
                }
                let mut owner = RecorderOwner::new(recorder);
                let result = if asynchronous {
                    owner.start_notifying(
                        &target,
                        RecordingTimecode::Preserve,
                        &EventQueue::new(16),
                    )
                } else {
                    owner.recorder.start(&target, RecordingTimecode::Preserve)
                };
                if stale == 255 {
                    assert!(result.is_ok());
                    assert_eq!(device.starts.load(Ordering::Acquire), 1);
                } else {
                    assert!(matches!(
                        result,
                        Err(Error::InvalidState(StateConflict::EarlierSessionEvents))
                    ));
                    assert_eq!(device.starts.load(Ordering::Acquire), 0);
                }
                assert_eq!(device.captures.load(Ordering::Acquire), 256);
                owner.recorder.abandon();
            }
        }
    }

    #[test]
    fn unreadable_stale_captures_refuse_both_start_paths_at_the_same_limit() {
        for asynchronous in [false, true] {
            let device = mock::Recorder::new(Closing::AtStop);
            device.install();
            let mut recorder = crate::file::FileRecorder::new(
                &mock::recorder_api(),
                &FileRecorderOptions::new(SourceSpec::new("mock").unwrap()),
            )
            .unwrap();
            let target =
                RecordingTarget::new(std::env::temp_dir().join("mock-undrainable-stale-events"))
                    .unwrap();
            device.oversize_every_event.store(true, Ordering::Release);
            let result = if asynchronous {
                RecorderOwner::new(recorder).start_notifying(
                    &target,
                    RecordingTimecode::Preserve,
                    &EventQueue::new(16),
                )
            } else {
                recorder.start(&target, RecordingTimecode::Preserve)
            };
            assert!(matches!(
                result,
                Err(Error::InvalidState(StateConflict::EarlierSessionEvents))
            ));
            assert_eq!(device.starts.load(Ordering::Acquire), 0);
            assert_eq!(
                device.captures.load(Ordering::Acquire),
                crate::file::recorder::STALE_EVENT_LIMIT
            );
            device.oversize_every_event.store(false, Ordering::Release);
        }
    }

    #[test]
    fn a_deadline_drain_is_bounded_even_when_more_events_remain() {
        let device = mock::Recorder::new(Closing::Never);
        device.install();
        let mut recorder = crate::file::FileRecorder::new(
            &mock::recorder_api(),
            &FileRecorderOptions::new(SourceSpec::new("mock").unwrap()),
        )
        .unwrap();
        recorder
            .start(
                &RecordingTarget::new(std::env::temp_dir().join("mock-drain-limit")).unwrap(),
                RecordingTimecode::Preserve,
            )
            .unwrap();
        for _ in 0..1024 {
            device.push(mock::FIRST_FRAME, mock::TAKE);
        }
        let mut owner = RecorderOwner::new(recorder);
        let (reply, finishing) = Arc::new(Control::new()).command();
        let finishing = FileOperation::live(finishing);
        owner.register_finish(
            Deadline::after(Duration::ZERO).unwrap(),
            true,
            FinishResponse::Finish(reply),
        );
        owner.settle_at(
            false,
            &EventQueue::new(16),
            &Control::new(),
            false,
            Instant::now(),
        );
        assert!(matches!(
            block_on(finishing).unwrap(),
            FinishStatus::TimedOut(_)
        ));
        assert!(
            owner.poll_native().unwrap().is_some(),
            "the final drain taken the whole backlog"
        );
        owner.recorder.abandon();
    }

    fn local_finishing_recorder(
        deadline: Deadline,
    ) -> (
        Arc<mock::Recorder>,
        RecorderOwner,
        FileOperation<FinishStatus>,
    ) {
        let device = mock::Recorder::new(Closing::WhenTold);
        device.install();
        let mut recorder = crate::file::FileRecorder::new(
            &mock::recorder_api(),
            &FileRecorderOptions::new(SourceSpec::new("mock").unwrap()),
        )
        .unwrap();
        recorder
            .start(
                &RecordingTarget::new(std::env::temp_dir().join("mock-expiry-drain")).unwrap(),
                RecordingTimecode::Preserve,
            )
            .unwrap();
        recorder.next_event(Duration::ZERO).unwrap();
        let mut owner = RecorderOwner::new(recorder);
        let (reply, finishing) = Arc::new(Control::new()).command();
        let finishing = FileOperation::live(finishing);
        owner.register_finish(deadline, true, FinishResponse::Finish(reply));
        (device, owner, finishing)
    }

    #[test]
    fn a_close_during_reply_settlement_keeps_delivering_pending_cleanup() {
        struct CloseOnReply {
            control: Arc<Control>,
            device: Arc<mock::Recorder>,
        }
        impl Wake for CloseOnReply {
            fn wake(self: Arc<Self>) {
                self.control.request_shutdown();
                self.device.close();
            }
        }
        let device = mock::Recorder::new(Closing::Never);
        device.install();
        let mut recorder = crate::file::FileRecorder::new(
            &mock::recorder_api(),
            &FileRecorderOptions::new(SourceSpec::new("mock").unwrap()),
        )
        .unwrap();
        recorder
            .start(
                &RecordingTarget::new(std::env::temp_dir().join("mock-settlement-close")).unwrap(),
                RecordingTimecode::Preserve,
            )
            .unwrap();
        recorder.next_event(Duration::ZERO).unwrap();
        let mut owner = RecorderOwner::new(recorder);
        let (reply, finishing) = Arc::new(Control::new()).command();
        let finishing = FileOperation::live(finishing);
        owner.register_finish(
            Deadline::after(Duration::from_secs(30)).unwrap(),
            true,
            FinishResponse::Finish(reply),
        );
        device.dropped_events.store(1, Ordering::Release);
        assert!(owner.poll_native().unwrap().is_none());
        assert_eq!(owner.recorder.state(), RecorderState::Indeterminate);
        let control = Arc::new(Control::new());
        let events = EventQueue::new(16);
        let mut finishing = Box::pin(finishing);
        let waker = Waker::from(Arc::new(CloseOnReply {
            control: Arc::clone(&control),
            device: Arc::clone(&device),
        }));
        assert!(finishing
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        owner.after_events(true, &events, &control);
        assert!(control.shutdown_requested());
        assert!(matches!(
            block_on(finishing).unwrap(),
            FinishStatus::Indeterminate(_)
        ));
        owner.shutdown(Instant::now(), &events, &control, &mut []);
        let event = lock(&events.0).queue.pop_front().unwrap().unwrap();
        assert_eq!(event.kind(), crate::file::RecorderEventKind::FileClosed);
    }

    #[test]
    fn reset_identity_ends_waiters_without_the_owners_explicit_drain() {
        let deadline = Deadline::after(Duration::from_secs(30)).unwrap();
        let (_device, mut owner, finishing) = local_finishing_recorder(deadline);
        let replacement = mock::Recorder::new(Closing::AtStop);
        replacement.install();
        // Deliberately bypass RecorderOwner::reset: the generation mismatch
        // alone must prevent following the inherited report or a new session.
        owner
            .recorder
            .reset(&FileRecorderOptions::new(
                SourceSpec::new("replacement").unwrap(),
            ))
            .unwrap();
        owner.settle_at(
            false,
            &EventQueue::new(16),
            &Control::new(),
            false,
            Instant::now(),
        );
        assert!(matches!(
            block_on(finishing),
            Err(Error::InvalidState(StateConflict::Closed))
        ));
    }

    #[test]
    fn failed_inner_reset_keeps_waiters_on_the_original_identity() {
        let deadline = Deadline::after(Duration::from_secs(30)).unwrap();
        let (device, mut owner, finishing) = local_finishing_recorder(deadline);
        let replacement = mock::Recorder::new(Closing::AtStop);
        replacement.refuse_creation.store(true, Ordering::Release);
        replacement.install();
        assert!(owner
            .recorder
            .reset(&FileRecorderOptions::new(
                SourceSpec::new("replacement").unwrap()
            ))
            .is_err());
        owner.settle_at(
            false,
            &EventQueue::new(16),
            &Control::new(),
            false,
            Instant::now(),
        );
        assert!(!finishing.is_completed());
        device.close();
        while owner.poll_native().unwrap().is_some() {}
        owner.settle_at(
            true,
            &EventQueue::new(16),
            &Control::new(),
            false,
            Instant::now(),
        );
        assert!(matches!(
            block_on(finishing).unwrap(),
            FinishStatus::Closed(_)
        ));
    }

    #[test]
    fn expiry_redrains_after_a_pump_poll_that_was_empty_before_the_deadline() {
        let deadline = Deadline::after(Duration::from_secs(1)).unwrap();
        let (device, mut owner, finishing) = local_finishing_recorder(deadline);
        assert!(owner.poll_native().unwrap().is_none());
        device.close();
        // Advance the settlement clock past the earlier empty poll. The
        // closure was queued in between, before this wait's logical expiry.
        owner.settle_at(
            true,
            &EventQueue::new(16),
            &Control::new(),
            false,
            deadline.end(),
        );
        assert!(matches!(
            block_on(finishing).unwrap(),
            FinishStatus::Closed(_)
        ));
        owner.recorder.abandon();
    }

    #[test]
    fn a_deadline_crossed_during_another_reply_gets_its_own_fresh_drain() {
        struct QueueClosure(Arc<mock::Recorder>);

        impl Wake for QueueClosure {
            fn wake(self: Arc<Self>) {
                self.0.close();
            }
        }

        // Supply a logical snapshot between two deadlines, with the wall
        // clock already past both. Any expiry recheck during reply delivery
        // would incorrectly time out the later waiter using the first drain.
        let at = Instant::now() - Duration::from_secs(1);
        let first_deadline =
            Deadline::starting(at, crate::timeout::Timeout::new(Duration::ZERO).unwrap()).unwrap();
        let later_deadline = Deadline::starting(
            at,
            crate::timeout::Timeout::new(Duration::from_nanos(1)).unwrap(),
        )
        .unwrap();
        let (device, mut owner, first) = local_finishing_recorder(first_deadline);
        let mut first = Box::pin(first);
        let waker = Waker::from(Arc::new(QueueClosure(Arc::clone(&device))));
        assert!(first
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending());
        let (reply, later) = Arc::new(Control::new()).command();
        let later = FileOperation::live(later);
        owner.register_finish(later_deadline, false, FinishResponse::Finish(reply));
        let events = EventQueue::new(16);
        let control = Control::new();
        owner.settle_at(false, &events, &control, false, at);
        assert!(matches!(
            block_on(first).unwrap(),
            FinishStatus::TimedOut(_)
        ));
        assert!(
            !later.is_completed(),
            "the later waiter reused the earlier drain"
        );
        // Simulate the first reply's callback taking the owner past the
        // later deadline. Its queued closure needs a new native drain.
        owner.settle_at(true, &events, &control, false, later_deadline.end());
        assert!(matches!(block_on(later).unwrap(), FinishStatus::Closed(_)));
        owner.recorder.abandon();
    }
}
