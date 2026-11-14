//! Declined native requests remain visible even when no frame is delivered.
#![cfg(feature = "advanced")]

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name, advanced, loopback);

use grafton_ndi::{
    advanced::{
        AllocationBuffer, AllocationDecline, AllocationRequest, AllocatorLimits, AllocatorService,
    },
    Receiver, ReceiverOptions, Sender, SenderOptions, VideoFrame,
};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[test]
fn an_insufficient_native_byte_grant_counts_declines_without_retaining_storage() {
    let Some(ndi) = common::advanced_ndi() else {
        return;
    };
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("allocator byte declines"))
            .clock_video(false)
            .clock_audio(false),
    )
    .unwrap();
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(common::loopback_source(&ndi, &sender).unwrap()),
    )
    .unwrap();
    // This integration binary has its own process-wide registry, so these
    // intentionally small limits do not compete with tests using defaults.
    let limits = AllocatorLimits::default()
        .max_concurrent_calls(1)
        .max_allocations(2)
        .max_allocation_bytes(1024)
        .max_bytes(1024);
    let service = AllocatorService::new(ndi.process_lifetime(), limits).unwrap();
    let mut receiver = service.attach(receiver).unwrap();
    let policy_calls = Arc::new(AtomicUsize::new(0));
    receiver
        .set_video_allocator({
            let policy_calls = policy_calls.clone();
            move |request: AllocationRequest| {
                policy_calls.fetch_add(1, Ordering::Relaxed);
                AllocationBuffer::zeroed(request)
            }
        })
        .unwrap();
    let video = VideoFrame::builder().resolution(64, 32).build().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while service.usage().declined(AllocationDecline::ByteLimit) == 0 {
        assert!(
            Instant::now() < deadline,
            "the native allocator reported no declined request"
        );
        sender.send_video(&video).unwrap();
        assert!(receiver
            .receiver()
            .video()
            .capture_ref(Duration::from_millis(100))
            .unwrap()
            .is_none());
    }
    let usage = service.usage();
    assert!(usage.declined_allocations() > 0);
    assert_eq!(
        usage.declined_allocations(),
        usage.declined(AllocationDecline::ByteLimit)
    );
    assert_eq!(usage.retained_allocations(), 0);
    assert_eq!(usage.retained_bytes(), 0);
    assert_eq!(usage.in_progress_allocations(), 0);
    assert_eq!(policy_calls.load(Ordering::Relaxed), 0);
    drop(receiver);
    let usage = service.usage();
    assert_eq!(usage.active_generations(), 0);
    assert_eq!(usage.retained_allocations(), 0);
    assert_eq!(usage.retained_bytes(), 0);
}
