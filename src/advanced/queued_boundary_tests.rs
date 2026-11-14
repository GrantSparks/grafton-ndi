use super::*;

fn retain(state: &CallbackState, id: u64) -> usize {
    let pixels = VideoFrame::builder().resolution(2, 2).build().unwrap();
    let bytes = pixels.retained_bytes();
    let authorized = QueuedAuthorization::Pixels(SendableVideoLayout::for_test(*pixels.layout()));
    let mut frame = QueuedStorage::new(pixels.into(), authorized);
    let lent = frame.lend(authorized);
    #[cfg(ndi_has_advanced_scatter_send)]
    let Lent::Frame(raw) = lent
    else {
        unreachable!()
    };
    #[cfg(not(ndi_has_advanced_scatter_send))]
    let Lent::Frame(raw) = lent;
    let header = AliasableBox::new(Box::new(raw));
    let key = header.p_data as usize;
    let mut locked = state.state.lock().unwrap();
    locked.bytes += bytes;
    locked.in_flight.insert(
        key,
        Entry {
            id: SubmissionId(id),
            _header: header,
            frame,
            bytes,
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
    let mut state = CallbackState::new(QueueOptions::default()).unwrap();
    retain(&state, 1);
    let opaque = (&mut *state as *mut CallbackState).cast();
    unsafe { completion(opaque, std::ptr::null()) };
    assert!(state.protocol_error.load(Ordering::Acquire));
    let locked = state.state.lock().unwrap();
    assert_eq!((locked.in_flight.len(), locked.ready.len()), (1, 0));
    assert!(locked.bytes > 0);
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
    let state = CallbackState::new(QueueOptions::new().max_frames(4)).unwrap();
    let mut keys = vec![retain(&state, 1)];
    let mut original = Vec::new();
    for number in 2..=4 {
        let frame = scatter();
        let pointers = frame
            .blocks()
            .map(|block| block.as_ptr() as usize)
            .collect::<Vec<_>>();
        let bytes = frame.retained_bytes();
        let (mut frame, allocations) = crate::test_alloc::allocation_count(|| {
            QueuedStorage::new(frame.into(), QueuedAuthorization::Scatter(table))
        });
        assert_eq!(allocations, 1, "only the stable scatter-list descriptor");
        let (lent, allocations) =
            crate::test_alloc::allocation_count(|| frame.lend(QueuedAuthorization::Scatter(table)));
        assert_eq!(allocations, 0);
        let Lent::Scatter { frame: raw, .. } = lent else {
            panic!("scatter storage")
        };
        assert_eq!(raw.p_data as usize, pointers[0]);
        let (header, allocations) =
            crate::test_alloc::allocation_count(|| AliasableBox::new(Box::new(raw)));
        assert_eq!(allocations, 1, "only the native frame descriptor");
        let key = header.p_data as usize;
        assert!(
            !keys.contains(&key),
            "keys are distinct across all variants"
        );
        keys.push(key);
        original.push((number, pointers));
        let mut locked = state.state.lock().unwrap();
        locked.bytes += bytes;
        locked.in_flight.insert(
            key,
            Entry {
                id: SubmissionId(number),
                _header: header,
                frame,
                bytes,
            },
        );
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
