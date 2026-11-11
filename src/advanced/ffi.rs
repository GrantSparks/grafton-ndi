//! Name-based lookup of Advanced entry points for the routing and discovery
//! modules, which create their configured objects through it.

use crate::{Error, Result, NDI};

/// Resolve the Advanced entry point `name` from the selected runtime.
///
/// # Safety
///
/// `T` must be the exact C function-pointer type the SDK declares for `name`.
pub(crate) unsafe fn required<T: Copy>(ndi: &NDI, name: &'static [u8]) -> Result<T> {
    let capability =
        || String::from_utf8_lossy(name.strip_suffix(&[0]).unwrap_or(name)).into_owned();
    if !cfg!(ndi_has_advanced_header) {
        return Err(Error::UnsupportedCapability {
            capability: capability(),
            reason: "grafton-ndi was built without the Advanced SDK headers; rebuild with \
                     NDI_SDK_DIR naming an installed NDI Advanced SDK"
                .into(),
        });
    }
    // SAFETY: forwarded to the caller's obligation.
    unsafe { ndi.resolve_optional(name)? }.ok_or_else(|| Error::UnsupportedCapability {
        capability: capability(),
        reason: "the selected NDI runtime does not export this Advanced SDK entry point".into(),
    })
}
