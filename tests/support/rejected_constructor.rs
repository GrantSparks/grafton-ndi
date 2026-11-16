//! A deliberately unusable library whose constructor records that it ran.
extern "C" fn mark_load() {
    if let Some(path) = std::env::var_os("GRAFTON_REJECTED_CONSTRUCTOR_MARKER") {
        std::fs::write(path, b"native constructor ran\n").unwrap();
    }
}
#[used]
#[cfg_attr(windows, link_section = ".CRT$XCU")]
#[cfg_attr(target_vendor = "apple", link_section = "__DATA,__mod_init_func")]
#[cfg_attr(all(unix, not(target_vendor = "apple")), link_section = ".init_array")]
static INITIALIZE: extern "C" fn() = mark_load;
