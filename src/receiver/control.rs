//! Shared receiver controls and the permission to use a replaceable connection.

use super::*;
#[cfg(ndi_has_advanced_kvm)]
use crate::{advanced::TouchPoints, sys::advanced::Kvm};

#[derive(Default)]
pub(super) struct ControlPermit {
    registrations: ConnectionMetadata,
    // Standard Windows controls do not synchronize their connection reads
    // with connect. Publish this phase before entering native code, then
    // release the control lock so receiver-owned observations can proceed.
    changing: bool,
    pending_tally: Option<Tally>,
}

/// Serialized controls that can operate during a local connection change.
pub(crate) struct Control<'a> {
    instance: RecvInstance,
    state: MutexGuard<'a, ControlPermit>,
}

/// Permission to use the receiver's replaceable connection. On runtimes
/// without native exclusion its guard prevents a local change from starting.
pub(crate) struct MutatingControl<'a>(Control<'a>);

impl ReceiverCore {
    pub(super) fn lock_control(&self) -> Control<'_> {
        Control {
            instance: self.instance,
            state: self.control_guard.lock(),
        }
    }

    /// Refuse immediately when Standard Windows is replacing the connection.
    pub(crate) fn lock_mutating_control(&self) -> Result<MutatingControl<'_>> {
        self.lock_control().admit().map_err(|_| Error::NotConnected)
    }

    pub(super) fn begin_control_change(&self) {
        self.control_guard.lock().changing = true;
    }

    pub(super) fn set_tally(&self, tally: Tally) {
        match self.lock_control().admit() {
            Ok(connection) => connection.set_tally(self.sdk(), tally),
            Err(mut control) => control.state.pending_tally = Some(tally),
        }
    }

    pub(super) fn set_delay(
        &self,
        stream: ReceiveDelayStream,
        delay: ReceiveDelay,
    ) -> Result<SettingChange> {
        let table = self.ndi().table::<sys::ReceiveDelay>()?;
        let control = self.lock_control();
        // SAFETY: the live receiver and permit cover this call. See the
        // RecvInstance inventory for the delay state and connection locks.
        let changed = unsafe {
            match stream {
                ReceiveDelayStream::Video => {
                    table.NDIlib_recv_set_video_delay(control.instance.0, delay.to_sdk())
                }
                ReceiveDelayStream::Audio => {
                    table.NDIlib_recv_set_audio_delay(control.instance.0, delay.to_sdk())
                }
            }
        };
        Ok(SettingChange::from_sdk(changed))
    }

    pub(super) fn delay(&self, stream: ReceiveDelayStream) -> Result<ReceiveDelay> {
        let table = self.ndi().table::<sys::ReceiveDelay>()?;
        let control = self.lock_control();
        // SAFETY: the permit covers access to the live receiver's delay state.
        let millis = unsafe {
            match stream {
                ReceiveDelayStream::Video => table.NDIlib_recv_get_video_delay(control.instance.0),
                ReceiveDelayStream::Audio => table.NDIlib_recv_get_audio_delay(control.instance.0),
            }
        };
        ReceiveDelay::from_sdk(millis)
    }
}

impl ConnectionChange<'_> {
    pub(super) fn finish_control_change(&self) {
        let mut control = self.core.lock_control();
        // The completed request still holds the change permit. Clear the
        // phase and replay tally under one lock so another tally cannot pass it.
        control.state.changing = false;
        if let Some(tally) = control.state.pending_tally.take() {
            MutatingControl(control).set_tally(self.core.sdk(), tally);
        }
    }
}

impl<'a> Control<'a> {
    fn admit(self) -> std::result::Result<MutatingControl<'a>, Self> {
        if self.state.changing {
            Err(self)
        } else {
            Ok(MutatingControl(self))
        }
    }

    pub(super) fn clear_metadata(&mut self, table: &sys::ReceiverConnectionMetadata) {
        self.state.registrations.clear(|| {
            // SAFETY: the live receiver owns these registrations until native
            // clear returns. The permit serializes controls, and the SDK
            // clears its receiver-owned list independently of connection state.
            unsafe { table.NDIlib_recv_clear_connection_metadata(self.instance.0) };
        });
    }

    pub(super) fn web_control(
        &self,
        table: &sys::ReceiverWebControl,
    ) -> sys::table::Called<*const c_char> {
        // SAFETY: the live receiver and permit cover this call. See the
        // RecvInstance inventory for web control access.
        unsafe { table.named().NDIlib_recv_get_web_control(self.instance.0) }
    }

    pub(super) fn performance(
        &self,
        sdk: &sys::Core,
        total: &mut NDIlib_recv_performance_t,
        dropped: &mut NDIlib_recv_performance_t,
    ) -> sys::table::Called<()> {
        // SAFETY: the receiver is live and both writable outputs outlive the
        // synchronous call. See the RecvInstance inventory for shared controls.
        unsafe {
            sdk.named()
                .NDIlib_recv_get_performance(self.instance.0, total, dropped)
        }
    }

    pub(super) fn queue(
        &self,
        sdk: &sys::Core,
        queue: &mut NDIlib_recv_queue_t,
    ) -> sys::table::Called<()> {
        // SAFETY: the receiver is live and the writable output outlives the
        // synchronous call. See the RecvInstance inventory for shared controls.
        unsafe { sdk.named().NDIlib_recv_get_queue(self.instance.0, queue) }
    }

    #[cfg(test)]
    pub(super) fn pending_tally(&self) -> Option<Tally> {
        self.state.pending_tally
    }
}

macro_rules! ptz_controls {
    ($($method:ident => $native:ident($($arg:ident: $ty:ty),*);)+) => {
        $(
            pub(crate) fn $method(&self, sdk: &sys::Core, $($arg: $ty),*) -> Result<()> {
                // SAFETY: this permit and the SDK exclusion recorded in the
                // RecvInstance inventory cover the live connection. Every
                // argument is a plain value.
                unsafe { sdk.named().$native(self.0.instance.0, $($arg),*) }.performed()
            }
        )+
    };
}

impl MutatingControl<'_> {
    fn set_tally(&self, sdk: &sys::Core, tally: Tally) {
        // SAFETY: the permit excludes local replacements on Standard Windows,
        // which forbids SDK-internal changes from Discovery Server clients.
        // Other runtimes supply native exclusion. See RecvInstance.
        unsafe { sdk.NDIlib_recv_set_tally(self.0.instance.0, &tally.to_sdk()) };
    }

    pub(crate) fn send_metadata<S: MetadataSource + ?Sized>(
        &self,
        sdk: &sys::Core,
        metadata: &S,
    ) -> Result<()> {
        let raw = metadata.metadata_raw();
        // SAFETY: the live connection is covered by this permit and the
        // exclusion in the RecvInstance inventory. The borrowed C string
        // outlives the synchronous call.
        let sent = unsafe { sdk.NDIlib_recv_send_metadata(self.0.instance.0, &raw) };
        // Processing.NDI.Recv.h: false means nothing is connected.
        if sent {
            Ok(())
        } else {
            Err(Error::NotConnected)
        }
    }

    pub(crate) fn add_metadata<S: MetadataSource + ?Sized>(
        &mut self,
        table: &sys::ReceiverConnectionMetadata,
        metadata: &S,
    ) {
        self.0.state.registrations.add(metadata, |raw| {
            // SAFETY: the permit protects the connection and serializes
            // controls. It retains the descriptor and text until clear
            // returns or after native destruction.
            unsafe { table.NDIlib_recv_add_connection_metadata(self.0.instance.0, raw) };
        });
    }

    pub(crate) fn ptz_is_supported(&self, sdk: &sys::Core) -> bool {
        // SAFETY: the permit covers the live connection per the inventory.
        unsafe { sdk.NDIlib_recv_ptz_is_supported(self.0.instance.0) }
    }

    ptz_controls! {
        ptz_zoom => NDIlib_recv_ptz_zoom(zoom: f32);
        ptz_zoom_speed => NDIlib_recv_ptz_zoom_speed(speed: f32);
        ptz_pan_tilt => NDIlib_recv_ptz_pan_tilt(pan: f32, tilt: f32);
        ptz_pan_tilt_speed => NDIlib_recv_ptz_pan_tilt_speed(pan: f32, tilt: f32);
        ptz_store_preset => NDIlib_recv_ptz_store_preset(preset: i32);
        ptz_recall_preset => NDIlib_recv_ptz_recall_preset(preset: i32, speed: f32);
        ptz_auto_focus => NDIlib_recv_ptz_auto_focus();
        ptz_focus => NDIlib_recv_ptz_focus(focus: f32);
        ptz_focus_speed => NDIlib_recv_ptz_focus_speed(speed: f32);
        ptz_white_balance_auto => NDIlib_recv_ptz_white_balance_auto();
        ptz_white_balance_indoor => NDIlib_recv_ptz_white_balance_indoor();
        ptz_white_balance_outdoor => NDIlib_recv_ptz_white_balance_outdoor();
        ptz_white_balance_oneshot => NDIlib_recv_ptz_white_balance_oneshot();
        ptz_white_balance_manual => NDIlib_recv_ptz_white_balance_manual(red: f32, blue: f32);
        ptz_exposure_auto => NDIlib_recv_ptz_exposure_auto();
        ptz_exposure_manual => NDIlib_recv_ptz_exposure_manual(level: f32);
        ptz_exposure_manual_v2 => NDIlib_recv_ptz_exposure_manual_v2(iris: f32, gain: f32, shutter_speed: f32);
    }

    #[cfg(ndi_has_advanced_keyframe_request)]
    pub(crate) fn request_keyframe(&self, table: &sys::advanced::KeyframeRequest) {
        // SAFETY: the permit covers the live connection per the inventory.
        unsafe { table.NDIlib_recv_request_keyframe(self.0.instance.0) };
    }

    #[cfg(all(test, feature = "advanced"))]
    pub(crate) fn with_test_instance<T>(
        instance: NDIlib_recv_instance_t,
        call: impl FnOnce(&MutatingControl<'_>) -> T,
    ) -> T {
        let gate = Mutex::new(ControlPermit::default());
        let control = Control {
            instance: RecvInstance(instance),
            state: gate.lock(),
        };
        let connection = MutatingControl(control);
        call(&connection)
    }

    #[cfg(all(test, feature = "advanced", not(ndi_has_advanced_kvm)))]
    pub(crate) fn test_instance(&self) -> NDIlib_recv_instance_t {
        self.0.instance.0
    }
}

#[cfg(ndi_has_advanced_kvm)]
macro_rules! kvm_controls {
    ($($method:ident => $native:ident($($arg:ident: $ty:ty),*);)+) => {
        $(
            pub(crate) fn $method(&self, table: &Kvm, $($arg: $ty),*) -> Result<()> {
                // SAFETY: the admission permit and RecvInstance inventory
                // cover the live connection. The arguments are plain values.
                unsafe { table.named().$native(self.0.instance.0, $($arg),*) }.performed()
            }
        )+
    };
}

#[cfg(ndi_has_advanced_kvm)]
impl MutatingControl<'_> {
    pub(crate) fn kvm_is_supported(&self, table: &Kvm) -> bool {
        // SAFETY: the permit covers the live connection per the inventory.
        unsafe { table.NDIlib_recv_kvm_is_supported(self.0.instance.0) }
    }

    kvm_controls! {
        kvm_left_click => NDIlib_recv_kvm_send_left_mouse_click();
        kvm_middle_click => NDIlib_recv_kvm_send_middle_mouse_click();
        kvm_right_click => NDIlib_recv_kvm_send_right_mouse_click();
        kvm_left_release => NDIlib_recv_kvm_send_left_mouse_release();
        kvm_middle_release => NDIlib_recv_kvm_send_middle_mouse_release();
        kvm_right_release => NDIlib_recv_kvm_send_right_mouse_release();
        kvm_wheel_vertical => NDIlib_recv_kvm_send_vertical_mouse_wheel(units: f32);
        kvm_wheel_horizontal => NDIlib_recv_kvm_send_horizontal_mouse_wheel(units: f32);
        kvm_touch_finish => NDIlib_recv_kvm_send_touch_finish();
        kvm_key_press => NDIlib_recv_kvm_send_keyboard_press(key: i32);
        kvm_key_release => NDIlib_recv_kvm_send_keyboard_release(key: i32);
    }

    pub(crate) fn kvm_mouse_position(&self, table: &Kvm, position: &[f32; 2]) -> Result<()> {
        // SAFETY: the permit covers the live connection, and the SDK reads
        // only the two coordinates that this array lends for the call.
        unsafe {
            table
                .named()
                .NDIlib_recv_kvm_send_mouse_position(self.0.instance.0, position.as_ptr())
        }
        .performed()
    }

    pub(crate) fn kvm_clipboard_contents(&self, table: &Kvm, text: &CStr) -> Result<()> {
        // SAFETY: the permit covers the live connection. The C string outlives
        // the synchronous call.
        unsafe {
            table
                .named()
                .NDIlib_recv_kvm_send_clipboard_contents(self.0.instance.0, text.as_ptr())
        }
        .performed()
    }

    pub(crate) fn kvm_touch_positions(&self, table: &Kvm, points: TouchPoints<'_>) -> Result<()> {
        // SAFETY: the permit covers the live connection, and TouchPoints
        // lends initialized pairs and their exact SDK count.
        unsafe {
            table.named().NDIlib_recv_kvm_send_touch_positions(
                self.0.instance.0,
                points.count(),
                points.as_ptr(),
            )
        }
        .performed()
    }
}
