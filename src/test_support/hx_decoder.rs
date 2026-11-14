//! Probe the decoders used by native NDI|HX integration tests.

use super::{
    api::Ndi,
    policy::{skip, Requirement},
};

/// Whether a test of NDI|HX video that needs `decoder` can go ahead: `false`,
/// with the calling test reported as skipped, when the selected runtime is
/// known to have no decoder.
///
/// The runtime has no query for its decoder, so this looks where
/// [`Requirement::Hevc444Decoder`] says the runtime does. On Linux that is what
/// NDI 6.4.0.2 opens, the same for both codecs. It is not a vendor API: a
/// runtime that looks elsewhere needs this updated. On macOS and Windows there
/// is nothing to look at beforehand, so the answer there is `true`.
///
/// `true` is therefore no proof of a decoder. A test that receives NDI|HX video
/// also passes each frame to [`is_hx_placeholder`], and reports the decoder
/// missing through [`skip`] when the runtime delivered nothing else.
///
/// # Panics
///
/// Panics when `decoder` is not [`Requirement::H264Decoder`] or
/// [`Requirement::Hevc444Decoder`], and when no decoder is found and
/// `GRAFTON_NDI_REQUIRE_RUNTIME` promises `decoder`.
pub fn hx_decoder(ndi: &Ndi, decoder: Requirement) -> bool {
    assert!(
        matches!(
            decoder,
            Requirement::H264Decoder | Requirement::Hevc444Decoder
        ),
        "{decoder:?} is not an NDI|HX decoder"
    );
    match missing_hx_decoder(ndi) {
        None => true,
        Some(reason) => {
            skip(decoder, &reason);
            false
        }
    }
}

/// The macOS and Windows runtimes decode NDI|HX themselves, through what the
/// machine offers (see [`Requirement::Hevc444Decoder`]): the macOS runtime
/// decodes HEVC 4:4:4 on the hosted macOS runners, and the Windows runtime
/// delivers placeholders for it on the hosted Windows Server 2025 runners.
/// Neither runtime shows beforehand whether it can decode a codec, so the
/// receiving test decides from what it is given.
#[cfg(any(windows, target_os = "macos"))]
fn missing_hx_decoder(_: &Ndi) -> Option<String> {
    None
}

/// Why the runtime has no NDI|HX decoder on Linux, or `None` when it has one.
///
/// The runtime library's search path is `$ORIGIN`, and it asks the dynamic
/// loader for each name below. So a decoder counts when the copy in the
/// directory the runtime was loaded from loads, or when the loader of this
/// process can load it by name. A copy that is present but cannot load, for
/// want of `libcurl.so.4` for example, decodes nothing. Loading runs only the
/// library's initializers, which the runtime would run too, and it is
/// unloaded again at once.
#[cfg(target_os = "linux")]
fn missing_hx_decoder(ndi: &Ndi) -> Option<String> {
    use std::{ffi::OsString, path::Path};

    /// The NDI|HX Driver, then FFmpeg 7's decoder under the name of the
    /// driver's copy and under its own.
    const DECODERS: [&str; 3] = ["libndihx.so", "libavcodec-ndi.so.61", "libavcodec.so.61"];

    let directory = ndi.info().library_path().and_then(Path::parent);
    let mut failures = Vec::new();
    for name in DECODERS {
        let beside = directory
            .map(|directory| directory.join(name))
            .filter(|path| path.is_file());
        for candidate in beside
            .map(OsString::from)
            .into_iter()
            .chain([OsString::from(name)])
        {
            // SAFETY: loading runs the library's initializers. These are the
            // libraries the NDI runtime itself loads to decode NDI|HX; nothing
            // in them is called, and the handle is closed when it is dropped.
            match unsafe { libloading::Library::new(&candidate) } {
                Ok(_) => return None,
                // The loader's own message, which names the file it tried, is
                // the source of libloading's error.
                Err(error) => failures.push(match std::error::Error::source(&error) {
                    Some(cause) => cause.to_string(),
                    None => error.to_string(),
                }),
            }
        }
    }
    let places = directory.map_or_else(
        || "by name (a statically linked runtime has no directory)".to_owned(),
        |directory| {
            format!(
                "from beside the runtime in {} or by name",
                directory.display()
            )
        },
    );
    Some(format!(
        "the NDI runtime has no NDI|HX video decoder: none of {} loads {places} ({})",
        DECODERS.join(", "),
        failures.join("; ")
    ))
}

/// No NDI|HX decoder is documented for other targets.
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn missing_hx_decoder(_: &Ndi) -> Option<String> {
    Some(format!(
        "no NDI|HX video decoder is documented for {}",
        std::env::consts::OS
    ))
}
