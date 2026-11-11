//! Every function the vendored SDK headers declare must be classified.

use std::{collections::BTreeMap, ffi::CStr};

use super::api::*;

/// Why a declared function has no table entry.
const SUPERSEDED: &str = "superseded by a newer entry point this crate calls instead";
const RECORDING: &str = "receiver-side recording control is not exposed by this crate";
const FUNCTION_TABLE: &str =
    "vendor function-table loader; entry points are resolved individually so a missing newer \
     function never invalidates older ones";

/// Declared functions this crate deliberately does not call, with the reason.
const UNWRAPPED: &[(&str, &str)] = &[
    ("NDIlib_find_create", SUPERSEDED),
    ("NDIlib_find_create2", SUPERSEDED),
    ("NDIlib_find_get_sources", SUPERSEDED),
    ("NDIlib_recv_create", SUPERSEDED),
    ("NDIlib_recv_create2", SUPERSEDED),
    ("NDIlib_recv_create_v2", SUPERSEDED),
    ("NDIlib_recv_capture", SUPERSEDED),
    ("NDIlib_recv_capture_v2", SUPERSEDED),
    ("NDIlib_recv_free_video", SUPERSEDED),
    ("NDIlib_recv_free_audio", SUPERSEDED),
    ("NDIlib_recv_free_audio_v2", SUPERSEDED),
    ("NDIlib_send_send_video", SUPERSEDED),
    ("NDIlib_send_send_video_async", SUPERSEDED),
    ("NDIlib_send_send_audio", SUPERSEDED),
    ("NDIlib_send_send_audio_v2", SUPERSEDED),
    ("NDIlib_framesync_capture_audio", SUPERSEDED),
    ("NDIlib_framesync_free_audio", SUPERSEDED),
    ("NDIlib_util_audio_to_interleaved_16s", SUPERSEDED),
    ("NDIlib_util_audio_to_interleaved_16s_v2", SUPERSEDED),
    ("NDIlib_util_audio_from_interleaved_16s", SUPERSEDED),
    ("NDIlib_util_audio_from_interleaved_16s_v2", SUPERSEDED),
    ("NDIlib_util_audio_to_interleaved_32s_v2", SUPERSEDED),
    ("NDIlib_util_audio_from_interleaved_32s_v2", SUPERSEDED),
    ("NDIlib_util_audio_to_interleaved_32f", SUPERSEDED),
    ("NDIlib_util_audio_to_interleaved_32f_v2", SUPERSEDED),
    ("NDIlib_util_audio_from_interleaved_32f", SUPERSEDED),
    ("NDIlib_util_audio_from_interleaved_32f_v2", SUPERSEDED),
    ("NDIlib_recv_recording_is_supported", RECORDING),
    ("NDIlib_recv_recording_start", RECORDING),
    ("NDIlib_recv_recording_stop", RECORDING),
    ("NDIlib_recv_recording_set_audio_level", RECORDING),
    ("NDIlib_recv_recording_is_recording", RECORDING),
    ("NDIlib_recv_recording_get_filename", RECORDING),
    ("NDIlib_recv_recording_get_error", RECORDING),
    ("NDIlib_recv_recording_get_times", RECORDING),
    ("NDIlib_v2_load", FUNCTION_TABLE),
    ("NDIlib_v3_load", FUNCTION_TABLE),
    ("NDIlib_v4_load", FUNCTION_TABLE),
    ("NDIlib_v4_5_load", FUNCTION_TABLE),
    ("NDIlib_v5_load", FUNCTION_TABLE),
    ("NDIlib_v6_load", FUNCTION_TABLE),
    ("NDIlib_v6_3_load", FUNCTION_TABLE),
    ("NDIlib_file_v1_load", FUNCTION_TABLE),
];

/// Every table of core entry points, by name.
const TABLES: &[(&str, &[&CStr])] = &[
    ("Core", Core::SYMBOLS),
    ("ReceiveDelay", ReceiveDelay::SYMBOLS),
    (
        "ReceiverConnectionMetadata",
        ReceiverConnectionMetadata::SYMBOLS,
    ),
    ("ReceiverStrings", ReceiverStrings::SYMBOLS),
    ("ReceiverWebControl", ReceiverWebControl::SYMBOLS),
    ("ReceiverSourceName", ReceiverSourceName::SYMBOLS),
    ("SenderMetadata", SenderMetadata::SYMBOLS),
    ("Timestamp", Timestamp::SYMBOLS),
    ("Routing", Routing::SYMBOLS),
    ("SenderAdvertising", SenderAdvertising::SYMBOLS),
    ("ReceiverAdvertising", ReceiverAdvertising::SYMBOLS),
    ("SenderDiscovery", SenderDiscovery::SYMBOLS),
    ("ReceiverDiscovery", ReceiverDiscovery::SYMBOLS),
    ("InterleavedAudio", InterleavedAudio::SYMBOLS),
    ("InterleavedSend", InterleavedSend::SYMBOLS),
    ("VideoConversion", VideoConversion::SYMBOLS),
];

/// The File library's tables, which exist only with the `file-io` feature.
#[cfg(feature = "file-io")]
const FILE_TABLES: &[(&str, &[&CStr])] = &[
    ("FilePlayback", super::FilePlayback::SYMBOLS),
    ("FileRecording", super::FileRecording::SYMBOLS),
];

/// The File library's functions, which only the `file-io` feature classifies.
fn is_file_function(name: &str) -> bool {
    name.starts_with("NDIlib_file_") && name != "NDIlib_file_v1_load"
}

/// Names of the foreign functions the vendored headers declare.
fn declared_functions() -> Vec<String> {
    let bindings = include_str!("declarations.rs");
    let mut names = Vec::new();
    for item in syn::parse_file(bindings)
        .expect("vendored bindings parse")
        .items
    {
        let syn::Item::ForeignMod(block) = item else {
            continue;
        };
        for item in block.items {
            if let syn::ForeignItem::Fn(function) = item {
                names.push(function.sig.ident.to_string());
            }
        }
    }
    names
}

#[test]
fn every_declared_function_is_in_one_table_or_explicitly_unwrapped() {
    // Where each classified name lives; inserting twice is a duplicate.
    let mut classified = BTreeMap::new();
    let mut classify = |name: String, owner: String| {
        if let Some(earlier) = classified.insert(name.clone(), owner.clone()) {
            panic!("{name} is classified twice: {earlier} and {owner}");
        }
    };
    #[cfg(feature = "file-io")]
    let tables = TABLES.iter().chain(FILE_TABLES);
    #[cfg(not(feature = "file-io"))]
    let tables = TABLES.iter();
    for (table, symbols) in tables {
        for symbol in *symbols {
            classify(
                symbol.to_str().unwrap().to_owned(),
                format!("table {table}"),
            );
        }
    }
    #[cfg(feature = "file-io")]
    let unwrapped = UNWRAPPED.iter().chain(super::file::UNWRAPPED);
    #[cfg(not(feature = "file-io"))]
    let unwrapped = UNWRAPPED.iter();
    for (name, reason) in unwrapped {
        assert!(!reason.is_empty());
        classify((*name).to_owned(), "the unwrapped list".to_owned());
    }

    let declared = declared_functions();
    assert!(declared.len() > Core::SYMBOLS.len());
    for name in &declared {
        if cfg!(not(feature = "file-io")) && is_file_function(name) {
            continue;
        }
        assert!(
            classified.remove(name).is_some(),
            "{name} is declared by the SDK headers but is in no table and not in UNWRAPPED"
        );
    }
    assert!(
        classified.is_empty(),
        "classified names the SDK headers do not declare: {classified:?}"
    );
}
