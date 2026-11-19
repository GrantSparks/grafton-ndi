//! Receiver-view integration needs a real native receiver.

use super::ownership_tests::{Calls, CALLS, TABLE};
use super::*;
use crate::ReceiverOptions;
use std::time::Duration;

#[test]
fn restricted_views_and_rejected_captures_add_no_allocations_or_native_calls() {
    let Some(ndi) = crate::test_support::ndi() else {
        return;
    };
    let receiver = Receiver::new(&ndi, &ReceiverOptions::disconnected()).unwrap();
    CALLS.with_borrow_mut(|calls| *calls = Calls::default());
    let mut session = KvmSession {
        receiver: &receiver,
        kvm: &TABLE,
        held: HeldInputs::default(),
    };
    session.set_clipboard_text("clipboard é").unwrap();
    assert!(matches!(
        session.set_clipboard_text("bad\0text"),
        Err(Error::InvalidCString(_))
    ));
    CALLS.with_borrow(|calls| assert_eq!(calls.clipboard, "clipboard é"));
    CALLS.with_borrow_mut(|calls| *calls = Calls::default());
    let invalid = crate::MAX_TIMEOUT + Duration::from_millis(1);
    let (view, allocations) = crate::test_alloc::allocation_count(|| {
        let view = session.receiver();
        assert!(view.capture_video_ref(invalid).is_err());
        assert!(view.capture_audio_ref(invalid).is_err());
        assert!(view.capture_metadata_ref(invalid).is_err());
        let _ = view.ptz();
        view
    });
    assert_eq!(allocations, 0);
    assert_eq!(
        std::mem::size_of_val(&view),
        std::mem::size_of::<&Receiver>()
    );
    CALLS.with_borrow(|calls| assert!(calls.instances.is_empty()));
    // Compare delegated controls with the same operation on the owner.
    let (_, allocations) = crate::test_alloc::allocation_count(|| {
        assert_eq!(view.is_connected(), receiver.is_connected());
        view.set_tally(crate::Tally::new(false, true));
    });
    assert_eq!(allocations, 0);
    let invalid = crate::MAX_TIMEOUT + Duration::from_millis(1);
    let (_, direct) = crate::test_alloc::allocation_count(|| {
        assert!(receiver.capture_video_ref(invalid).is_err());
    });
    let (_, forwarded) = crate::test_alloc::allocation_count(|| {
        assert!(view.capture_video_ref(invalid).is_err());
    });
    assert_eq!(forwarded, direct);
}
