//! A runtime borrow alone cannot stand in for a native frame owner.

fn unrelated_runtime(ndi: &crate::Ndi) {
    let _ =
        unsafe { crate::capture::Guard::<crate::capture::VideoKind>::new(ndi, Default::default()) };
}
