//! A library that defines one NDI entry point without being an NDI runtime.
//!
//! The loader tests link a fake runtime against it: a lookup through that
//! runtime's handle then finds this definition, which the loader must reject
//! because the runtime itself does not define it.

#[no_mangle]
pub extern "C" fn NDIlib_send_set_failover(
    _instance: *mut std::ffi::c_void,
    _failover_source: *const std::ffi::c_void,
) {
}
