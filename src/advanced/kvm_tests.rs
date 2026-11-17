//! Input ownership at the native boundary, including failed release and drop.

use super::*;
use crate::sys::bindings::NDIlib_recv_instance_t;
use std::cell::RefCell;
#[cfg(ndi_has_advanced_kvm)]
use std::ffi::CStr;

#[derive(Default)]
pub(super) struct Calls {
    pub(super) instances: Vec<usize>,
    keys: Vec<i32>,
    releases: usize,
    touches: Vec<f32>,
    touch_updates: usize,
    touch_finishes: usize,
    mouse_releases: usize,
    #[cfg(ndi_has_advanced_kvm)]
    pub(super) clipboard: String,
    fail_key_once: bool,
    fail_buttons: usize,
    fail_touch_once: bool,
}
thread_local! { pub(super) static CALLS: RefCell<Calls> = RefCell::default(); }

fn record(instance: NDIlib_recv_instance_t) {
    CALLS.with_borrow_mut(|calls| calls.instances.push(instance as usize));
}

#[cfg(ndi_has_advanced_kvm)]
unsafe extern "C" fn accept(instance: NDIlib_recv_instance_t) -> bool {
    record(instance);
    true
}
#[cfg(ndi_has_advanced_kvm)]
unsafe extern "C" fn scalar(instance: NDIlib_recv_instance_t, _: f32) -> bool {
    record(instance);
    true
}
#[cfg(ndi_has_advanced_kvm)]
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
    CALLS.with_borrow_mut(|calls| {
        calls.touch_updates += 1;
        calls.touches = points.to_vec();
    });
    true
}
unsafe extern "C" fn finish_touch(instance: NDIlib_recv_instance_t) -> bool {
    record(instance);
    CALLS.with_borrow_mut(|calls| {
        calls.touch_finishes += 1;
        !std::mem::take(&mut calls.fail_touch_once)
    })
}
#[cfg(ndi_has_advanced_kvm)]
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

#[cfg(ndi_has_advanced_kvm)]
pub(super) static TABLE: Kvm = Kvm {
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

/// The same callbacks are usable without an Advanced SDK declaration table.
#[cfg(not(ndi_has_advanced_kvm))]
struct TestSender;

#[cfg(not(ndi_has_advanced_kvm))]
impl InputSender for TestSender {
    fn key(&self, connection: &Control<'_>, value: i32, down: bool) -> Result<()> {
        let (accepted, operation) = if down {
            // SAFETY: these synthetic callbacks only record their arguments.
            (
                unsafe { key(connection.instance(), value) },
                c"NDIlib_recv_kvm_send_keyboard_press",
            )
        } else {
            (
                unsafe { release(connection.instance(), value) },
                c"NDIlib_recv_kvm_send_keyboard_release",
            )
        };
        crate::sys::table::for_test(accepted, operation).performed()
    }

    fn mouse(&self, connection: &Control<'_>, button: MouseButton, down: bool) -> Result<()> {
        if down {
            record(connection.instance());
            return Ok(());
        }
        let (accepted, operation) = match button {
            // SAFETY: these synthetic callbacks only record their arguments.
            MouseButton::Left => (
                unsafe { left_release(connection.instance()) },
                c"NDIlib_recv_kvm_send_left_mouse_release",
            ),
            MouseButton::Middle => (
                unsafe { middle_release(connection.instance()) },
                c"NDIlib_recv_kvm_send_middle_mouse_release",
            ),
            MouseButton::Right => (
                unsafe { right_release(connection.instance()) },
                c"NDIlib_recv_kvm_send_right_mouse_release",
            ),
        };
        crate::sys::table::for_test(accepted, operation).performed()
    }

    fn touch(&self, connection: &Control<'_>, count: i32, points: &[[f32; 2]]) -> Result<()> {
        // SAFETY: HeldInputs lends this validated count of initialized pairs.
        let accepted = unsafe { touch(connection.instance(), count, points.as_ptr().cast()) };
        crate::sys::table::for_test(accepted, c"NDIlib_recv_kvm_send_touch_positions").performed()
    }

    fn finish_touch(&self, connection: &Control<'_>) -> Result<()> {
        // SAFETY: the synthetic callback only records its instance argument.
        let accepted = unsafe { finish_touch(connection.instance()) };
        crate::sys::table::for_test(accepted, c"NDIlib_recv_kvm_send_touch_finish").performed()
    }
}

// With declarations, these same pure tests exercise the production table's
// dispatch too; without them, they still test every held-input transition.
fn sender() -> &'static impl InputSender {
    #[cfg(ndi_has_advanced_kvm)]
    {
        &TABLE
    }
    #[cfg(not(ndi_has_advanced_kvm))]
    {
        &TestSender
    }
}

struct TestSession {
    instance: NDIlib_recv_instance_t,
    held: HeldInputs,
}

impl TestSession {
    fn new(instance: NDIlib_recv_instance_t) -> Self {
        Self {
            instance,
            held: HeldInputs::default(),
        }
    }

    fn key_down(&mut self, keysym: u32) -> Result<()> {
        Control::with_test_instance(self.instance, |connection| {
            self.held
                .key(connection, sender(), key_symbol(keysym)?, true)
        })
    }

    fn mouse_down(&mut self, button: MouseButton) -> Result<()> {
        Control::with_test_instance(self.instance, |connection| {
            self.held.mouse(connection, sender(), button, true)
        })
    }

    fn touch(&mut self, points: &[[f32; 2]]) -> Result<()> {
        Control::with_test_instance(self.instance, |connection| {
            self.held.touch(connection, sender(), points)
        })
    }

    fn release_all(&mut self) -> Result<()> {
        Control::with_test_instance(self.instance, |connection| {
            self.held.release_all(connection, sender())
        })
    }
}

impl Drop for TestSession {
    fn drop(&mut self) {
        Control::with_test_instance(self.instance, |connection| {
            self.held.release_on_drop(connection, sender());
        });
    }
}

#[test]
fn empty_touch_finishes_and_failed_lifts_are_retried() {
    let instance_storage = 0u8;
    let instance = std::ptr::from_ref(&instance_storage).cast_mut().cast();
    for retry_on_drop in [false, true] {
        CALLS.with_borrow_mut(|calls| *calls = Calls::default());
        let mut session = TestSession::new(instance);
        session.touch(&[]).unwrap();
        assert!(!session.held.touching);
        session.touch(&[[0.25, 0.5]]).unwrap();
        CALLS.with_borrow_mut(|calls| calls.fail_touch_once = true);
        assert!(matches!(
            session.touch(&[]),
            Err(Error::NativeOperationFailed {
                operation}) if operation.name() == c"NDIlib_recv_kvm_send_touch_finish"
        ));
        assert!(session.held.touching);
        if !retry_on_drop {
            session.release_all().unwrap();
            assert!(!session.held.touching);
        }
        drop(session);
        CALLS.with_borrow(|calls| {
            assert_eq!(calls.touch_updates, 1);
            assert_eq!(calls.touches, [0.25, 0.5]);
            assert_eq!(calls.touch_finishes, 3);
        });
    }
}

#[test]
fn rejected_releases_are_retained_and_drop_retries_only_the_inputs_still_pressed() {
    let instance_storage = 0u8;
    let instance = std::ptr::from_ref(&instance_storage).cast_mut().cast();
    CALLS.with_borrow_mut(|calls| {
        *calls = Calls {
            fail_key_once: true,
            ..Default::default()
        };
    });
    let mut session = TestSession::new(instance);
    session.key_down(0xff0d).unwrap();
    session.mouse_down(MouseButton::Left).unwrap();
    session.touch(&[[0.25, 0.5], [0.75, 1.0]]).unwrap();
    // The first key release is refused: the session says which function,
    // keeps the key, and still releases the rest.
    assert!(matches!(
        session.release_all(),
        Err(Error::NativeOperationFailed {
            operation}) if operation.name() == c"NDIlib_recv_kvm_send_keyboard_release"
    ));
    assert_eq!(session.held.keys.len(), 1);
    assert!(session.held.buttons.is_empty());
    assert!(!session.held.touching);
    drop(session);
    CALLS.with_borrow(|calls| {
        assert!(calls.instances.iter().all(|&id| id == instance as usize));
        assert_eq!(calls.keys, [0xff0d]);
        assert_eq!(calls.releases, 2);
        assert_eq!(calls.mouse_releases, 1);
        assert_eq!(calls.touch_finishes, 1);
        assert_eq!(calls.touches, [0.25, 0.5, 0.75, 1.0]);
    });
}

#[test]
fn every_held_input_combination_retries_only_failed_releases() {
    let instance_storage = 0u8;
    let instance = std::ptr::from_ref(&instance_storage).cast_mut().cast();
    // A key, each of the three mouse buttons, and a touch, independently held.
    for held in 0u32..32 {
        // Independently refuse each held input's first release.
        for failures in 0u32..32 {
            CALLS.with_borrow_mut(|calls| *calls = Calls::default());
            let mut session = TestSession::new(instance);
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
            assert_eq!(session.held.keys.len(), usize::from(key_failed));
            assert_eq!(session.held.buttons.len(), buttons_failed);
            for (index, button) in buttons.into_iter().enumerate() {
                assert_eq!(
                    session.held.buttons.contains(&button),
                    held & failures & (2 << index) != 0
                );
            }
            assert_eq!(session.held.touching, touch_failed);
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
                assert!(calls.instances.iter().all(|&id| id == instance as usize));
            });
        }
    }
}
