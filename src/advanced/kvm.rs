use super::ffi;
use crate::{advanced::ffi::native::NDIlib_recv_instance_t, Receiver, ReceiverMode, Result};
use std::{
    collections::BTreeSet,
    ffi::{c_char, CString},
};

type Button = unsafe extern "C" fn(NDIlib_recv_instance_t) -> bool;
type Wheel = unsafe extern "C" fn(NDIlib_recv_instance_t, f32) -> bool;
type Key = unsafe extern "C" fn(NDIlib_recv_instance_t, i32) -> bool;
type Position = unsafe extern "C" fn(NDIlib_recv_instance_t, *const f32) -> bool;
type Clipboard = unsafe extern "C" fn(NDIlib_recv_instance_t, *const c_char) -> bool;
type Touch = unsafe extern "C" fn(NDIlib_recv_instance_t, i32, *const f32) -> bool;

/// A mouse button whose down/up state is tracked for best-effort cleanup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// Explicit exclusive remote-control session. It never reads the local clipboard.
/// Drop attempts to release pressed inputs, but cannot guarantee delivery after disconnect.
pub struct KvmSession<'a, M: ReceiverMode> {
    receiver: &'a mut Receiver<M>,
    supported: Button,
    down: [Button; 3],
    up: [Button; 3],
    wheel: [Wheel; 2],
    position: Position,
    clipboard: Clipboard,
    touch: Touch,
    finish_touch: Button,
    key_down: Key,
    key_up: Key,
    keys: BTreeSet<i32>,
    buttons: BTreeSet<MouseButton>,
    touching: bool,
}

impl<M: ReceiverMode> Receiver<M> {
    /// Resolve the complete KVM control group before any input can be pressed.
    pub fn kvm(&mut self) -> Result<KvmSession<'_, M>> {
        let ndi = &self.core._ndi;
        let session = KvmSession {
            supported: unsafe { ffi::required(ndi, b"NDIlib_recv_kvm_is_supported\0")? },
            down: unsafe {
                [
                    ffi::required(ndi, b"NDIlib_recv_kvm_send_left_mouse_click\0")?,
                    ffi::required(ndi, b"NDIlib_recv_kvm_send_middle_mouse_click\0")?,
                    ffi::required(ndi, b"NDIlib_recv_kvm_send_right_mouse_click\0")?,
                ]
            },
            up: unsafe {
                [
                    ffi::required(ndi, b"NDIlib_recv_kvm_send_left_mouse_release\0")?,
                    ffi::required(ndi, b"NDIlib_recv_kvm_send_middle_mouse_release\0")?,
                    ffi::required(ndi, b"NDIlib_recv_kvm_send_right_mouse_release\0")?,
                ]
            },
            wheel: unsafe {
                [
                    ffi::required(ndi, b"NDIlib_recv_kvm_send_vertical_mouse_wheel\0")?,
                    ffi::required(ndi, b"NDIlib_recv_kvm_send_horizontal_mouse_wheel\0")?,
                ]
            },
            position: unsafe { ffi::required(ndi, b"NDIlib_recv_kvm_send_mouse_position\0")? },
            clipboard: unsafe { ffi::required(ndi, b"NDIlib_recv_kvm_send_clipboard_contents\0")? },
            touch: unsafe { ffi::required(ndi, b"NDIlib_recv_kvm_send_touch_positions\0")? },
            finish_touch: unsafe { ffi::required(ndi, b"NDIlib_recv_kvm_send_touch_finish\0")? },
            key_down: unsafe { ffi::required(ndi, b"NDIlib_recv_kvm_send_keyboard_press\0")? },
            key_up: unsafe { ffi::required(ndi, b"NDIlib_recv_kvm_send_keyboard_release\0")? },
            receiver: self,
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            touching: false,
        };
        Ok(session)
    }
}

impl<M: ReceiverMode> KvmSession<'_, M> {
    fn instance(&self) -> NDIlib_recv_instance_t {
        self.receiver.core.instance
    }
    pub fn is_supported(&self) -> bool {
        unsafe { (self.supported)(self.instance()) }
    }
    pub fn mouse_down(&mut self, button: MouseButton) -> bool {
        let result = unsafe { (self.down[button as usize])(self.instance()) };
        if result {
            self.buttons.insert(button);
        }
        result
    }
    pub fn mouse_up(&mut self, button: MouseButton) -> bool {
        let result = unsafe { (self.up[button as usize])(self.instance()) };
        if result {
            self.buttons.remove(&button);
        }
        result
    }
    pub fn key_down(&mut self, keysym: u32) -> Result<bool> {
        let keysym =
            i32::try_from(keysym).map_err(|_| ffi::invalid("KVM key symbol exceeds SDK range"))?;
        let result = unsafe { (self.key_down)(self.instance(), keysym) };
        if result {
            self.keys.insert(keysym);
        }
        Ok(result)
    }
    pub fn key_up(&mut self, keysym: u32) -> Result<bool> {
        let keysym =
            i32::try_from(keysym).map_err(|_| ffi::invalid("KVM key symbol exceeds SDK range"))?;
        let result = unsafe { (self.key_up)(self.instance(), keysym) };
        if result {
            self.keys.remove(&keysym);
        }
        Ok(result)
    }
    pub fn wheel(&mut self, horizontal: bool, units: f32) -> Result<bool> {
        if !units.is_finite() {
            return Err(ffi::invalid("KVM wheel units must be finite"));
        }
        Ok(unsafe { (self.wheel[usize::from(horizontal)])(self.instance(), units) })
    }
    pub fn move_pointer(&mut self, x: f32, y: f32) -> Result<bool> {
        validate_position(x, y)?;
        Ok(unsafe { (self.position)(self.instance(), [x, y].as_ptr()) })
    }
    pub fn set_clipboard_text(&mut self, text: &str) -> Result<bool> {
        let text = CString::new(text)?;
        Ok(unsafe { (self.clipboard)(self.instance(), text.as_ptr()) })
    }
    pub fn touch(&mut self, points: &[[f32; 2]]) -> Result<bool> {
        let count =
            i32::try_from(points.len()).map_err(|_| ffi::invalid("Too many KVM touch points"))?;
        for [x, y] in points {
            validate_position(*x, *y)?;
        }
        let result = unsafe { (self.touch)(self.instance(), count, points.as_ptr().cast()) };
        if result {
            self.touching = !points.is_empty();
        }
        Ok(result)
    }
    pub fn finish_touch(&mut self) -> bool {
        let result = unsafe { (self.finish_touch)(self.instance()) };
        if result {
            self.touching = false;
        }
        result
    }
    /// Release every tracked input; false means one or more sends were rejected.
    pub fn release_all(&mut self) -> bool {
        let mut succeeded = true;
        for key in self.keys.clone() {
            succeeded &= self.key_up(key as u32).unwrap_or(false);
        }
        for button in self.buttons.clone() {
            succeeded &= self.mouse_up(button);
        }
        if self.touching {
            succeeded &= self.finish_touch();
        }
        succeeded
    }
}
impl<M: ReceiverMode> Drop for KvmSession<'_, M> {
    fn drop(&mut self) {
        self.release_all();
    }
}

fn validate_position(x: f32, y: f32) -> Result<()> {
    if !x.is_finite() || !y.is_finite() || !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        Err(ffi::invalid("KVM position must be finite and within 0..=1"))
    } else {
        Ok(())
    }
}

#[cfg(ndi_has_advanced_header)]
const _: () = {
    let _: Button = crate::sys::advanced_declarations::NDIlib_recv_kvm_is_supported;
    let _: Button = crate::sys::advanced_declarations::NDIlib_recv_kvm_send_left_mouse_click;
    let _: Button = crate::sys::advanced_declarations::NDIlib_recv_kvm_send_middle_mouse_click;
    let _: Button = crate::sys::advanced_declarations::NDIlib_recv_kvm_send_right_mouse_click;
    let _: Button = crate::sys::advanced_declarations::NDIlib_recv_kvm_send_left_mouse_release;
    let _: Button = crate::sys::advanced_declarations::NDIlib_recv_kvm_send_middle_mouse_release;
    let _: Button = crate::sys::advanced_declarations::NDIlib_recv_kvm_send_right_mouse_release;
    let _: Button = crate::sys::advanced_declarations::NDIlib_recv_kvm_send_touch_finish;
    let _: Wheel = crate::sys::advanced_declarations::NDIlib_recv_kvm_send_vertical_mouse_wheel;
    let _: Wheel = crate::sys::advanced_declarations::NDIlib_recv_kvm_send_horizontal_mouse_wheel;
    let _: Key = crate::sys::advanced_declarations::NDIlib_recv_kvm_send_keyboard_press;
    let _: Key = crate::sys::advanced_declarations::NDIlib_recv_kvm_send_keyboard_release;
    let _: Position = crate::sys::advanced_declarations::NDIlib_recv_kvm_send_mouse_position;
    let _: Clipboard = crate::sys::advanced_declarations::NDIlib_recv_kvm_send_clipboard_contents;
    let _: Touch = crate::sys::advanced_declarations::NDIlib_recv_kvm_send_touch_positions;
};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn positions_reject_nan_and_outside_range() {
        assert!(validate_position(0.0, 1.0).is_ok());
        assert!(validate_position(f32::NAN, 0.0).is_err());
        assert!(validate_position(0.0, -0.1).is_err());
    }
}
