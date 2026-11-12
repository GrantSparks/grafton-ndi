//! Remote keyboard, mouse, touch and clipboard control of a source.

use std::{collections::BTreeSet, ffi::CString};

use super::invalid;
use crate::{
    sys::{
        advanced::{native, Kvm},
        bindings::NDIlib_recv_instance_t,
    },
    Receiver, ReceiverMode, Result,
};

/// A mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MouseButton {
    /// The left button.
    Left,
    /// The middle button.
    Middle,
    /// The right button.
    Right,
}

/// A wheel axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelAxis {
    /// The usual scroll direction.
    Vertical,
    /// Sideways scrolling.
    Horizontal,
}

/// Exclusive remote control of the source a receiver is connected to.
///
/// Created by [`Receiver::kvm`]; the receiver is borrowed mutably for the
/// session, so nothing else can use it meanwhile. The session never reads the
/// local clipboard: it sends only the text it is given.
///
/// Every send method returns whether the SDK accepted the message, which is
/// not a confirmation that the source acted on it. The session remembers which
/// keys, buttons and touches it has pressed, and dropping it releases them on
/// a best-effort basis; call [`release_all`](Self::release_all) to learn
/// whether that succeeded.
pub struct KvmSession<'a, M: ReceiverMode> {
    receiver: &'a mut Receiver<M>,
    kvm: &'static Kvm,
    keys: BTreeSet<i32>,
    buttons: BTreeSet<MouseButton>,
    touching: bool,
}

impl<M: ReceiverMode> Receiver<M> {
    /// Start a remote-control session with the connected source.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`](crate::Error::UnsupportedCapability)
    /// without an Advanced runtime (or in a build without the Advanced SDK).
    pub fn kvm(&mut self) -> Result<KvmSession<'_, M>> {
        Ok(KvmSession {
            kvm: self.core._ndi.table()?,
            receiver: self,
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            touching: false,
        })
    }
}

fn key_symbol(keysym: u32) -> Result<i32> {
    i32::try_from(keysym).map_err(|_| invalid("KVM key symbol exceeds SDK range"))
}

/// Positions are fractions of the source's screen: `0.0..=1.0` on each axis.
fn validate_position(x: f32, y: f32) -> Result<()> {
    let in_range = |value: f32| (0.0..=1.0).contains(&value);
    if in_range(x) && in_range(y) {
        Ok(())
    } else {
        Err(invalid("KVM position must be within 0..=1"))
    }
}

impl<M: ReceiverMode> KvmSession<'_, M> {
    fn instance(&self) -> NDIlib_recv_instance_t {
        self.receiver.core.instance
    }

    /// Whether the connected source accepts remote control. This can change
    /// when the receiver connects to a different source.
    pub fn is_supported(&self) -> bool {
        // SAFETY: the receiver instance is live for this borrow.
        native!(Kvm, self.kvm => unsafe { self.kvm.NDIlib_recv_kvm_is_supported(self.instance()) })
    }

    /// Press a mouse button.
    pub fn mouse_down(&mut self, button: MouseButton) -> bool {
        // SAFETY: the receiver instance is live for this borrow.
        let sent = native!(Kvm, self.kvm => unsafe {
            match button {
                MouseButton::Left => self.kvm.NDIlib_recv_kvm_send_left_mouse_click(self.instance()),
                MouseButton::Middle => {
                    self.kvm.NDIlib_recv_kvm_send_middle_mouse_click(self.instance())
                }
                MouseButton::Right => self.kvm.NDIlib_recv_kvm_send_right_mouse_click(self.instance()),
            }
        });
        if sent {
            self.buttons.insert(button);
        }
        sent
    }

    /// Release a mouse button.
    pub fn mouse_up(&mut self, button: MouseButton) -> bool {
        // SAFETY: the receiver instance is live for this borrow.
        let sent = native!(Kvm, self.kvm => unsafe {
            match button {
                MouseButton::Left => {
                    self.kvm.NDIlib_recv_kvm_send_left_mouse_release(self.instance())
                }
                MouseButton::Middle => {
                    self.kvm.NDIlib_recv_kvm_send_middle_mouse_release(self.instance())
                }
                MouseButton::Right => {
                    self.kvm.NDIlib_recv_kvm_send_right_mouse_release(self.instance())
                }
            }
        });
        if sent {
            self.buttons.remove(&button);
        }
        sent
    }

    /// Press a key, identified by its X11 key symbol.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) when the symbol is
    /// outside the range the SDK accepts.
    pub fn key_down(&mut self, keysym: u32) -> Result<bool> {
        let key = key_symbol(keysym)?;
        Ok(self.press(key))
    }

    /// Release a key, identified by its X11 key symbol.
    ///
    /// # Errors
    ///
    /// As for [`key_down`](Self::key_down).
    pub fn key_up(&mut self, keysym: u32) -> Result<bool> {
        let key = key_symbol(keysym)?;
        Ok(self.release(key))
    }

    fn press(&mut self, key: i32) -> bool {
        // SAFETY: the receiver instance is live for this borrow.
        let sent = native!(Kvm, self.kvm => unsafe {
            self.kvm.NDIlib_recv_kvm_send_keyboard_press(self.instance(), key)
        });
        if sent {
            self.keys.insert(key);
        }
        sent
    }

    fn release(&mut self, key: i32) -> bool {
        // SAFETY: the receiver instance is live for this borrow.
        let sent = native!(Kvm, self.kvm => unsafe {
            self.kvm.NDIlib_recv_kvm_send_keyboard_release(self.instance(), key)
        });
        if sent {
            self.keys.remove(&key);
        }
        sent
    }

    /// Turn a mouse wheel by `units` notches; the sign gives the direction.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) when `units` is not
    /// finite.
    pub fn wheel(&mut self, axis: WheelAxis, units: f32) -> Result<bool> {
        if !units.is_finite() {
            return Err(invalid("KVM wheel units must be finite"));
        }
        // SAFETY: the receiver instance is live for this borrow.
        Ok(native!(Kvm, self.kvm => unsafe {
            match axis {
                WheelAxis::Vertical => {
                    self.kvm.NDIlib_recv_kvm_send_vertical_mouse_wheel(self.instance(), units)
                }
                WheelAxis::Horizontal => {
                    self.kvm.NDIlib_recv_kvm_send_horizontal_mouse_wheel(self.instance(), units)
                }
            }
        }))
    }

    /// Move the pointer to `(x, y)`, each a fraction of the source's screen
    /// from `0.0` (left, top) to `1.0` (right, bottom).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) when a coordinate is
    /// outside `0.0..=1.0`.
    pub fn move_pointer(&mut self, x: f32, y: f32) -> Result<bool> {
        validate_position(x, y)?;
        let position = [x, y];
        // SAFETY: the receiver instance is live for this borrow, and
        // `position` holds the two coordinates the SDK reads.
        Ok(native!(Kvm, self.kvm => unsafe {
            self.kvm.NDIlib_recv_kvm_send_mouse_position(self.instance(), position.as_ptr())
        }))
    }

    /// Replace the source's clipboard text.
    ///
    /// # Errors
    ///
    /// An invalid-string error when `text` contains a NUL.
    pub fn set_clipboard_text(&mut self, text: &str) -> Result<bool> {
        let text = CString::new(text)?;
        // SAFETY: the receiver instance is live for this borrow, and `text`
        // outlives the call.
        Ok(native!(Kvm, self.kvm => unsafe {
            self.kvm.NDIlib_recv_kvm_send_clipboard_contents(self.instance(), text.as_ptr())
        }))
    }

    /// Report the current touch points, each an `[x, y]` fraction of the
    /// source's screen. An empty slice lifts every touch.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`](crate::Error::InvalidFrame) when a coordinate is
    /// outside `0.0..=1.0` or there are more points than the SDK can count.
    pub fn touch(&mut self, points: &[[f32; 2]]) -> Result<bool> {
        let count =
            i32::try_from(points.len()).map_err(|_| invalid("Too many KVM touch points"))?;
        for [x, y] in points {
            validate_position(*x, *y)?;
        }
        // SAFETY: the receiver instance is live for this borrow, and `points`
        // is `count` consecutive coordinate pairs.
        let sent = native!(Kvm, self.kvm => unsafe {
            self.kvm.NDIlib_recv_kvm_send_touch_positions(
                self.instance(),
                count,
                points.as_ptr().cast(),
            )
        });
        if sent {
            self.touching = !points.is_empty();
        }
        Ok(sent)
    }

    /// Lift every touch.
    pub fn finish_touch(&mut self) -> bool {
        // SAFETY: the receiver instance is live for this borrow.
        let sent = native!(Kvm, self.kvm => unsafe {
            self.kvm.NDIlib_recv_kvm_send_touch_finish(self.instance())
        });
        if sent {
            self.touching = false;
        }
        sent
    }

    /// Release every key, button and touch this session has pressed.
    ///
    /// Returns `false` when the SDK rejected at least one release; those
    /// inputs stay recorded as pressed and a later call tries them again.
    pub fn release_all(&mut self) -> bool {
        let mut released = true;
        for key in std::mem::take(&mut self.keys) {
            if !self.release(key) {
                self.keys.insert(key);
                released = false;
            }
        }
        for button in std::mem::take(&mut self.buttons) {
            if !self.mouse_up(button) {
                self.buttons.insert(button);
                released = false;
            }
        }
        if self.touching {
            released &= self.finish_touch();
        }
        released
    }
}

impl<M: ReceiverMode> Drop for KvmSession<'_, M> {
    fn drop(&mut self) {
        // Best effort: a source that has disconnected cannot be told.
        self.release_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_reject_nan_and_outside_range() {
        assert!(validate_position(0.0, 1.0).is_ok());
        assert!(validate_position(f32::NAN, 0.0).is_err());
        assert!(validate_position(0.0, -0.1).is_err());
        assert!(validate_position(1.1, 0.5).is_err());
        assert!(validate_position(f32::INFINITY, 0.5).is_err());
    }

    #[test]
    fn key_symbols_beyond_the_native_range_are_rejected() {
        assert_eq!(key_symbol(0xff0d).unwrap(), 0xff0d);
        assert!(key_symbol(u32::MAX).is_err());
    }
}

#[cfg(test)]
crate::sys::advanced::declared! { Kvm {
    #[path = "kvm_tests.rs"]
    mod ownership_tests;
} else {} }
