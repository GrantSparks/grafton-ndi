//! Input ownership at the native boundary, including failed release and drop.

use super::*;
use crate::{sys::advanced::Kvm, ReceiverOptions};
use std::{cell::RefCell, ffi::CStr};

#[derive(Default)]
struct Calls {
    instances: Vec<usize>,
    keys: Vec<i32>,
    releases: usize,
    touches: Vec<f32>,
    touch_finishes: usize,
    mouse_releases: usize,
    clipboard: String,
    fail_key_once: bool,
    fail_buttons: usize,
    fail_touch_once: bool,
}
thread_local! { static CALLS: RefCell<Calls> = RefCell::default(); }

fn record(instance: NDIlib_recv_instance_t) {
    CALLS.with_borrow_mut(|calls| calls.instances.push(instance as usize));
}

unsafe extern "C" fn accept(instance: NDIlib_recv_instance_t) -> bool {
    record(instance);
    true
}
unsafe extern "C" fn scalar(instance: NDIlib_recv_instance_t, _: f32) -> bool {
    record(instance);
    true
}
unsafe extern "C" fn position(instance: NDIlib_recv_instance_t, _: *const f32) -> bool {
    record(instance);
    true
}
unsafe extern "C" fn key(instance: NDIlib_recv_instance_t, value: i32) -> bool {
    record(instance);
    CALLS.with_borrow_mut(|calls| calls.keys.push(value));
    true
}
unsafe extern "C" fn release(instance: NDIlib_recv_instance_t, _: i32) -> bool {
    record(instance);
    CALLS.with_borrow_mut(|calls| {
        calls.releases += 1;
        !std::mem::take(&mut calls.fail_key_once)
    })
}
fn mouse_release(instance: NDIlib_recv_instance_t, mask: usize) -> bool {
    record(instance);
    CALLS.with_borrow_mut(|calls| {
        calls.mouse_releases += 1;
        let refused = calls.fail_buttons & mask != 0;
        calls.fail_buttons &= !mask;
        !refused
    })
}
unsafe extern "C" fn left_release(instance: NDIlib_recv_instance_t) -> bool {
    mouse_release(instance, 1)
}
unsafe extern "C" fn middle_release(instance: NDIlib_recv_instance_t) -> bool {
    mouse_release(instance, 2)
}
unsafe extern "C" fn right_release(instance: NDIlib_recv_instance_t) -> bool {
    mouse_release(instance, 4)
}
unsafe extern "C" fn touch(
    instance: NDIlib_recv_instance_t,
    count: i32,
    points: *const f32,
) -> bool {
    record(instance);
    // SAFETY: the wrapper lends `count` coordinate pairs for this call.
    let points = unsafe { std::slice::from_raw_parts(points, count as usize * 2) };
    CALLS.with_borrow_mut(|calls| calls.touches = points.to_vec());
    true
}
unsafe extern "C" fn finish_touch(instance: NDIlib_recv_instance_t) -> bool {
    record(instance);
    CALLS.with_borrow_mut(|calls| {
        calls.touch_finishes += 1;
        !std::mem::take(&mut calls.fail_touch_once)
    })
}
unsafe extern "C" fn clipboard(
    instance: NDIlib_recv_instance_t,
    value: *const std::ffi::c_char,
) -> bool {
    record(instance);
    // SAFETY: the wrapper lends a CString for the duration of this call.
    let text = unsafe { CStr::from_ptr(value) }.to_str().unwrap();
    CALLS.with_borrow_mut(|calls| calls.clipboard = text.to_owned());
    true
}

static TABLE: Kvm = Kvm {
    NDIlib_recv_kvm_is_supported: accept,
    NDIlib_recv_kvm_send_left_mouse_click: accept,
    NDIlib_recv_kvm_send_middle_mouse_click: accept,
    NDIlib_recv_kvm_send_right_mouse_click: accept,
    NDIlib_recv_kvm_send_left_mouse_release: left_release,
    NDIlib_recv_kvm_send_middle_mouse_release: middle_release,
    NDIlib_recv_kvm_send_right_mouse_release: right_release,
    NDIlib_recv_kvm_send_vertical_mouse_wheel: scalar,
    NDIlib_recv_kvm_send_horizontal_mouse_wheel: scalar,
    NDIlib_recv_kvm_send_mouse_position: position,
    NDIlib_recv_kvm_send_clipboard_contents: clipboard,
    NDIlib_recv_kvm_send_touch_positions: touch,
    NDIlib_recv_kvm_send_touch_finish: finish_touch,
    NDIlib_recv_kvm_send_keyboard_press: key,
    NDIlib_recv_kvm_send_keyboard_release: release,
};

#[test]
fn rejected_releases_are_retained_and_drop_retries_only_the_inputs_still_pressed() {
    let Some(ndi) = crate::test_support::ndi() else {
        return;
    };
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(crate::Source::new(""))).unwrap();
    CALLS.with_borrow_mut(|calls| {
        *calls = Calls {
            fail_key_once: true,
            ..Default::default()
        };
    });
    let mut session = KvmSession {
        receiver: &receiver,
        kvm: &TABLE,
        keys: BTreeSet::new(),
        buttons: BTreeSet::new(),
        touching: false,
    };
    session.key_down(0xff0d).unwrap();
    session.mouse_down(MouseButton::Left).unwrap();
    session.touch(&[[0.25, 0.5], [0.75, 1.0]]).unwrap();
    session.set_clipboard_text("clipboard é").unwrap();
    assert!(matches!(
        session.set_clipboard_text("bad\0text"),
        Err(Error::InvalidCString(_))
    ));
    // The first key release is refused: the session says which function,
    // keeps the key, and still releases the rest.
    assert!(matches!(
        session.release_all(),
        Err(Error::NativeOperationFailed {
            operation: "NDIlib_recv_kvm_send_keyboard_release"
        })
    ));
    assert_eq!(session.keys.len(), 1);
    assert!(session.buttons.is_empty());
    assert!(!session.touching);
    drop(session);
    CALLS.with_borrow(|calls| {
        assert!(calls
            .instances
            .iter()
            .all(|&id| id == receiver.core.instance as usize));
        assert_eq!(calls.keys, [0xff0d]);
        assert_eq!(calls.releases, 2);
        assert_eq!(calls.mouse_releases, 1);
        assert_eq!(calls.touch_finishes, 1);
        assert_eq!(calls.touches, [0.25, 0.5, 0.75, 1.0]);
        assert_eq!(calls.clipboard, "clipboard é");
    });
}

#[test]
fn every_held_input_combination_retries_only_failed_releases() {
    let Some(ndi) = crate::test_support::ndi() else {
        return;
    };
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(crate::Source::new(""))).unwrap();
    // A key, each of the three mouse buttons, and a touch, independently held.
    for held in 0u32..32 {
        // Independently refuse each held input's first release.
        for failures in 0u32..32 {
            CALLS.with_borrow_mut(|calls| *calls = Calls::default());
            let mut session = KvmSession {
                receiver: &receiver,
                kvm: &TABLE,
                keys: BTreeSet::new(),
                buttons: BTreeSet::new(),
                touching: false,
            };
            if held & 1 != 0 {
                session.key_down(0xffe1).unwrap();
            }
            let buttons = [MouseButton::Left, MouseButton::Middle, MouseButton::Right];
            for (index, button) in buttons.into_iter().enumerate() {
                if held & (2 << index) != 0 {
                    session.mouse_down(button).unwrap();
                }
            }
            if held & 16 != 0 {
                session.touch(&[[0.25, 0.5]]).unwrap();
            }
            let key_failed = held & 1 != 0 && failures & 1 != 0;
            let button_count = (held & 14).count_ones() as usize;
            let buttons_failed = (held & failures & 14).count_ones() as usize;
            let touch_failed = held & failures & 16 != 0;
            CALLS.with_borrow_mut(|calls| {
                calls.fail_key_once = failures & 1 != 0;
                calls.fail_buttons = ((failures & 14) >> 1) as usize;
                calls.fail_touch_once = failures & 16 != 0;
            });
            assert_eq!(
                session.release_all().is_err(),
                key_failed || buttons_failed > 0 || touch_failed
            );
            assert_eq!(session.keys.len(), usize::from(key_failed));
            assert_eq!(session.buttons.len(), buttons_failed);
            for (index, button) in buttons.into_iter().enumerate() {
                assert_eq!(
                    session.buttons.contains(&button),
                    held & failures & (2 << index) != 0
                );
            }
            assert_eq!(session.touching, touch_failed);
            // Exercise both explicit retry and Drop's retry.
            if held % 2 == 0 {
                session.release_all().unwrap();
            }
            drop(session);
            CALLS.with_borrow(|calls| {
                assert_eq!(
                    calls.releases,
                    usize::from(held & 1 != 0) + usize::from(key_failed)
                );
                assert_eq!(calls.mouse_releases, button_count + buttons_failed);
                assert_eq!(
                    calls.touch_finishes,
                    usize::from(held & 16 != 0) + usize::from(touch_failed)
                );
                assert!(calls
                    .instances
                    .iter()
                    .all(|&id| id == receiver.core.instance as usize));
            });
        }
    }
}

#[test]
fn restricted_views_and_capture_handles_add_no_allocations_or_native_calls() {
    let Some(ndi) = crate::test_support::ndi() else {
        return;
    };
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(crate::Source::new(""))).unwrap();
    CALLS.with_borrow_mut(|calls| *calls = Calls::default());
    let session = KvmSession {
        receiver: &receiver,
        kvm: &TABLE,
        keys: BTreeSet::new(),
        buttons: BTreeSet::new(),
        touching: false,
    };
    let (view, allocations) = crate::test_alloc::allocation_count(|| {
        let view = session.receiver();
        let _ = (view.video(), view.audio(), view.metadata(), view.ptz());
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
        assert!(receiver.video().capture_ref(invalid).is_err());
    });
    let (_, forwarded) = crate::test_alloc::allocation_count(|| {
        assert!(view.video().capture_ref(invalid).is_err());
    });
    assert_eq!(forwarded, direct);
}
