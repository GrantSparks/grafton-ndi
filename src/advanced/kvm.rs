//! Remote keyboard, mouse, touch and clipboard control of a source.

use std::collections::BTreeSet;
#[cfg(ndi_has_advanced_kvm)]
use std::ffi::CString;

use super::{invalid_input, KvmReceiver};
#[cfg(any(test, ndi_has_advanced_kvm, ndi_test_receiver_proofs))]
use crate::receiver::Connection;
#[cfg(any(doc, test))]
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

/// Input bookkeeping has no receiver borrow. Its table is statically dispatched
/// so the same rule can be tested when Advanced declarations are unavailable.
#[derive(Default)]
struct HeldInputs {
    keys: BTreeSet<i32>,
    buttons: BTreeSet<MouseButton>,
    touching: bool,
}

#[cfg(any(test, ndi_has_advanced_kvm, ndi_test_receiver_proofs))]
trait InputSender {
    fn key(&self, connection: &Connection<'_>, key: i32, down: bool) -> Result<()>;
    fn mouse(&self, connection: &Connection<'_>, button: MouseButton, down: bool) -> Result<()>;
    fn touch(&self, connection: &Connection<'_>, count: i32, points: &[[f32; 2]]) -> Result<()>;
    fn finish_touch(&self, connection: &Connection<'_>) -> Result<()>;
}

#[cfg(any(test, ndi_has_advanced_kvm, ndi_test_receiver_proofs))]
impl HeldInputs {
    fn key(
        &mut self,
        connection: &Connection<'_>,
        sender: &impl InputSender,
        key: i32,
        down: bool,
    ) -> Result<()> {
        sender.key(connection, key, down)?;
        if down {
            self.keys.insert(key);
        } else {
            self.keys.remove(&key);
        }
        Ok(())
    }

    fn mouse(
        &mut self,
        connection: &Connection<'_>,
        sender: &impl InputSender,
        button: MouseButton,
        down: bool,
    ) -> Result<()> {
        sender.mouse(connection, button, down)?;
        if down {
            self.buttons.insert(button);
        } else {
            self.buttons.remove(&button);
        }
        Ok(())
    }

    fn touch(
        &mut self,
        connection: &Connection<'_>,
        sender: &impl InputSender,
        points: &[[f32; 2]],
    ) -> Result<()> {
        if points.is_empty() {
            return self.finish_touch(connection, sender);
        }
        let count = i32::try_from(points.len()).map_err(|_| {
            invalid_input(crate::InputDefect::KvmTouchCount {
                count: points.len(),
            })
        })?;
        for [x, y] in points {
            validate_position(*x, *y)?;
        }
        sender.touch(connection, count, points)?;
        self.touching = true;
        Ok(())
    }

    fn finish_touch(
        &mut self,
        connection: &Connection<'_>,
        sender: &impl InputSender,
    ) -> Result<()> {
        sender.finish_touch(connection)?;
        self.touching = false;
        Ok(())
    }

    fn release_all(
        &mut self,
        connection: &Connection<'_>,
        sender: &impl InputSender,
    ) -> Result<()> {
        let mut released = Ok(());
        for key in std::mem::take(&mut self.keys) {
            if let Err(error) = self.key(connection, sender, key, false) {
                self.keys.insert(key);
                released = released.and(Err(error));
            }
        }
        for button in std::mem::take(&mut self.buttons) {
            if let Err(error) = self.mouse(connection, sender, button, false) {
                self.buttons.insert(button);
                released = released.and(Err(error));
            }
        }
        if self.touching {
            released = released.and(self.finish_touch(connection, sender));
        }
        released
    }

    fn release_on_drop(&mut self, connection: &Connection<'_>, sender: &impl InputSender) {
        // Best effort: a source that has disconnected cannot be told.
        let _ = self.release_all(connection, sender);
    }
}

/// Exclusive remote control of the source a receiver is connected to.
///
/// Created by [`Receiver::kvm`], which borrows the receiver mutably, so a
/// receiver has at most one session at a time. The session lends the receiver
/// back as a restricted [`KvmReceiver`] through [`receiver`](Self::receiver),
/// so frames can be captured, and held, while keys, buttons and touches stay pressed
/// across them. The view offers mode-permitted capture, tally, PTZ, statistics
/// and metadata, without allowing application connection changes. For a
/// packet session, `session.receiver()` delivers owned or borrowed packets,
/// decoded media and events through its mode-specific capture methods.
///
/// To change source, successfully [`release_all`](Self::release_all), end the
/// session, change the connection through its owner, then start a new session.
/// Native disconnection, Discovery Server remote control and failed release
/// delivery can still leave input pressed on the previous source: release is
/// best effort and the session cannot address a source it has lost.
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
    held: HeldInputs,
}

impl<M: ReceiverMode> std::fmt::Debug for KvmSession<'_, M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KvmSession")
            .field("mode", &std::any::type_name::<M>())
            .field("pressed_keys", &self.held.keys.len())
            .field("pressed_buttons", &self.held.buttons.len())
            .field("touching", &self.held.touching)
            .finish_non_exhaustive()
    }
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
            held: HeldInputs::default(),
        })
    }
}

/// Send one message to the session `$session`'s source through the KVM entry
/// point `$func`, with `$arg`s after the receiver connection.
///
/// `Processing.NDI.KVM.h` describes each of these functions as sending a
/// message to the source the receiver is connected to, and gives its result
/// no other meaning, so `false` is a message the SDK did not send:
/// [`Error::NativeOperationFailed`] naming `$func`.
#[cfg(ndi_has_advanced_kvm)]
macro_rules! kvm_send {
    ($session:expr, $func:ident $(, $arg:expr)*) => {{
        let connection = $session.receiver.core.lock_connection();
        kvm_send!(table $session.kvm, &connection, $func $(, $arg)*)
    }};
    (table $table:expr, $connection:expr, $func:ident $(, $arg:expr)*) => {{
        // SAFETY: the session retains the live receiver, and the caller holds
        // its connection permit (including HeldInputs cleanup). Arguments
        // outlive this synchronous call. See the RecvInstance inventory for
        // control/capture and SDK-internal connection changes.
        unsafe { $table.named().$func($connection.instance() $(, $arg)*) }.performed()
    }};
}

#[cfg(ndi_has_advanced_kvm)]
impl InputSender for Kvm {
    fn key(&self, connection: &Connection<'_>, key: i32, down: bool) -> Result<()> {
        if down {
            kvm_send!(table self, connection, NDIlib_recv_kvm_send_keyboard_press, key)
        } else {
            kvm_send!(table self, connection, NDIlib_recv_kvm_send_keyboard_release, key)
        }
    }

    fn mouse(&self, connection: &Connection<'_>, button: MouseButton, down: bool) -> Result<()> {
        match (button, down) {
            (MouseButton::Left, true) => {
                kvm_send!(table self, connection, NDIlib_recv_kvm_send_left_mouse_click)
            }
            (MouseButton::Middle, true) => {
                kvm_send!(table self, connection, NDIlib_recv_kvm_send_middle_mouse_click)
            }
            (MouseButton::Right, true) => {
                kvm_send!(table self, connection, NDIlib_recv_kvm_send_right_mouse_click)
            }
            (MouseButton::Left, false) => {
                kvm_send!(table self, connection, NDIlib_recv_kvm_send_left_mouse_release)
            }
            (MouseButton::Middle, false) => {
                kvm_send!(table self, connection, NDIlib_recv_kvm_send_middle_mouse_release)
            }
            (MouseButton::Right, false) => {
                kvm_send!(table self, connection, NDIlib_recv_kvm_send_right_mouse_release)
            }
        }
    }

    fn touch(&self, connection: &Connection<'_>, count: i32, points: &[[f32; 2]]) -> Result<()> {
        // HeldInputs validated `count` and all coordinates; the slice lends
        // that many consecutive coordinate pairs through this native call.
        kvm_send!(table self, connection, NDIlib_recv_kvm_send_touch_positions, count, points.as_ptr().cast())
    }

    fn finish_touch(&self, connection: &Connection<'_>) -> Result<()> {
        kvm_send!(table self, connection, NDIlib_recv_kvm_send_touch_finish)
    }
}

fn key_symbol(keysym: u32) -> Result<i32> {
    i32::try_from(keysym).map_err(|_| invalid_input(crate::InputDefect::KvmKeySymbol { keysym }))
}

/// Positions are fractions of the source's screen: `0.0..=1.0` on each axis.
#[cfg(any(test, ndi_has_advanced_kvm, ndi_test_receiver_proofs))]
fn validate_position(x: f32, y: f32) -> Result<()> {
    let in_range = |value: f32| (0.0..=1.0).contains(&value);
    if in_range(x) && in_range(y) {
        Ok(())
    } else {
        Err(invalid_input(crate::InputDefect::KvmPosition { x, y }))
    }
}

impl<'a, M: ReceiverMode> KvmSession<'a, M> {
    /// Restricted receiver access for the session's original receiver lifetime.
    ///
    /// A frame captured through it can be held while the session's input
    /// methods run, which is how a key, button or touch stays down across
    /// captured frames. The view cannot change the connection or recover an
    /// unrestricted receiver, even when moved to a scoped thread. Creating it
    /// allocates nothing and makes no SDK call or lock acquisition.
    pub fn receiver(&self) -> KvmReceiver<'a, M> {
        KvmReceiver::new(self.receiver)
    }

    /// Whether the connected source accepts remote control. This can change
    /// during the session when that source enables or disables remote control.
    #[doc(alias = "NDIlib_recv_kvm_is_supported")]
    pub fn is_supported(&self) -> bool {
        native!(Kvm, self.kvm => {
            let connection = self.receiver.core.lock_connection();
            // SAFETY: the live core and permit cover the call; see the
            // RecvInstance concurrency inventory.
            unsafe { self.kvm.NDIlib_recv_kvm_is_supported(connection.instance()) }
        })
    }

    /// Press a mouse button.
    ///
    /// # Errors
    ///
    /// [`Error::NativeOperationFailed`] when the SDK did not send the press.
    #[doc(alias = "NDIlib_recv_kvm_send_left_mouse_click")]
    #[doc(alias = "NDIlib_recv_kvm_send_middle_mouse_click")]
    #[doc(alias = "NDIlib_recv_kvm_send_right_mouse_click")]
    pub fn mouse_down(&mut self, button: MouseButton) -> Result<()> {
        native!(Kvm, self.kvm, uses(button) => {
            let connection = self.receiver.core.lock_connection();
            self.held.mouse(&connection, self.kvm, button, true)
        })
    }

    /// Release a mouse button.
    ///
    /// # Errors
    ///
    /// [`Error::NativeOperationFailed`] when the SDK did not send the release;
    /// the button stays recorded as pressed.
    #[doc(alias = "NDIlib_recv_kvm_send_left_mouse_release")]
    #[doc(alias = "NDIlib_recv_kvm_send_middle_mouse_release")]
    #[doc(alias = "NDIlib_recv_kvm_send_right_mouse_release")]
    pub fn mouse_up(&mut self, button: MouseButton) -> Result<()> {
        native!(Kvm, self.kvm, uses(button) => {
            let connection = self.receiver.core.lock_connection();
            self.held.mouse(&connection, self.kvm, button, false)
        })
    }

    /// Press a key, identified by its X11 key symbol.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when the symbol is outside the range the SDK
    /// accepts, and [`Error::NativeOperationFailed`] when the SDK did not
    /// send the press.
    #[doc(alias = "NDIlib_recv_kvm_send_keyboard_press")]
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
    #[doc(alias = "NDIlib_recv_kvm_send_keyboard_release")]
    pub fn key_up(&mut self, keysym: u32) -> Result<()> {
        let key = key_symbol(keysym)?;
        self.release(key)
    }

    fn press(&mut self, _key: i32) -> Result<()> {
        native!(Kvm, self.kvm => {
            let connection = self.receiver.core.lock_connection();
            self.held.key(&connection, self.kvm, _key, true)
        })
    }

    fn release(&mut self, _key: i32) -> Result<()> {
        native!(Kvm, self.kvm => {
            let connection = self.receiver.core.lock_connection();
            self.held.key(&connection, self.kvm, _key, false)
        })
    }

    /// Turn a mouse wheel by `units` notches; the sign gives the direction.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when `units` is not finite, and
    /// [`Error::NativeOperationFailed`] when the SDK did not send the turn.
    #[doc(alias = "NDIlib_recv_kvm_send_vertical_mouse_wheel")]
    #[doc(alias = "NDIlib_recv_kvm_send_horizontal_mouse_wheel")]
    pub fn wheel(&mut self, axis: WheelAxis, units: f32) -> Result<()> {
        native!(Kvm, self.kvm, uses(axis, units) => {
            if !units.is_finite() {
                return Err(invalid_input(crate::InputDefect::KvmWheel { units }));
            }
            match axis {
                WheelAxis::Vertical => {
                    kvm_send!(self, NDIlib_recv_kvm_send_vertical_mouse_wheel, units)
                }
                WheelAxis::Horizontal => {
                    kvm_send!(self, NDIlib_recv_kvm_send_horizontal_mouse_wheel, units)
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
    #[doc(alias = "NDIlib_recv_kvm_send_mouse_position")]
    pub fn move_pointer(&mut self, x: f32, y: f32) -> Result<()> {
        native!(Kvm, self.kvm, uses(x, y) => {
            validate_position(x, y)?;
            // The SDK reads the two coordinates through the pointer.
            let position = [x, y];
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
    #[doc(alias = "NDIlib_recv_kvm_send_clipboard_contents")]
    pub fn set_clipboard_text(&mut self, text: &str) -> Result<()> {
        native!(Kvm, self.kvm, uses(text) => {
            let text = CString::new(text)?;
            kvm_send!(
                self,
                NDIlib_recv_kvm_send_clipboard_contents,
                text.as_ptr()
            )
        })
    }

    /// Report the current touch points, each an `[x, y]` fraction of the
    /// source's screen. An empty slice sends the same lift as
    /// [`finish_touch`](Self::finish_touch).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidInput`] when a coordinate is outside `0.0..=1.0` or
    /// there are more points than the SDK can count, and
    /// [`Error::NativeOperationFailed`] when the SDK did not send them.
    #[doc(alias = "NDIlib_recv_kvm_send_touch_positions")]
    pub fn touch(&mut self, points: &[[f32; 2]]) -> Result<()> {
        native!(Kvm, self.kvm, uses(points) => {
            let connection = self.receiver.core.lock_connection();
            self.held.touch(&connection, self.kvm, points)
        })
    }

    /// Lift every touch.
    ///
    /// # Errors
    ///
    /// [`Error::NativeOperationFailed`] when the SDK did not send the lift;
    /// the touches stay recorded.
    #[doc(alias = "NDIlib_recv_kvm_send_touch_finish")]
    pub fn finish_touch(&mut self) -> Result<()> {
        native!(Kvm, self.kvm => {
            let connection = self.receiver.core.lock_connection();
            self.held.finish_touch(&connection, self.kvm)
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
        native!(Kvm, self.kvm => {
            let connection = self.receiver.core.lock_connection();
            self.held.release_all(&connection, self.kvm)
        })
    }
}

impl<M: ReceiverMode> Drop for KvmSession<'_, M> {
    fn drop(&mut self) {
        native!(Kvm, self.kvm => {
            let connection = self.receiver.core.lock_connection();
            self.held.release_on_drop(&connection, self.kvm);
        })
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
#[path = "kvm_tests.rs"]
mod ownership_tests;

#[cfg(all(test, ndi_has_advanced_kvm))]
#[path = "kvm_view_tests.rs"]
mod view_tests;

#[cfg(ndi_test_receiver_proofs)]
fn a_raw_handle_cannot_send_input(
    sender: &impl InputSender,
    instance: crate::sys::bindings::NDIlib_recv_instance_t,
) {
    let _ = sender.key(instance, 1, true);
}
