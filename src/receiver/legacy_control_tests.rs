//! A native double holds connect at its entry while callers exercise controls.
use super::*;
use parking_lot::Condvar;

#[derive(Default)]
struct Native {
    gate: Mutex<GateState>,
    changed: Condvar,
    active: std::sync::atomic::AtomicBool,
    unsafe_entries: AtomicUsize,
    getter_calls: AtomicUsize,
    tallies: Mutex<Vec<Tally>>,
}

#[derive(Default)]
struct GateState {
    entered: bool,
    released: bool,
}

struct Release<'a>(&'a Native);

impl Drop for Release<'_> {
    fn drop(&mut self) {
        self.0.gate.lock().released = true;
        self.0.changed.notify_all();
    }
}

fn native(instance: NDIlib_recv_instance_t) -> Arc<Native> {
    // SAFETY: the test's creation closure transfers this box to the receiver.
    // Its live instance keeps the box until the bound destroy call.
    Arc::clone(unsafe { &*instance.cast::<Arc<Native>>() })
}

unsafe extern "C" fn destroy(instance: NDIlib_recv_instance_t) {
    // SAFETY: receiver creation transferred one box to this native owner.
    drop(unsafe { Box::from_raw(instance.cast::<Arc<Native>>()) });
}

unsafe extern "C" fn connect(instance: NDIlib_recv_instance_t, _: *const NDIlib_source_t) {
    let native = native(instance);
    native.active.store(true, Ordering::Release);
    let mut gate = native.gate.lock();
    gate.entered = true;
    native.changed.notify_all();
    while !gate.released {
        native.changed.wait(&mut gate);
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

unsafe extern "C" fn connections(_: NDIlib_recv_instance_t) -> i32 {
    0
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
    let mut table = sys::Core::load(&sys::table::ExportsWhere(|_| true))
        .unwrap()
        .unwrap();
    table.NDIlib_recv_connect = connect;
    table.NDIlib_recv_set_tally = tally;
    table.NDIlib_recv_send_metadata = metadata;
    table.NDIlib_recv_ptz_zoom = zoom;
    table.NDIlib_recv_get_no_connections = connections;
    table.NDIlib_recv_get_performance = performance;
    table.NDIlib_recv_get_queue = queue;
    table.NDIlib_recv_destroy = destroy;
    let ndi = Ndi::test_double(table);
    let native = Arc::new(Native::default());
    // SAFETY: the closure creates a fresh receiver handle for this runtime,
    // whose core includes the callbacks above and owns destruction.
    let mut receiver: Receiver = unsafe {
        Receiver::create(
            &ndi,
            &ReceiverOptions::new(Source::new("").unwrap()),
            #[cfg(feature = "advanced")]
            None,
            |_, _| {
                let instance = Box::into_raw(Box::new(Arc::clone(&native))).cast();
                sys::table::for_test(instance, c"NDIlib_recv_create_v3")
            },
        )
    }
    .unwrap();
    Arc::get_mut(&mut receiver.core)
        .unwrap()
        .guard_local_connection_changes = true;
    std::thread::scope(|scope| {
        let receiver = &receiver;
        let release = Release(&native);
        let worker = scope.spawn(move || receiver.disconnect());
        let mut gate = native.gate.lock();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !gate.entered {
            assert!(
                !native.changed.wait_until(&mut gate, deadline).timed_out(),
                "native double never entered connect"
            );
        }
        drop(gate);
        let (done, completed) = std::sync::mpsc::channel();
        scope.spawn(move || {
            receiver.set_tally(Tally::new(true, false));
            receiver.set_tally(Tally::new(false, true));
            let metadata = MetadataFrame::new("<during-connect/>").unwrap();
            let sent = receiver.send_metadata(&metadata);
            let zoom = receiver.ptz().zoom(0.5);
            let registration = receiver.add_connection_metadata(&metadata);
            let stats = receiver.connection_stats();
            done.send((sent, zoom, registration, stats)).unwrap();
        });
        let observed = completed.recv_timeout(Duration::from_secs(2));
        let before_flush = native.tallies.lock().clone();
        drop(release);
        worker.join().unwrap();
        let (sent, zoom, registration, stats) =
            observed.expect("controls waited for native connect");
        assert!(matches!(sent, Err(Error::NotConnected)));
        assert!(matches!(zoom, Err(Error::NotConnected)));
        assert!(matches!(
            registration,
            Err(Error::NotConnected | Error::UnsupportedCapability { .. })
        ));
        stats.unwrap();
        assert!(
            before_flush.is_empty(),
            "native tally entered before connect returned"
        );
    });
    assert_eq!(native.unsafe_entries.load(Ordering::Relaxed), 0);
    assert_eq!(native.getter_calls.load(Ordering::Relaxed), 2);
    assert_eq!(*native.tallies.lock(), [Tally::new(false, true)]);
}
