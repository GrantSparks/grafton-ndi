//! Sender tally has one consumable native update, shared by polls and waits.
use super::*;
use std::{collections::HashMap, sync::{Arc, Condvar, Mutex}, time::{Duration, Instant}};

#[derive(Default)]
struct Tally {
    state: Mutex<(u8, bool)>,
    changed: Condvar,
}

fn senders() -> &'static Mutex<HashMap<usize, Arc<Tally>>> {
    static SENDERS: OnceLock<Mutex<HashMap<usize, Arc<Tally>>>> = OnceLock::new();
    SENDERS.get_or_init(Mutex::default)
}

pub(super) fn created(sender: NDIlib_send_instance_t) {
    senders().lock().unwrap().insert(sender as usize, Arc::default());
}

pub(super) fn destroyed(sender: NDIlib_send_instance_t) {
    senders().lock().unwrap().remove(&(sender as usize));
}

export! {
    fn NDIlib_send_get_tally(sender: NDIlib_send_instance_t, tally: *mut NDIlib_tally_t, timeout: u32) -> bool {
        let entry = senders().lock().unwrap()[&(sender as usize)].clone();
        let end = Instant::now() + Duration::from_millis(u64::from(timeout));
        log(if timeout == 0 { "tally-poll" } else { "tally-wait-begin" });
        let mut state = entry.state.lock().unwrap();
        while !state.1 {
            let remaining = end.saturating_duration_since(Instant::now());
            if remaining.is_zero() { break; }
            state = entry.changed.wait_timeout(state, remaining).unwrap().0;
        }
        unsafe { *tally = NDIlib_tally_t { on_program: state.0 & 1 != 0, on_preview: state.0 & 2 != 0 }; }
        let changed = std::mem::take(&mut state.1);
        if timeout > 0 { log("tally-wait-end"); }
        changed
    }

    fn NDIlib_recv_set_tally(_receiver: NDIlib_recv_instance_t, tally: *const NDIlib_tally_t) -> bool {
        let tally = unsafe { &*tally };
        let bits = u8::from(tally.on_program) | (u8::from(tally.on_preview) << 1);
        for entry in senders().lock().unwrap().values() {
            let mut state = entry.state.lock().unwrap();
            if state.0 != bits {
                state.0 = bits;
                state.1 = true;
                entry.changed.notify_one();
            }
        }
        true
    }
}
