//! Executor callbacks may reenter File handles, including from clone and drop.
use super::tests::block_on;
use super::*;
use std::task::{RawWaker, RawWakerVTable, Wake};

struct OnDrop<F: Fn() + Send + Sync>(F);

impl<F: Fn() + Send + Sync> Wake for OnDrop<F> {
    fn wake(self: Arc<Self>) {
        // Consuming the last task reference runs the executor's destructor.
        drop(self);
    }
}

impl<F: Fn() + Send + Sync> Drop for OnDrop<F> {
    fn drop(&mut self) {
        (self.0)();
    }
}

fn drop_probe(unlocked: impl Fn() -> bool + Send + Sync + 'static) -> (Waker, Receiver<bool>) {
    let (send, receive) = mpsc::channel();
    let waker = Waker::from(Arc::new(OnDrop(move || {
        let _ = send.send(unlocked());
    })));
    (waker, receive)
}

fn poll<F: Future + Unpin>(future: &mut F, waker: &Waker) -> Poll<F::Output> {
    Pin::new(future).poll(&mut Context::from_waker(waker))
}

fn pending_event() -> (FileEventFuture<()>, Arc<EventQueue<()>>, Receiver<bool>) {
    let events = Arc::new(EventQueue::new(2));
    let weak = Arc::downgrade(&events);
    let (waker, released) = drop_probe(move || weak.upgrade().unwrap().0.try_lock().is_ok());
    let mut future = events.wait(Duration::from_secs(60));
    assert!(poll(&mut future, &waker).is_pending());
    drop(waker); // The registered waiter now owns the last executor reference.
    (future, events, released)
}

fn assert_released_unlocked(released: Receiver<bool>) {
    assert!(
        released.recv_timeout(Duration::from_secs(1)).unwrap(),
        "executor destructor ran under an internal lock"
    );
}

#[test]
fn cancelling_an_event_releases_executor_state_outside_the_queue_lock() {
    let (future, _events, released) = pending_event();
    drop(future);
    assert_released_unlocked(released);
}

#[test]
fn delivering_an_event_releases_executor_state_outside_the_queue_lock() {
    let (mut future, events, released) = pending_event();
    events.push(Ok(()));
    assert!(matches!(
        poll(&mut future, Waker::noop()),
        Poll::Ready(Ok(Some(())))
    ));
    assert_released_unlocked(released);
}

#[test]
fn replacing_an_event_waker_releases_it_outside_the_queue_lock() {
    let (mut future, _events, released) = pending_event();
    assert!(poll(&mut future, Waker::noop()).is_pending());
    assert_released_unlocked(released);
}

#[test]
fn replacing_a_response_waker_releases_it_outside_the_response_lock() {
    let (_reply, mut future) = operation::<()>();
    let weak = Arc::downgrade(&future.response);
    let (waker, released) = drop_probe(move || weak.upgrade().unwrap().try_lock().is_ok());
    assert!(poll(&mut future, &waker).is_pending());
    drop(waker);
    assert!(poll(&mut future, Waker::noop()).is_pending());
    assert_released_unlocked(released);
}

#[test]
fn cancelling_a_close_releases_executor_state_before_pruning_replies() {
    let control = Arc::new(Control::new());
    let weak = Arc::downgrade(&control);
    let (waker, released) = drop_probe(move || weak.upgrade().unwrap().closing.try_lock().is_ok());
    let mut future = control.request_close_now();
    assert!(poll(&mut future, &waker).is_pending());
    drop(waker);
    drop(future);
    let _next = control.request_close_now();
    assert_released_unlocked(released);
}

#[test]
fn panicking_executor_cleanup_does_not_escape_cancellation_or_poison_locks() {
    let events = Arc::new(EventQueue::<()>::new(1));
    let mut event = events.wait(Duration::from_secs(60));
    let (_reply, mut command) = operation::<()>();
    let waker = || Waker::from(Arc::new(OnDrop(|| panic!("executor destructor panicked"))));
    assert!(poll(&mut event, &waker()).is_pending());
    assert!(poll(&mut command, &waker()).is_pending());
    assert!(catch_unwind(AssertUnwindSafe(|| drop(event))).is_ok());
    assert!(catch_unwind(AssertUnwindSafe(|| drop(command))).is_ok());
    assert!(!events.0.is_poisoned());
    let mut next = events.wait(Duration::ZERO);
    assert!(matches!(
        poll(&mut next, Waker::noop()),
        Poll::Ready(Ok(None))
    ));
}

struct CloneProbe {
    on_clone: Box<dyn Fn() + Send + Sync>,
    clones: AtomicUsize,
    wakes: AtomicUsize,
}

// Keep one vtable address so will_wake can recognize clones of the same task.
static PROBE_VTABLE: RawWakerVTable = CloneProbe::VTABLE;

impl CloneProbe {
    fn new(on_clone: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            on_clone: Box::new(on_clone),
            clones: AtomicUsize::new(0),
            wakes: AtomicUsize::new(0),
        })
    }

    fn waker(self: &Arc<Self>) -> Waker {
        // SAFETY: raw transfers one Arc reference to the vtable below. All
        // callbacks use the same type, balance ownership and are thread-safe.
        unsafe { Waker::from_raw(Self::raw(Arc::clone(self))) }
    }

    fn raw(probe: Arc<Self>) -> RawWaker {
        RawWaker::new(Arc::into_raw(probe).cast(), &PROBE_VTABLE)
    }

    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        |data| {
            // SAFETY: the source waker owns a live Arc throughout this call.
            let probe = unsafe { &*data.cast::<Self>() };
            probe.clones.fetch_add(1, Ordering::Relaxed);
            (probe.on_clone)();
            // SAFETY: the source reference stays live; transfer a new strong
            // reference to the returned waker only after the callback succeeds.
            unsafe { Arc::increment_strong_count(data.cast::<Self>()) };
            RawWaker::new(data, &PROBE_VTABLE)
        },
        |data| {
            // SAFETY: wake consumes the reference owned by this waker.
            let probe = unsafe { Arc::from_raw(data.cast::<Self>()) };
            probe.wakes.fetch_add(1, Ordering::Relaxed);
        },
        |data| {
            // SAFETY: wake_by_ref borrows a live waker without consuming it.
            let probe = unsafe { &*data.cast::<Self>() };
            probe.wakes.fetch_add(1, Ordering::Relaxed);
        },
        |data| {
            // SAFETY: drop consumes exactly the reference owned by this waker.
            drop(unsafe { Arc::from_raw(data.cast::<Self>()) });
        },
    );
}

#[test]
fn a_reply_during_waker_cloning_is_observed_by_the_same_poll() {
    let (_reply, mut future) = operation::<u32>();
    let weak = Arc::downgrade(&future.response);
    let probe = CloneProbe::new(move || {
        let response = weak.upgrade().unwrap();
        // Fail instead of hanging if clone runs under the response lock.
        assert!(response.try_lock().is_ok());
        deliver(&response, Ok(7), false);
    });
    assert!(matches!(
        poll(&mut future, &probe.waker()),
        Poll::Ready(Ok(7))
    ));
    assert_eq!(probe.clones.load(Ordering::Relaxed), 1);
    assert_eq!(probe.wakes.load(Ordering::Relaxed), 0);
}

#[test]
fn events_and_closure_during_waker_cloning_are_observed_by_the_same_poll() {
    for close in [false, true] {
        let events = Arc::new(EventQueue::<u32>::new(1));
        let weak = Arc::downgrade(&events);
        let probe = CloneProbe::new(move || {
            let events = weak.upgrade().unwrap();
            assert!(events.0.try_lock().is_ok());
            if close {
                events.close(false);
            } else {
                events.push(Ok(7));
            }
        });
        let mut future = events.wait(Duration::from_secs(60));
        let result = poll(&mut future, &probe.waker());
        if close {
            assert!(matches!(
                result,
                Poll::Ready(Err(Error::InvalidState(StateError::Closed)))
            ));
        } else {
            assert!(matches!(result, Poll::Ready(Ok(Some(7)))));
        }
        assert!(future.timer.is_none());
        assert_eq!(probe.clones.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn ready_and_same_waker_command_polls_do_not_clone() {
    let (reply, mut future) = operation::<()>();
    let probe = CloneProbe::new(|| {});
    let waker = probe.waker();
    assert!(poll(&mut future, &waker).is_pending());
    assert!(poll(&mut future, &waker).is_pending());
    assert_eq!(probe.clones.load(Ordering::Relaxed), 1);
    reply.complete(Ok(()));
    assert_eq!(probe.wakes.load(Ordering::Relaxed), 1);
    assert!(matches!(poll(&mut future, &waker), Poll::Ready(Ok(()))));
    assert_eq!(probe.clones.load(Ordering::Relaxed), 1);

    let (reply, mut ready) = operation::<()>();
    reply.complete(Ok(()));
    assert!(poll(&mut ready, &waker).is_ready());
    assert_eq!(probe.clones.load(Ordering::Relaxed), 1);
}

#[test]
fn event_bursts_wake_once_without_cloning_or_releasing_the_waiter_reservation() {
    let events = Arc::new(EventQueue::<u32>::new(2));
    let mut future = events.wait(Duration::from_secs(60));
    let probe = CloneProbe::new(|| {});
    let waker = probe.waker();
    assert!(poll(&mut future, &waker).is_pending());
    assert!(poll(&mut future, &waker).is_pending());
    events.push(Ok(1));
    events.push(Ok(2));
    let mut competing = events.wait(Duration::ZERO);
    assert!(matches!(
        poll(&mut competing, &waker),
        Poll::Ready(Err(Error::InvalidState(StateError::WaiterPending)))
    ));
    assert!(matches!(
        poll(&mut future, &waker),
        Poll::Ready(Ok(Some(1)))
    ));
    let mut next = events.wait(Duration::ZERO);
    assert!(matches!(poll(&mut next, &waker), Poll::Ready(Ok(Some(2)))));
    assert_eq!(probe.clones.load(Ordering::Relaxed), 1);
    assert_eq!(probe.wakes.load(Ordering::Relaxed), 1);
}

#[test]
fn expiration_and_close_wake_without_cloning_and_pending_polls_rearm() {
    let events = Arc::new(EventQueue::<()>::new(1));
    let mut future = events.wait(Duration::from_secs(60));
    let probe = CloneProbe::new(|| {});
    let waker = probe.waker();
    assert!(poll(&mut future, &waker).is_pending());
    let token = Arc::downgrade(&future.token);
    // Invoke the timer callback directly, keeping the real deadline distant
    // so a subsequent poll can exercise rearming without timing sensitivity.
    events.expire(&token, Instant::now() + Duration::from_secs(61));
    events.expire(&token, Instant::now() + Duration::from_secs(61));
    assert_eq!(probe.clones.load(Ordering::Relaxed), 1);
    assert_eq!(probe.wakes.load(Ordering::Relaxed), 1);
    assert!(poll(&mut future, &waker).is_pending());
    assert_eq!(probe.clones.load(Ordering::Relaxed), 2);
    events.close(false);
    assert_eq!(probe.wakes.load(Ordering::Relaxed), 2);
    assert_eq!(probe.clones.load(Ordering::Relaxed), 2);
    assert!(matches!(
        poll(&mut future, &waker),
        Poll::Ready(Err(Error::InvalidState(StateError::Closed)))
    ));
}

/// Its destructor cannot finish until the test releases it. A joining
/// callback would therefore stall the File thread for the whole bounded wait.
struct HeldOwner {
    release: Receiver<()>,
    destroyed: Sender<()>,
}

impl Owner for HeldOwner {
    type Event = ();

    fn poll_native(&mut self) -> Result<Option<()>> {
        Ok(None)
    }
}

impl Drop for HeldOwner {
    fn drop(&mut self) {
        let _ = self.release.recv_timeout(Duration::from_secs(5));
        let _ = self.destroyed.send(());
    }
}

fn held_owner() -> (Arc<Worker<HeldOwner>>, Sender<()>, Receiver<()>) {
    let (release, released) = mpsc::channel();
    let (destroyed, destruction) = mpsc::channel();
    let worker = block_on(spawn_owner(
        "file-test-held-owner",
        move || {
            Ok(HeldOwner {
                release: released,
                destroyed,
            })
        },
        AsyncFileOptions::new(),
        |worker| worker,
    ))
    .unwrap();
    (worker, release, destruction)
}

fn owner_releasing_waker(worker: Arc<Worker<HeldOwner>>) -> (Waker, Receiver<()>) {
    let owner = Mutex::new(Some(worker));
    let (released, release) = mpsc::channel();
    let waker = Waker::from(Arc::new(OnDrop(move || {
        drop(lock(&owner).take());
        let _ = released.send(());
    })));
    (waker, release)
}

#[test]
fn a_timer_callback_can_release_the_last_owner_without_delaying_other_deadlines() {
    let (worker, release_owner, destroyed) = held_owner();
    let events = Arc::new(EventQueue::<()>::new(1));
    let mut expiry = events.wait(Duration::from_millis(40));
    let (waker, released) = owner_releasing_waker(worker);
    assert!(poll(&mut expiry, &waker).is_pending());
    drop(waker);

    let other = Arc::new(EventQueue::<()>::new(1));
    let mut independent = other.wait(Duration::from_millis(80));
    let (expired, expiration) = mpsc::channel();
    let waker = Waker::from(Arc::new(OnDrop(move || {
        let _ = expired.send(());
    })));
    assert!(poll(&mut independent, &waker).is_pending());
    drop(waker);
    // Shutdown is still held, so neither callback can rely on it completing.
    released.recv_timeout(Duration::from_secs(1)).unwrap();
    expiration.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(destroyed.try_recv().is_err());
    release_owner.send(()).unwrap();
    destroyed.recv_timeout(Duration::from_secs(1)).unwrap();
}

#[test]
fn a_probe_completion_can_release_the_last_owner_without_delaying_the_next_probe() {
    let (worker, release_owner, destroyed) = held_owner();
    let (reply, mut response) = operation::<()>();
    let (waker, released) = owner_releasing_waker(worker);
    assert!(poll(&mut response, &waker).is_pending());
    drop(waker);
    let probes = ProbeQueue::new(2);
    probes
        .submit(Box::new(move || reply.complete(Ok(()))))
        .unwrap();
    let (next, ran_next) = mpsc::channel();
    probes
        .submit(Box::new(move || {
            let _ = next.send(());
        }))
        .unwrap();
    released.recv_timeout(Duration::from_secs(1)).unwrap();
    ran_next.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(destroyed.try_recv().is_err());
    release_owner.send(()).unwrap();
    destroyed.recv_timeout(Duration::from_secs(1)).unwrap();
}

#[test]
fn owners_whose_callbacks_release_each_others_last_handles_do_not_join_in_a_cycle() {
    let (first, release_first, first_destroyed) = held_owner();
    let (second, release_second, second_destroyed) = held_owner();
    release_first.send(()).unwrap();
    release_second.send(()).unwrap();
    let first_events = Arc::clone(&first.events);
    let second_events = Arc::clone(&second.events);
    let first_control = Arc::clone(&first.control);
    let second_control = Arc::clone(&second.control);

    // Hold the notification until both event queues own the cross references.
    let (trigger, triggered) = mpsc::channel();
    let notify = Arc::clone(&first_events);
    let sending = first
        .call(move |_| {
            triggered.recv_timeout(Duration::from_secs(5)).unwrap();
            notify.push(Ok(()));
            Ok(())
        })
        .submit();
    let mut first_event = first_events.wait(Duration::from_secs(60));
    let mut second_event = second_events.wait(Duration::from_secs(60));
    let (first_waker, second_released) = owner_releasing_waker(second);
    let (second_waker, first_released) = owner_releasing_waker(first);
    assert!(poll(&mut first_event, &first_waker).is_pending());
    assert!(poll(&mut second_event, &second_waker).is_pending());
    drop(first_waker);
    drop(second_waker);
    trigger.send(()).unwrap();
    first_released.recv_timeout(Duration::from_secs(1)).unwrap();
    second_released
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    first_destroyed
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    second_destroyed
        .recv_timeout(Duration::from_secs(1))
        .unwrap();
    block_on(sending).unwrap();
    block_on(first_control.request_close()).unwrap();
    block_on(second_control.request_close()).unwrap();
}
