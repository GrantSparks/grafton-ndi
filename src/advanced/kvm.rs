//! Remote keyboard, mouse, touch and clipboard control of a source.

#[cfg(ndi_has_advanced_kvm)]
use std::ffi::CString;
use std::{collections::BTreeSet, time::Duration};

use super::{invalid_input, PacketAttached, PacketItem, PacketReceiver};
#[cfg(ndi_has_advanced_kvm)]
use crate::sys::bindings::NDIlib_recv_instance_t;
#[cfg(any(doc, test, ndi_has_advanced_kvm))]
use crate::Error;
use crate::{
    sys::advanced::{native, Kvm},
    Receiver, ReceiverMode, Result,
};

/// A mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MouseButton {
    /// The left button.
    Left,
    /// The middle button.
    Middle,
    /// The right button.
    Right,
}

/// A wheel axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WheelAxis {
    /// The usual scroll direction.
    Vertical,
    /// Sideways scrolling.
    Horizontal,
}

/// Exclusive remote control of the source a receiver is connected to.
///
/// Created by [`Receiver::kvm`], which borrows the receiver mutably, so a
/// receiver has at most one session at a time. The session lends the receiver
/// back as a shared reference through [`receiver`](Self::receiver), so frames
/// can be captured, and held, while keys, buttons and touches stay pressed
/// across them. The lent receiver offers what any `&Receiver` offers: capture,
/// tally, PTZ, statistics, metadata and reconnecting. For a
/// [`PacketReceiver`](super::PacketReceiver), which has no frame capture of its
/// own, [`KvmSession::capture`] on a session from `packets.controls().kvm()`
/// delivers its packets and events.
///
/// A source change through the lent receiver (`connect_to`, `reconnect` or
/// `disconnect`) leaves whatever the session holds pressed on the previous
/// source, and the session cannot tell it to let go once the receiver has moved
/// on. Call [`release_all`](Self::release_all) before changing source.
///
/// The session never reads the local clipboard: it sends only the text it is
/// given. It is `Send` and `Sync`, as the receiver is; its input methods take
/// `&mut self`, so one thread at a time sends input.
///
/// Every method that sends a message returns
/// [`Error::NativeOperationFailed`], naming the SDK function, when the SDK did
/// not send it. Success is not a confirmation that the source acted on the
/// message. The session remembers which keys, buttons and touches it has
/// pressed, and dropping it releases them on a best-effort basis; call
/// [`release_all`](Self::release_all) to learn whether that succeeded.
pub struct KvmSession<'a, M: ReceiverMode> {
    receiver: &'a Receiver<M>,
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
    /// [`Error::UnsupportedCapability`]
    /// without an Advanced runtime (or in a build without the Advanced SDK).
    ///
    /// The receiver stays borrowed for the session, so only one exists at a
    /// time; use [`KvmSession::receiver`] to keep using it meanwhile.
    pub fn kvm(&mut self) -> Result<KvmSession<'_, M>> {
        Ok(KvmSession {
            kvm: self.core._ndi.table()?,
            receiver: &*self,
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            touching: false,
        })
    }
}

/// Send one message to the session `$session`'s source through the KVM entry
/// point `$func`, with `$arg`s after the receiver instance.
///
/// `Processing.NDI.KVM.h` describes each of these functions as sending a
/// message to the source the receiver is connected to, and gives its result
/// no other meaning, so `false` is a message the SDK did not send:
/// [`Error::NativeOperationFailed`] naming `$func`.
#[cfg(ndi_has_advanced_kvm)]
macro_rules! kvm_send {
    ($session:expr, $func:ident $(, $arg:expr)*) => {{
        // SAFETY: the receiver instance is live for the session's shared
        // borrow, and every argument outlives this synchronous call. Like
        // PTZ controls, this call may overlap shared use of the receiver.
        let sent = unsafe { $session.kvm.$func($session.instance() $(, $arg)*) };
        if sent {
            Ok(())
        } else {
            Err(Error::NativeOperationFailed { operation: stringify!($func) })
        }
    }};
}

fn key_symbol(keysym: u32) -> Result<i32> {
    i32::try_from(keysym).map_err(|_| {
        invalid_input(format!(
            "KVM key symbol {keysym:#x} exceeds the SDK's range, 0..={:#x}",
            i32::MAX
        ))
    })
}

/// Positions are fractions of the source's screen: `0.0..=1.0` on each axis.
#[cfg(any(test, ndi_has_advanced_kvm))]
fn validate_position(x: f32, y: f32) -> Result<()> {
    let in_range = |value: f32| (0.0..=1.0).contains(&value);
    if in_range(x) && in_range(y) {
        Ok(())
    } else {
        Err(invalid_input(format!(
            "KVM position ({x}, {y}) is not within 0..=1 on each axis"
        )))
    }
}

impl<'a, M: ReceiverMode> KvmSession<'a, M> {
    /// The receiver, lent for the session's whole lifetime.
    ///
    /// A frame captured through it can be held while the session's input
    /// methods run, which is how a key, button or touch stays down across
    /// captured frames. Changing the source through it is covered in the
    /// [type documentation](KvmSession).
    pub fn receiver(&self) -> &'a Receiver<M> {
        self.receiver
    }

    #[cfg(ndi_has_advanced_kvm)]
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
    ///
    /// # Errors
    ///
    /// [`Error::NativeOperationFailed`] when the SDK did not send the press.
    pub fn mouse_down(&mut self, _button: MouseButton) -> Result<()> {
        native!(Kvm, self.kvm => {
            match _button {
                MouseButton::Left => kvm_send!(self, NDIlib_recv_kvm_send_left_mouse_click),
                MouseButton::Middle => kvm_send!(self, NDIlib_recv_kvm_send_middle_mouse_click),
                MouseButton::Right => kvm_send!(self, NDIlib_recv_kvm_send_right_mouse_click),
            }?;
            self.buttons.insert(_button);
            Ok(())
        })
    }

    /// Release a mouse button.
    ///
    /// # Errors
    ///
    /// [`Error::NativeOperationFailed`] when the SDK did not send the release;
    /// the button stays recorded as pressed.
    pub fn mouse_up(&mut self, _button: MouseButton) -> Result<()> {
        native!(Kvm, self.kvm => {
            match _button {
                MouseButton::Left => kvm_send!(self, NDIlib_recv_kvm_send_left_mouse_release),
                MouseButton::Middle => kvm_send!(self, NDIlib_recv_kvm_send_middle_mouse_release),
                MouseButton::Right => kvm_send!(self, NDIlib_recv_kvm_send_right_mouse_release),
            }?;
            self.buttons.remove(&_button);
            Ok(())
        })
    }

    /// Press a key, identified by its X11 key symbol.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when the symbol is outside the range the SDK
    /// accepts, and [`Error::NativeOperationFailed`] when the SDK did not
    /// send the press.
    pub fn key_down(&mut self, keysym: u32) -> Result<()> {
        let key = key_symbol(keysym)?;
        self.press(key)
    }

    /// Release a key, identified by its X11 key symbol.
    ///
    /// # Errors
    ///
    /// As for [`key_down`](Self::key_down); a key whose release the SDK did
    /// not send stays recorded as pressed.
    pub fn key_up(&mut self, keysym: u32) -> Result<()> {
        let key = key_symbol(keysym)?;
        self.release(key)
    }

    fn press(&mut self, _key: i32) -> Result<()> {
        native!(Kvm, self.kvm => {
            kvm_send!(self, NDIlib_recv_kvm_send_keyboard_press, _key)?;
            self.keys.insert(_key);
            Ok(())
        })
    }

    fn release(&mut self, _key: i32) -> Result<()> {
        native!(Kvm, self.kvm => {
            kvm_send!(self, NDIlib_recv_kvm_send_keyboard_release, _key)?;
            self.keys.remove(&_key);
            Ok(())
        })
    }

    /// Turn a mouse wheel by `units` notches; the sign gives the direction.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `units` is not finite, and
    /// [`Error::NativeOperationFailed`] when the SDK did not send the turn.
    pub fn wheel(&mut self, _axis: WheelAxis, _units: f32) -> Result<()> {
        native!(Kvm, self.kvm => {
            if !_units.is_finite() {
                return Err(invalid_input(format!(
                    "KVM wheel units {_units} are not finite"
                )));
            }
            match _axis {
                WheelAxis::Vertical => {
                    kvm_send!(self, NDIlib_recv_kvm_send_vertical_mouse_wheel, _units)
                }
                WheelAxis::Horizontal => {
                    kvm_send!(self, NDIlib_recv_kvm_send_horizontal_mouse_wheel, _units)
                }
            }
        })
    }

    /// Move the pointer to `(x, y)`, each a fraction of the source's screen
    /// from `0.0` (left, top) to `1.0` (right, bottom).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when a coordinate is outside `0.0..=1.0`, and
    /// [`Error::NativeOperationFailed`] when the SDK did not send the move.
    pub fn move_pointer(&mut self, _x: f32, _y: f32) -> Result<()> {
        native!(Kvm, self.kvm => {
            validate_position(_x, _y)?;
            // The SDK reads the two coordinates through the pointer.
            let position = [_x, _y];
            kvm_send!(
                self,
                NDIlib_recv_kvm_send_mouse_position,
                position.as_ptr()
            )
        })
    }

    /// Replace the source's clipboard text.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidCString`] when `text` contains a NUL, and
    /// [`Error::NativeOperationFailed`] when the SDK did not send the text.
    pub fn set_clipboard_text(&mut self, _text: &str) -> Result<()> {
        native!(Kvm, self.kvm => {
            let text = CString::new(_text)?;
            kvm_send!(
                self,
                NDIlib_recv_kvm_send_clipboard_contents,
                text.as_ptr()
            )
        })
    }

    /// Report the current touch points, each an `[x, y]` fraction of the
    /// source's screen. An empty slice lifts every touch.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when a coordinate is outside `0.0..=1.0` or
    /// there are more points than the SDK can count, and
    /// [`Error::NativeOperationFailed`] when the SDK did not send them.
    pub fn touch(&mut self, _points: &[[f32; 2]]) -> Result<()> {
        native!(Kvm, self.kvm => {
            let count = i32::try_from(_points.len()).map_err(|_| {
                invalid_input(format!(
                    "{} KVM touch points are more than the SDK can count",
                    _points.len()
                ))
            })?;
            for [x, y] in _points {
                validate_position(*x, *y)?;
            }
            // The SDK reads `count` consecutive coordinate pairs.
            kvm_send!(
                self,
                NDIlib_recv_kvm_send_touch_positions,
                count,
                _points.as_ptr().cast()
            )?;
            self.touching = !_points.is_empty();
            Ok(())
        })
    }

    /// Lift every touch.
    ///
    /// # Errors
    ///
    /// [`Error::NativeOperationFailed`] when the SDK did not send the lift;
    /// the touches stay recorded.
    pub fn finish_touch(&mut self) -> Result<()> {
        native!(Kvm, self.kvm => {
            kvm_send!(self, NDIlib_recv_kvm_send_touch_finish)?;
            self.touching = false;
            Ok(())
        })
    }

    /// Release every key, button and touch this session has pressed.
    ///
    /// # Errors
    ///
    /// [`Error::NativeOperationFailed`] for the first release the SDK did not
    /// send. Every release is still attempted; those the SDK did not send
    /// stay recorded as pressed, and a later call tries them again.
    pub fn release_all(&mut self) -> Result<()> {
        let mut released = Ok(());
        for key in std::mem::take(&mut self.keys) {
            if let Err(error) = self.release(key) {
                self.keys.insert(key);
                released = released.and(Err(error));
            }
        }
        for button in std::mem::take(&mut self.buttons) {
            if let Err(error) = self.mouse_up(button) {
                self.buttons.insert(button);
                released = released.and(Err(error));
            }
        }
        if self.touching {
            released = released.and(self.finish_touch());
        }
        released
    }
}

impl KvmSession<'_, PacketAttached> {
    /// Wait up to `timeout` for the next frame or event of a
    /// [`PacketReceiver`], exactly as [`PacketReceiver::capture`] does, while
    /// the session holds its inputs.
    ///
    /// Every result owns its data, so nothing it returns keeps the session
    /// borrowed. It takes `&mut self`, as the packet receiver's does, so one
    /// capture runs at a time.
    ///
    /// # Errors
    ///
    /// As for [`PacketReceiver::capture`].
    pub fn capture(&mut self, timeout: Duration) -> Result<Option<PacketItem>> {
        PacketReceiver::capture_from(self.receiver, timeout)
    }
}

impl<M: ReceiverMode> Drop for KvmSession<'_, M> {
    fn drop(&mut self) {
        // Best effort: a source that has disconnected cannot be told.
        let _ = self.release_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_reject_nan_and_outside_range() {
        assert!(validate_position(0.0, 1.0).is_ok());
        for (x, y) in [
            (f32::NAN, 0.0),
            (0.0, -0.1),
            (1.1, 0.5),
            (f32::INFINITY, 0.5),
        ] {
            assert!(matches!(
                validate_position(x, y),
                Err(Error::InvalidInput(_))
            ));
        }
    }

    #[test]
    fn key_symbols_beyond_the_native_range_are_rejected() {
        assert_eq!(key_symbol(0xff0d).unwrap(), 0xff0d);
        assert!(matches!(key_symbol(u32::MAX), Err(Error::InvalidInput(_))));
    }
}

#[cfg(test)]
crate::sys::advanced::declared! { Kvm {
    #[path = "kvm_tests.rs"]
    mod ownership_tests;
} else {} }
