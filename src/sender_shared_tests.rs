// Exercise the real shared owner with a synthetic native sender, including
// its consuming conversion and flush-on-drop, under both Miri borrow models.
use super::*;
use std::sync::Mutex;

type Trace = Arc<Mutex<Vec<&'static str>>>;

struct Native {
    trace: Trace,
    pending: Option<NDIlib_video_frame_v2_t>,
}

impl Native {
    fn release(&mut self) {
        if let Some(previous) = self.pending.take() {
            // SAFETY: every submitted frame remains owned by its actual
            // pipeline until this synchronizing call has finished reading it.
            let bytes = unsafe { std::slice::from_raw_parts(previous.p_data, 16) };
            assert_eq!(bytes, &[7; 16]);
            assert_eq!(unsafe { CStr::from_ptr(previous.p_metadata) }, c"<loan/>");
        }
    }
}

unsafe extern "C" fn submit(
    instance: NDIlib_send_instance_t,
    frame: *const NDIlib_video_frame_v2_t,
) {
    // SAFETY: create transfers this allocation to SenderResource, whose live
    // handle is the only caller and whose destroy callback consumes it once.
    let native = unsafe { &mut *instance.cast::<Native>() };
    native.release();
    if frame.is_null() {
        native.trace.lock().unwrap().push("flush");
    } else {
        native.pending = Some(unsafe { *frame });
        native.trace.lock().unwrap().push("submit");
    }
}

unsafe extern "C" fn destroy(instance: NDIlib_send_instance_t) {
    // SAFETY: the final SenderResource returns this native allocation exactly
    // once, after its pipeline has synchronized any remaining loan.
    let mut native = unsafe { Box::from_raw(instance.cast::<Native>()) };
    native.release();
    native.trace.lock().unwrap().push("destroy");
}

fn sender(trace: &Trace) -> Sender {
    let mut core = sys::Core::load(&sys::table::ExportsWhere(|_| true))
        .unwrap()
        .unwrap();
    core.NDIlib_send_send_video_async_v2 = submit;
    core.NDIlib_send_destroy = destroy;
    let ndi = Ndi::test_double(core);
    // SAFETY: the closure creates a unique native allocation for this runtime;
    // its supplied core contains the callbacks that consume and destroy it.
    unsafe {
        Sender::create(
            &ndi,
            &SenderOptions::new("synthetic shared owner"),
            |_, _| {
                let native = Box::new(Native {
                    trace: Arc::clone(trace),
                    pending: None,
                });
                crate::runtime::NativeCreation::plain(sys::table::for_test(
                    Box::into_raw(native).cast(), c"NDIlib_send_create"
                ))
            },
        )
    }
    .unwrap()
}

fn loan() -> VideoFrame {
    VideoFrame::builder()
        .resolution(2, 2)
        .metadata("<loan/>")
        .data(vec![7; 16])
        .build()
        .unwrap()
}

#[cfg(all(feature = "advanced", ndi_has_advanced_send_completion))]
#[test]
fn shared_conversion_refuses_without_touching_the_loan_then_recovers_it_once() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let mut sender = sender(&trace);
    let original = loan();
    let pixels = original.data().as_ptr();
    assert!(sender.submit_video(original).unwrap().is_none());
    let registration = sender.native_owner();
    let sender = sender.into_attached::<QueueAttached>().unwrap_err();
    assert_eq!(*trace.lock().unwrap(), ["submit"]);
    assert!(sender.native.video.test_state().in_flight.is_in_flight());
    drop(registration);
    let (sender, recovered) = sender.into_attached::<QueueAttached>().unwrap();
    let recovered = recovered.unwrap();
    assert_eq!(recovered.data().as_ptr(), pixels);
    assert_eq!(*trace.lock().unwrap(), ["submit", "flush"]);
    drop(sender);
    assert_eq!(*trace.lock().unwrap(), ["submit", "flush", "destroy"]);
    assert_eq!(recovered.data(), &[7; 16]);
}

#[cfg(all(feature = "advanced", ndi_has_advanced_send_completion))]
#[test]
fn shared_conversion_without_a_loan_does_not_flush() {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let (sender, recovered) = sender(&trace).into_attached::<QueueAttached>().unwrap();
    assert!(recovered.is_none());
    assert!(trace.lock().unwrap().is_empty());
    drop(sender);
    assert_eq!(*trace.lock().unwrap(), ["destroy"]);
}

#[test]
fn shared_drop_flushes_before_destruction_with_and_without_registration() {
    for retained in [false, true] {
        let trace = Arc::new(Mutex::new(Vec::new()));
        let mut sender = sender(&trace);
        sender.submit_video(loan()).unwrap();
        let registration = retained.then(|| sender.native_owner());
        drop(sender);
        assert_eq!(
            *trace.lock().unwrap(),
            if retained {
                vec!["submit", "flush"]
            } else {
                vec!["submit", "flush", "destroy"]
            }
        );
        drop(registration);
        assert_eq!(*trace.lock().unwrap(), ["submit", "flush", "destroy"]);
    }
}
