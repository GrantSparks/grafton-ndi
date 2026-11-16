use super::*;

fn retain(state: &CallbackState, id: u64) -> usize {
    let pixels = VideoFrame::builder().resolution(2, 2).build().unwrap();
    let bytes = pixels.retained_bytes();
    let frame = QueuedVideoFrame::from(pixels).authorize_for_test(None);
    let mut locked = state.state.lock().unwrap();
    let admitted = locked.admit(frame, bytes, &state.protocol_error).unwrap();
    assert_eq!(admitted.id.get(), id);
    admitted.lent.descriptor().p_data as usize
}

#[test]
fn completion_trampoline_accepts_copied_descriptors_on_concurrent_threads() {
    let state = CallbackState::new(QueueLimits::default()).unwrap();
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
    assert_eq!((locked.in_flight.len(), locked.ready.len()), (0, 2));
    let mut completed = [
        take_completed(&mut locked).unwrap().id().get(),
        take_completed(&mut locked).unwrap().id().get(),
    ];
    completed.sort_unstable();
    assert_eq!(completed, [1, 2]);
    assert_eq!(locked.retained(), 0);
    assert_eq!(locked.bytes, 0);
    assert!(!state.protocol_error.load(Ordering::Acquire));
}

#[test]
fn malformed_callback_retains_pending_ownership_and_reports_protocol_failure() {
    let mut state = CallbackState::new(QueueLimits::default()).unwrap();
    retain(&state, 1);
    let opaque = (&mut *state as *mut CallbackState).cast();
    unsafe { completion(opaque, std::ptr::null()) };
    assert!(state.protocol_error.load(Ordering::Acquire));
    let locked = state.state.lock().unwrap();
    assert_eq!((locked.in_flight.len(), locked.ready.len()), (1, 0));
    assert!(locked.bytes > 0);
}

#[test]
fn malformed_callback_publishes_only_after_acquiring_the_state_lock() {
    let state = CallbackState::new(QueueLimits::default()).unwrap();
    let locked = state.state.lock().unwrap();
    std::thread::scope(|scope| {
        let (started, entering) = std::sync::mpsc::channel();
        let (finished, done) = std::sync::mpsc::channel();
        let state = &*state;
        scope.spawn(move || {
            started.send(()).unwrap();
            unsafe {
                completion(
                    (state as *const CallbackState).cast_mut().cast(),
                    std::ptr::null(),
                )
            };
            finished.send(()).unwrap();
        });
        entering.recv().unwrap();
        assert!(done.recv_timeout(Duration::from_millis(50)).is_err());
        assert!(!state.protocol_error.load(Ordering::Acquire));
        drop(locked);
        done.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(state.protocol_error.load(Ordering::Acquire));
    });
}

#[test]
fn malformed_callback_wakes_a_completion_waiter() {
    let state = CallbackState::new(QueueLimits::default()).unwrap();
    retain(&state, 1);
    std::thread::scope(|scope| {
        let (started, entering) = std::sync::mpsc::channel();
        let (finished, done) = std::sync::mpsc::channel();
        let state = &*state;
        scope.spawn(move || {
            started.send(()).unwrap();
            let result = state.next_completed(Deadline::after(Duration::from_secs(30)).unwrap());
            finished.send(result).unwrap();
        });
        entering.recv().unwrap();
        assert!(done.recv_timeout(Duration::from_millis(50)).is_err());
        unsafe {
            completion(
                (state as *const CallbackState).cast_mut().cast(),
                std::ptr::null(),
            )
        };
        let result = done.recv_timeout(Duration::from_secs(1));
        // Do not strand the scoped worker for its whole timeout on failure.
        if result.is_err() {
            state.changed.notify_all();
        }
        assert!(matches!(
            result.unwrap(),
            Err(Error::UnexpectedSdkResult(
                crate::SdkResultDefect::UnknownCompletion
            ))
        ));
    });
}

#[test]
fn malformed_callback_recovers_a_poisoned_state_lock() {
    let state = CallbackState::new(QueueLimits::default()).unwrap();
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _locked = state.state.lock().unwrap();
        panic!("bookkeeping failed");
    }));
    unsafe {
        completion(
            (&*state as *const CallbackState).cast_mut().cast(),
            std::ptr::null(),
        )
    };
    assert!(state.protocol_error.load(Ordering::Acquire));
    assert!(matches!(
        state.next_completed(Deadline::after(Duration::ZERO).unwrap()),
        Err(Error::UnexpectedSdkResult(
            crate::SdkResultDefect::UnknownCompletion
        ))
    ));
}

#[cfg(ndi_has_advanced_scatter_send)]
fn scatter() -> ScatterVideoFrame {
    ScatterVideoFrame::new(
        super::super::VideoPacketFormat::new(super::super::VideoCodec::SpeedHq422, 16, 16).unwrap(),
        vec![vec![0, 1, 2, 3], vec![4, 5, 6, 7]],
    )
    .unwrap()
}

#[test]
fn scatter_byte_budget_counts_capacities_arrays_and_metadata() {
    let mut first = Vec::with_capacity(128);
    first.extend_from_slice(&[0, 1, 2, 3]);
    let mut second = Vec::with_capacity(256);
    second.extend_from_slice(&[4, 5, 6, 7]);
    let mut metadata = String::with_capacity(512);
    metadata.push_str("<pooled/>");
    let expected = first.capacity()
        + second.capacity()
        + metadata.capacity()
        + 2 * size_of::<super::super::aliasable::AliasableVec<u8>>()
        + 3 * (size_of::<*const u8>() + size_of::<i32>());
    #[cfg(ndi_has_advanced_scatter_send)]
    let expected = expected + size_of::<crate::sys::advanced::FrameScatter>();
    let mut frame = ScatterVideoFrame::new(
        super::super::VideoPacketFormat::new(super::super::VideoCodec::SpeedHq422, 16, 16).unwrap(),
        vec![first, second],
    )
    .unwrap();
    frame.set_metadata(metadata).unwrap();
    assert_eq!(QueuedVideoFrame::from(frame).retained_bytes(), expected);
}

#[cfg(ndi_has_advanced_scatter_send)]
#[test]
fn scatter_keys_and_concurrent_completions_preserve_allocations() {
    let Some(ndi) = crate::test_support::advanced_ndi() else {
        return;
    };
    let table = ndi.table::<ScatterSend>().unwrap();
    let state = CallbackState::new(QueueLimits::new().max_frames(4)).unwrap();
    let mut keys = vec![retain(&state, 1)];
    let mut original = Vec::new();
    for number in 2..=4 {
        let frame = scatter();
        let pointers = frame
            .blocks()
            .map(|block| block.as_ptr() as usize)
            .collect::<Vec<_>>();
        let bytes = frame.retained_bytes();
        let (frame, allocations) = crate::test_alloc::allocation_count(|| {
            QueuedVideoFrame::from(frame).authorize_for_test(Some(table))
        });
        assert_eq!(
            allocations, 0,
            "authorization retains capabilities without allocating"
        );
        let mut locked = state.state.lock().unwrap();
        let (admitted, allocations) = crate::test_alloc::allocation_count(|| {
            locked.admit(frame, bytes, &state.protocol_error)
        });
        let admitted = admitted.unwrap();
        assert_eq!(
            allocations, 0,
            "the header uses a reserved slot and the frame owns its scatter list"
        );
        assert_eq!(admitted.lent.descriptor().p_data as usize, pointers[0]);
        assert_eq!(admitted.id.get(), number);
        let key = admitted.lent.descriptor().p_data as usize;
        assert!(
            !keys.contains(&key),
            "keys are distinct across all variants"
        );
        keys.push(key);
        original.push((number, pointers));
    }
    std::thread::scope(|scope| {
        for key in keys.into_iter().rev() {
            let state = &state;
            scope.spawn(move || {
                let ((), allocations) = crate::test_alloc::allocation_count(|| state.complete(key));
                assert_eq!(
                    allocations, 0,
                    "completion moves owned storage without allocating"
                );
            });
        }
    });
    let mut locked = state.state.lock().unwrap();
    assert_eq!(locked.retained(), 4);
    let mut completed = Vec::new();
    while let Some(done) = take_completed(&mut locked) {
        completed.push(done.id().get());
        if let QueuedVideoFrame::Scatter(frame) = done.into_frame() {
            let blocks = frame.into_blocks();
            let pointers = blocks
                .iter()
                .map(|block| block.as_ptr() as usize)
                .collect::<Vec<_>>();
            assert!(original.iter().any(|(_, expected)| *expected == pointers));
        }
    }
    completed.sort_unstable();
    assert_eq!(completed, [1, 2, 3, 4]);
    assert_eq!(locked.bytes, 0);
    assert!(!state.protocol_error.load(Ordering::Acquire));
}
