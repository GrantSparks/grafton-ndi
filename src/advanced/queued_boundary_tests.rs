use super::*;

fn retain(state: &CallbackState, id: u64) -> usize {
    let frame = QueuedVideoFrame::Pixels(VideoFrame::builder().resolution(2, 2).build().unwrap());
    let header = Box::new(frame.raw());
    let key = header.p_data as usize;
    let bytes = frame.retained_bytes();
    let mut locked = state.state.lock().unwrap();
    locked.bytes += bytes;
    locked.entries.insert(
        key,
        Entry {
            id: SubmissionId(id),
            _header: header,
            frame,
            bytes,
            complete: false,
        },
    );
    key
}

#[test]
fn completion_trampoline_accepts_copied_descriptors_on_concurrent_threads() {
    let state = CallbackState::new(QueueOptions::default()).unwrap();
    let keys = [retain(&state, 1), retain(&state, 2)];
    std::thread::scope(|scope| {
        for key in keys {
            let state = &*state;
            scope.spawn(move || {
                // The SDK may return its own descriptor address. The retained
                // allocation identity is the contract, not a header address.
                let native = NDIlib_video_frame_v2_t {
                    p_data: key as *mut u8,
                    ..Default::default()
                };
                unsafe { completion((state as *const CallbackState).cast_mut().cast(), &native) };
            });
        }
    });
    let mut locked = state.state.lock().unwrap();
    assert_eq!(locked.entries.len(), 2);
    let mut completed = [
        take_completed(&mut locked).unwrap().id().get(),
        take_completed(&mut locked).unwrap().id().get(),
    ];
    completed.sort_unstable();
    assert_eq!(completed, [1, 2]);
    assert!(locked.entries.is_empty());
    assert_eq!(locked.bytes, 0);
    assert!(!state.protocol_error.load(Ordering::Acquire));
}

#[test]
fn malformed_callback_retains_pending_ownership_and_reports_protocol_failure() {
    let mut state = CallbackState::new(QueueOptions::default()).unwrap();
    retain(&state, 1);
    let opaque = (&mut *state as *mut CallbackState).cast();
    unsafe { completion(opaque, std::ptr::null()) };
    assert!(state.protocol_error.load(Ordering::Acquire));
    let locked = state.state.lock().unwrap();
    assert_eq!(locked.entries.len(), 1);
    assert!(locked.ready.is_empty());
    assert!(locked.bytes > 0);
}
