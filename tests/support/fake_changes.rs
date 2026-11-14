//! Native change reports deliberately release only one waiter per update.
use super::*;
use std::{
    collections::HashMap,
    sync::{atomic::{AtomicBool, Ordering}, Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct NativeChanges {
    state: Mutex<(usize, bool)>,
    changed: Condvar,
}

impl NativeChanges {
    pub(super) fn generation(&self) -> usize {
        self.state.lock().unwrap().0
    }

    fn publish(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 += 1;
        state.1 = true;
        self.changed.notify_one();
    }

    pub(super) fn wait(&self, timeout: u32, label: &str) -> bool {
        log(&format!("{label}-wait-begin"));
        let end = Instant::now() + Duration::from_millis(u64::from(timeout));
        let mut state = self.state.lock().unwrap();
        while !state.1 {
            let remaining = end.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                log(&format!("{label}-wait-end"));
                return false;
            }
            state = self.changed.wait_timeout(state, remaining).unwrap().0;
        }
        state.1 = false;
        log(&format!("{label}-wait-end"));
        true
    }
}

pub(super) fn sources() -> &'static NativeChanges {
    static SOURCES: OnceLock<NativeChanges> = OnceLock::new();
    SOURCES.get_or_init(NativeChanges::default)
}

/// The test controls when a source appears, without making a second native
/// finder or allowing a snapshot itself to consume a change.
#[no_mangle]
pub extern "C" fn GRAFTON_fake_source_change() {
    sources().publish();
}

type Selectors = HashMap<usize, Arc<NativeChanges>>;
fn selectors() -> &'static Mutex<Selectors> {
    static SELECTORS: OnceLock<Mutex<Selectors>> = OnceLock::new();
    SELECTORS.get_or_init(Mutex::default)
}

static KEYFRAME_REQUIRED: AtomicBool = AtomicBool::new(false);

#[cfg(fake_advanced)]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_send_is_keyframe_required(
    _sender: NDIlib_send_instance_t,
    _format: *const NDIlib_video_frame_v2_t,
) -> bool {
    let program = _format.is_null() || unsafe { (*_format).FourCC }.to_le_bytes()[0].is_ascii_uppercase();
    program && KEYFRAME_REQUIRED.load(Ordering::Acquire)
}

#[cfg(fake_advanced)]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_send_wait_for_keyframe_request(
    sender: NDIlib_send_instance_t,
    timeout: u32,
    format: *const NDIlib_video_frame_v2_t,
) -> bool {
    let selector = if format.is_null() { None } else { Some(unsafe { (*format).FourCC }) };
    let changes = selectors().lock().unwrap().entry(sender as usize).or_default().clone();
    changes.wait(timeout, &format!("keyframe-{selector:?}"))
}

#[cfg(fake_advanced)]
#[no_mangle]
pub unsafe extern "C" fn NDIlib_recv_request_keyframe(_receiver: NDIlib_recv_instance_t) {
    KEYFRAME_REQUIRED.store(true, Ordering::Release);
    for changes in selectors().lock().unwrap().values() {
        changes.publish();
    }
}
