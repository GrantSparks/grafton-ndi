//! Shared plumbing for the Advanced wrappers. Declarations from the Advanced-only
//! headers are generated into OUT_DIR at build time and never checked in.

use crate::{Error, Result, NDI};
#[cfg(not(ndi_has_advanced_header))]
use std::ffi::c_void;

// The SDK's C enums are signed on MSVC and unsigned on other supported targets.
// FourCCs are 32-bit bit patterns: protection uses the high bit of every byte.
// Reinterpret those bits at the ABI boundary instead of using numeric conversion.
pub(crate) const fn video_fourcc_bits(
    value: crate::advanced::ffi::native::NDIlib_FourCC_video_type_e,
) -> u32 {
    u32::from_ne_bytes(value.to_ne_bytes())
}

pub(crate) const fn video_fourcc_native(
    bits: u32,
) -> crate::advanced::ffi::native::NDIlib_FourCC_video_type_e {
    crate::advanced::ffi::native::NDIlib_FourCC_video_type_e::from_ne_bytes(bits.to_ne_bytes())
}

pub(crate) const fn audio_fourcc_bits(
    value: crate::advanced::ffi::native::NDIlib_FourCC_audio_type_e,
) -> u32 {
    u32::from_ne_bytes(value.to_ne_bytes())
}

pub(crate) const fn audio_fourcc_native(
    bits: u32,
) -> crate::advanced::ffi::native::NDIlib_FourCC_audio_type_e {
    crate::advanced::ffi::native::NDIlib_FourCC_audio_type_e::from_ne_bytes(bits.to_ne_bytes())
}

/// The native declarations the Advanced wrappers use: the vendored common
/// types and, when the build found the Advanced SDK, the locally generated ones.
pub(crate) mod native {
    pub(crate) use crate::sys::bindings::*;

    #[cfg(ndi_has_advanced_header)]
    pub(crate) use crate::sys::advanced_bindings::*;
}

#[cfg(ndi_has_advanced_header)]
pub(crate) type Scatter = crate::advanced::ffi::native::NDIlib_frame_scatter_t;
#[cfg(ndi_has_advanced_header)]
pub(crate) type SourceV2 = crate::advanced::ffi::native::NDIlib_source_v2_t;

pub(crate) unsafe fn required<T: Copy>(ndi: &NDI, name: &'static [u8]) -> Result<T> {
    if !cfg!(ndi_has_advanced_header) {
        return Err(build_unavailable());
    }
    if ndi.info().edition() != crate::capabilities::SdkEdition::Advanced {
        return Err(Error::UnsupportedCapability {
            capability: "Advanced SDK".into(),
            reason: "The selected runtime edition is Standard".into(),
        });
    }
    // SAFETY: each caller supplies the exact native function-pointer signature.
    unsafe { ndi.resolve_optional(name)? }.ok_or_else(|| Error::UnsupportedCapability {
        capability: String::from_utf8_lossy(name.strip_suffix(&[0]).unwrap_or(name)).into_owned(),
        reason: "The selected NDI runtime does not export this operation".into(),
    })
}

pub(crate) fn build_unavailable() -> Error {
    Error::UnsupportedCapability {
        capability: "Advanced SDK".into(),
        reason: "BuildUnavailable: rebuild with authorized local Advanced SDK headers".into(),
    }
}

#[cfg(not(ndi_has_advanced_header))]
pub(crate) type Opaque = *mut c_void;

pub(crate) fn invalid(message: &str) -> Error {
    Error::InvalidFrame(message.into())
}
