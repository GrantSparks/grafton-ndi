//! A deliberately unusable library whose constructor records that it ran.
extern "C" fn mark_load() {
    if let Some(path) = std::env::var_os("GRAFTON_REJECTED_CONSTRUCTOR_MARKER") {
        std::fs::write(path, b"native constructor ran\n").unwrap();
    }
}
#[used]
#[link_section = ".init_array"]
static INITIALIZE: extern "C" fn() = mark_load;
