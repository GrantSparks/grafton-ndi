//! `use grafton_ndi::*` must not hide the `tokio` crate: the crate root
//! exports no item named `tokio`, so the glob import and `use tokio::...`
//! resolve without ambiguity.

#![cfg(feature = "tokio")]

use grafton_ndi::*;
use tokio::runtime::Builder;

#[test]
fn glob_import_leaves_the_tokio_crate_nameable() {
    let runtime = Builder::new_current_thread().build().unwrap();
    assert_eq!(runtime.block_on(async { 1 + 1 }), 2);
    // An item from the glob import is still usable.
    assert_eq!(PixelFormat::Bgra, PixelFormat::Bgra);
    // The async receiver is reached through its module path.
    let _ = std::mem::size_of::<asynchronous::tokio::AsyncReceiver>();
}
