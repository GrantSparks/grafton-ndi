//! Private discovery keys must not add Borrow inference choices to Source.
use grafton_ndi::Source;
use std::borrow::Borrow;

fn main() {
    let source = Source::new("STUDIO (Camera)").unwrap();
    let borrowed = source.borrow();
    assert_eq!(borrowed.name(), "STUDIO (Camera)");
}
