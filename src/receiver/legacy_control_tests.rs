//! A native double holds connect at its entry while callers exercise controls.
use super::*;
use parking_lot::Condvar;
use std::{collections::HashMap, sync::OnceLock};

#[derive(Default)]
struct Native {
    entered: Mutex<bool>,
    released: Mutex<bool>,
    changed: Condvar,
    active: std::sync::atomic::AtomicBool,
    unsafe_entries: AtomicUsize,
    getter_calls: AtomicUsize,
    tallies: Mutex<Vec<Tally>>,
}

fn natives() -> &'static Mutex<HashMap<usize, Arc<Native>>> {
    static NATIVES: OnceLock<Mutex<HashMap<usize, Arc<Native>>>> = OnceLock::new();
    NATIVES.get_or_init(Mutex::default)
}

fn native(instance: NDIlib_recv_instance_t) -> Arc<Native> {
    Arc::clone(natives().lock().get(&(instance as usize)).unwrap())
}

unsafe extern "C" fn connect(instance: NDIlib_recv_instance_t, _: *const NDIlib_source_t) {
    let native = native(instance);
    native.active.store(true, Ordering::Release);
    *native.entered.lock() = true;
    native.changed.notify_all();
    let mut released = native.released.lock();
    while !*released {
        native.changed.wait(&mut released);
    }
    native.active.store(false, Ordering::Release);
}

fn record_write(native: &Native) {
    if native.active.load(Ordering::Acquire) {
        native.unsafe_entries.fetch_add(1, Ordering::Relaxed);
    }
}

unsafe extern "C" fn tally(instance: NDIlib_recv_instance_t, tally: *const NDIlib_tally_t) -> bool {
    let native = native(instance);
    record_write(&native);
    // SAFETY: the synchronous receiver call lends this initialized tally.
    let tally = unsafe { &*tally };
    native
        .tallies
        .lock()
        .push(Tally::new(tally.on_program, tally.on_preview));
    true
}

unsafe extern "C" fn metadata(
    instance: NDIlib_recv_instance_t,
    _: *const NDIlib_metadata_frame_t,
) -> bool {
    record_write(&native(instance));
    true
}

unsafe extern "C" fn zoom(instance: NDIlib_recv_instance_t, _: f32) -> bool {
    record_write(&native(instance));
    true
}

unsafe extern "C" fn performance(
    instance: NDIlib_recv_instance_t,
    total: *mut NDIlib_recv_performance_t,
    dropped: *mut NDIlib_recv_performance_t,
) {
    native(instance)
        .getter_calls
        .fetch_add(1, Ordering::Relaxed);
    // SAFETY: connection_stats supplies writable descriptors of these types.
    unsafe {
        total.write(Default::default());
        dropped.write(Default::default());
    }
}

unsafe extern "C" fn queue(instance: NDIlib_recv_instance_t, queue: *mut NDIlib_recv_queue_t) {
    native(instance)
        .getter_calls
        .fetch_add(1, Ordering::Relaxed);
    // SAFETY: connection_stats supplies this writable descriptor.
    unsafe {
        queue.write(Default::default());
    }
}

#[test]
fn native_change_refuses_unsafe_controls_and_replays_tally_without_blocking_getters() {
    let Some(ndi) = crate::test_support::ndi() else {
        return;
    };
    let mut receiver =
        Receiver::new(&ndi, &ReceiverOptions::new(Source::new("").unwrap())).unwrap();
    let core = Arc::get_mut(&mut receiver.core).unwrap();
    core.guard_local_connection_changes = true;
    let table = core.sdk().with_test_overrides(|table| {
        table.NDIlib_recv_connect = connect;
        table.NDIlib_recv_set_tally = tally;
        table.NDIlib_recv_send_metadata = metadata;
        table.NDIlib_recv_ptz_zoom = zoom;
        table.NDIlib_recv_get_performance = performance;
        table.NDIlib_recv_get_queue = queue;
    });
    core.test_sdk = Some(Box::leak(Box::new(table)));
    let instance = core.instance.0 as usize;
    let native = Arc::new(Native::default());
    natives().lock().insert(instance, Arc::clone(&native));
    std::thread::scope(|scope| {
        let receiver = &receiver;
        let worker = scope.spawn(move || receiver.disconnect());
        let mut entered = native.entered.lock();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !*entered {
            assert!(
                !native
                    .changed
                    .wait_until(&mut entered, deadline)
                    .timed_out(),
                "native double never entered connect"
            );
        }
        drop(entered);
        receiver.set_tally(Tally::new(true, false));
        receiver.set_tally(Tally::new(false, true));
        let metadata = MetadataFrame::new("<during-connect/>").unwrap();
        let sent = receiver.send_metadata(&metadata);
        let zoom = receiver.ptz().zoom(0.5);
        let registration = receiver.add_connection_metadata(&metadata);
        let (done, completed) = std::sync::mpsc::channel();
        scope.spawn(move || {
            done.send(receiver.connection_stats()).unwrap();
        });
        let observed = completed.recv_timeout(Duration::from_secs(2));
        let before_flush = native.tallies.lock().clone();
        *native.released.lock() = true;
        native.changed.notify_all();
        worker.join().unwrap();
        assert!(matches!(sent, Err(Error::NotConnected)));
        assert!(matches!(zoom, Err(Error::NotConnected)));
        assert!(matches!(
            registration,
            Err(Error::NotConnected | Error::UnsupportedCapability { .. })
        ));
        observed.expect("getter waited for native connect").unwrap();
        assert!(
            before_flush.is_empty(),
            "native tally entered before connect returned"
        );
    });
    assert_eq!(native.unsafe_entries.load(Ordering::Relaxed), 0);
    assert_eq!(native.getter_calls.load(Ordering::Relaxed), 2);
    assert_eq!(*native.tallies.lock(), [Tally::new(false, true)]);
    natives().lock().remove(&instance);
}
