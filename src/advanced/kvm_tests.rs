//! Input ownership at the native boundary, including failed release and drop.

use super::*;
use crate::{sys::advanced::Kvm, ReceiverOptions};
use std::{cell::RefCell, ffi::CStr};

#[derive(Default)]
struct Calls {
    keys: Vec<i32>,
    releases: usize,
    touches: Vec<f32>,
    touch_finishes: usize,
    mouse_releases: usize,
    clipboard: String,
}
thread_local! { static CALLS: RefCell<Calls> = RefCell::default(); }

unsafe extern "C" fn accept(_: NDIlib_recv_instance_t) -> bool {
    true
}
unsafe extern "C" fn scalar(_: NDIlib_recv_instance_t, _: f32) -> bool {
    true
}
unsafe extern "C" fn position(_: NDIlib_recv_instance_t, _: *const f32) -> bool {
    true
}
unsafe extern "C" fn key(_: NDIlib_recv_instance_t, value: i32) -> bool {
    CALLS.with_borrow_mut(|calls| calls.keys.push(value));
    true
}
unsafe extern "C" fn release(_: NDIlib_recv_instance_t, _: i32) -> bool {
    CALLS.with_borrow_mut(|calls| {
        calls.releases += 1;
        calls.releases > 1
    })
}
unsafe extern "C" fn mouse_release(_: NDIlib_recv_instance_t) -> bool {
    CALLS.with_borrow_mut(|calls| calls.mouse_releases += 1);
    true
}
unsafe extern "C" fn touch(_: NDIlib_recv_instance_t, count: i32, points: *const f32) -> bool {
    // SAFETY: the wrapper lends `count` coordinate pairs for this call.
    let points = unsafe { std::slice::from_raw_parts(points, count as usize * 2) };
    CALLS.with_borrow_mut(|calls| calls.touches = points.to_vec());
    true
}
unsafe extern "C" fn finish_touch(_: NDIlib_recv_instance_t) -> bool {
    CALLS.with_borrow_mut(|calls| calls.touch_finishes += 1);
    true
}
unsafe extern "C" fn clipboard(_: NDIlib_recv_instance_t, value: *const std::ffi::c_char) -> bool {
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
    NDIlib_recv_kvm_send_left_mouse_release: mouse_release,
    NDIlib_recv_kvm_send_middle_mouse_release: mouse_release,
    NDIlib_recv_kvm_send_right_mouse_release: mouse_release,
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
    let mut receiver = Receiver::new(&ndi, &ReceiverOptions::new(crate::Source::new(""))).unwrap();
    CALLS.with_borrow_mut(|calls| *calls = Calls::default());
    let mut session = KvmSession {
        receiver: &mut receiver,
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
        assert_eq!(calls.keys, [0xff0d]);
        assert_eq!(calls.releases, 2);
        assert_eq!(calls.mouse_releases, 1);
        assert_eq!(calls.touch_finishes, 1);
        assert_eq!(calls.touches, [0.25, 0.5, 0.75, 1.0]);
        assert_eq!(calls.clipboard, "clipboard é");
    });
}
