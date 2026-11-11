//! Entry points that only the Advanced SDK headers declare.
//!
//! Those headers may not be redistributed, so their declarations are generated
//! at build time when an Advanced SDK is installed and the build sets
//! `ndi_has_advanced_header`. Everything that depends on that setting is in
//! this file:
//!
//! * [`advanced_tables!`] defines each table once. With the declarations a
//!   table is a [`native_table!`](super::table::native_table) checked against
//!   them; without, it is an uninhabited stand-in of the same name.
//! * [`AdvancedTable::of`] hands a wrapper its table, or the reason there is
//!   none: the runtime does not export it, or the build has no declarations.
//! * [`native!`] marks an expression that uses the declarations. Without them
//!   the expression is not compiled, and the stand-in table it names proves
//!   the position unreachable.
//!
//! A wrapper therefore reads the same in both builds: it takes its table when
//! it is constructed and calls through `native!`.

use super::Runtime;
use crate::{Error, Result};

#[cfg(ndi_has_advanced_header)]
use super::{
    advanced_bindings::*,
    bindings::*,
    table::{native_table, Resolver},
};
#[cfg(ndi_has_advanced_header)]
use crate::capabilities::SdkEdition;
#[cfg(ndi_has_advanced_header)]
use std::ffi::CStr;

/// A table of Advanced SDK entry points, or its stand-in in a build without
/// the Advanced SDK declarations.
pub(crate) trait AdvancedTable: Sized + 'static {
    /// The capability the table provides, as errors name it.
    const CAPABILITY: &'static str;

    /// The table resolved from `runtime`.
    ///
    /// # Errors
    ///
    /// [`Error::UnsupportedCapability`] when the runtime lacks one of the
    /// table's entry points or the build has no Advanced SDK declarations.
    fn of(runtime: &'static Runtime) -> Result<&'static Self>;
}

/// Native types from the Advanced-only headers that wrappers keep in fields.
#[cfg(ndi_has_advanced_header)]
pub(crate) use super::advanced_bindings::{
    NDIlib_avsync_instance_t as AvSyncHandle, NDIlib_frame_scatter_t as FrameScatter,
    NDIlib_genlock_instance_t as GenlockHandle,
};

/// Stand-ins for the stored native types. No value of them is ever created,
/// because every wrapper that stores one also needs a table to exist.
#[cfg(not(ndi_has_advanced_header))]
mod stored {
    pub(crate) type AvSyncHandle = *mut std::ffi::c_void;
    pub(crate) type GenlockHandle = *mut std::ffi::c_void;
    pub(crate) enum FrameScatter {}
}
#[cfg(not(ndi_has_advanced_header))]
pub(crate) use stored::{AvSyncHandle, FrameScatter, GenlockHandle};

/// Evaluate an expression that uses the Advanced SDK declarations.
///
/// `native!(table => body)` is `body` where the build has the declarations.
/// Elsewhere `body` is not compiled and `table`, a reference to an uninhabited
/// stand-in, proves that the position cannot be reached. The expression then
/// has no type of its own, so a binding whose later uses depend on its type
/// states it: `let handle: GenlockHandle = native!(..);`.
#[cfg(ndi_has_advanced_header)]
macro_rules! native {
    ($table:expr => $body:expr) => {{
        // A helper may take its table only as evidence that the declarations
        // exist; naming it here counts as its use.
        let _: &'static _ = $table;
        $body
    }};
}
#[cfg(not(ndi_has_advanced_header))]
macro_rules! native {
    ($table:expr => $body:expr) => {
        match *$table {}
    };
}
pub(crate) use native;

/// Why `runtime` has no table for `capability`: `symbol` is the first entry
/// point it does not export.
#[cfg(ndi_has_advanced_header)]
fn not_exported(capability: &str, runtime: &Runtime, symbol: &CStr) -> Error {
    let symbol = symbol.to_string_lossy();
    let reason = if runtime.info.edition() == SdkEdition::Advanced {
        format!("the selected NDI runtime does not export {symbol}")
    } else {
        format!(
            "the selected NDI runtime is not an Advanced SDK runtime: it does not export {symbol}"
        )
    };
    Error::UnsupportedCapability {
        capability: capability.into(),
        reason,
    }
}

/// The error every Advanced operation reports in a build without the
/// Advanced SDK declarations.
#[cfg(not(ndi_has_advanced_header))]
fn not_built(capability: &str) -> Error {
    Error::UnsupportedCapability {
        capability: capability.into(),
        reason: "grafton-ndi was built without the Advanced SDK headers; rebuild with \
                 NDI_SDK_DIR naming an installed NDI Advanced SDK"
            .into(),
    }
}

/// Define the Advanced tables.
///
/// Each entry names the table, its field in [`AdvancedApi`] and its capability,
/// then lists its entry points exactly as the Advanced headers declare them.
#[cfg(ndi_has_advanced_header)]
macro_rules! advanced_tables {
    ($(
        $(#[$meta:meta])*
        struct $Table:ident in $field:ident: $capability:literal {
            $(fn $name:ident($($argument:ident: $type:ty),* $(,)?) $(-> $return:ty)?;)+
        }
    )+) => {
        $(
            native_table! {
                $(#[$meta])*
                pub(crate) struct $Table from crate::sys::advanced_declarations {
                    $(fn $name($($argument: $type),*) $(-> $return)?;)+
                }
            }

            impl AdvancedTable for $Table {
                const CAPABILITY: &'static str = $capability;

                fn of(runtime: &'static Runtime) -> Result<&'static Self> {
                    runtime
                        .advanced
                        .$field
                        .as_ref()
                        .map_err(|symbol| not_exported(Self::CAPABILITY, runtime, symbol))
                }
            }
        )+

        /// Every Advanced table resolved from one implementation. A table is
        /// present only when the implementation exports all of its entry
        /// points; otherwise the first missing one is kept for the error.
        pub(crate) struct AdvancedApi {
            $($field: std::result::Result<$Table, &'static CStr>,)+
        }

        impl AdvancedApi {
            /// Resolve every table from `resolver`.
            ///
            /// # Errors
            ///
            /// Fails only when an entry point resolves outside the selected
            /// implementation; a missing entry point disables its table.
            pub(crate) fn load(resolver: &dyn Resolver) -> Result<Self> {
                Ok(Self {
                    $($field: $Table::load(resolver)?,)+
                })
            }
        }

        /// Every Advanced table by name, for the declaration coverage test.
        #[cfg(test)]
        const TABLES: &[(&str, &[&CStr])] = &[$((stringify!($Table), $Table::SYMBOLS),)+];
    };
}

/// Define the stand-ins for the Advanced tables. The entry points are parsed
/// and then ignored: their types exist only with the declarations.
#[cfg(not(ndi_has_advanced_header))]
macro_rules! advanced_tables {
    ($(
        $(#[$meta:meta])*
        struct $Table:ident in $field:ident: $capability:literal {
            $(fn $name:ident($($argument:ident: $type:ty),* $(,)?) $(-> $return:ty)?;)+
        }
    )+) => {
        $(
            $(#[$meta])*
            pub(crate) enum $Table {}

            impl AdvancedTable for $Table {
                const CAPABILITY: &'static str = $capability;

                fn of(_: &'static Runtime) -> Result<&'static Self> {
                    Err(not_built(Self::CAPABILITY))
                }
            }
        )+
    };
}

advanced_tables! {
    /// Sender creation with a per-instance JSON configuration.
    struct SenderConfiguration in sender_configuration: "Advanced sender configuration" {
        fn NDIlib_send_create_v2(p_create_settings: *const NDIlib_send_create_t, p_config_data: *const ::std::os::raw::c_char) -> NDIlib_send_instance_t;
    }

    /// Receiver creation with a per-instance JSON configuration.
    struct ReceiverConfiguration in receiver_configuration: "Advanced receiver configuration" {
        fn NDIlib_recv_create_v4(p_create_settings: *const NDIlib_recv_create_v3_t, p_config_data: *const ::std::os::raw::c_char) -> NDIlib_recv_instance_t;
    }

    /// Finder creation with a per-instance JSON configuration.
    struct FinderConfiguration in finder_configuration: "Advanced finder configuration" {
        fn NDIlib_find_create_v3(p_create_settings: *const NDIlib_find_create_t, p_config_data: *const ::std::os::raw::c_char) -> NDIlib_find_instance_t;
    }

    /// Source snapshots that carry each source's metadata.
    struct SourceMetadata in source_metadata: "source metadata" {
        fn NDIlib_find_get_current_sources_v2(p_instance: NDIlib_find_instance_t, p_no_sources: *mut u32) -> *const NDIlib_source_v2_t;
    }

    /// A sender's own contribution to its tally state.
    struct SenderTally in sender_tally: "sender tally override" {
        fn NDIlib_send_set_tally(p_instance: NDIlib_send_instance_t, p_tally: *const NDIlib_tally_t) -> bool;
    }

    /// Target frame size, bit rate and legacy quality for an encoder.
    struct EncoderGuidance in encoder_guidance: "encoder guidance" {
        fn NDIlib_send_get_target_frame_size(p_instance: NDIlib_send_instance_t, p_video_data: *const NDIlib_video_frame_v2_t) -> ::std::os::raw::c_int;
        fn NDIlib_send_get_target_bit_rate(p_instance: NDIlib_send_instance_t, p_video_data: *const NDIlib_video_frame_v2_t) -> ::std::os::raw::c_int;
        fn NDIlib_send_get_q_factor(p_instance: NDIlib_send_instance_t, p_video_data: *const NDIlib_video_frame_v2_t) -> ::std::os::raw::c_int;
    }

    /// Encoder quality together with the formats connected receivers accept.
    struct QualityPlanning in quality_planning: "encoder quality planning" {
        fn NDIlib_send_get_q_factor_v2(p_instance: NDIlib_send_instance_t, p_video_data: *const NDIlib_video_frame_v2_t, supports_new_format: bool, p_enable_short_dc: *mut bool) -> ::std::os::raw::c_int;
    }

    /// Keyframe demand from the receivers connected to a sender.
    struct KeyframeDemand in keyframe_demand: "keyframe demand" {
        fn NDIlib_send_is_keyframe_required(p_instance: NDIlib_send_instance_t, p_video_data: *const NDIlib_video_frame_v2_t) -> bool;
        fn NDIlib_send_wait_for_keyframe_request(p_instance: NDIlib_send_instance_t, timeout_in_ms: u32, p_video_data: *const NDIlib_video_frame_v2_t) -> bool;
    }

    /// A receiver's request for a keyframe from its source.
    struct KeyframeRequest in keyframe_request: "keyframe request" {
        fn NDIlib_recv_request_keyframe(p_instance: NDIlib_recv_instance_t);
    }

    /// Submission of compressed frames held in several blocks.
    struct ScatterSend in scatter_send: "scatter sending" {
        fn NDIlib_send_send_video_scatter(p_instance: NDIlib_send_instance_t, p_video_data: *const NDIlib_video_frame_v2_t, p_video_scatter: *const NDIlib_frame_scatter_t);
        fn NDIlib_send_send_video_scatter_async(p_instance: NDIlib_send_instance_t, p_video_data: *const NDIlib_video_frame_v2_t, p_video_scatter: *const NDIlib_frame_scatter_t);
        fn NDIlib_send_send_audio_scatter(p_instance: NDIlib_send_instance_t, p_audio_data: *const NDIlib_audio_frame_v3_t, p_audio_scatter: *const NDIlib_frame_scatter_t);
    }

    /// A receiver's choice between the program and preview streams.
    struct ReceiverBandwidthControl in receiver_bandwidth_control: "receiver bandwidth control" {
        fn NDIlib_recv_set_bandwidth(p_instance: NDIlib_recv_instance_t, bandwidth: NDIlib_recv_bandwidth_e) -> bool;
    }

    /// A receiver's choice of which streams it receives.
    struct ReceiverStreamControl in receiver_stream_control: "receiver stream selection" {
        fn NDIlib_recv_set_enabled_streams(p_instance: NDIlib_recv_instance_t, enable_video: bool, enable_audio: bool, enable_metadata: bool) -> bool;
    }

    /// Application-supplied memory for received frames.
    struct ReceiveAllocators in receive_allocators: "custom receive allocators" {
        fn NDIlib_recv_set_video_allocator(p_instance: NDIlib_recv_instance_t, p_opaque: *mut ::std::os::raw::c_void, p_allocator: NDIlib_video_alloc_t, p_deallocator: NDIlib_video_free_t);
        fn NDIlib_recv_set_audio_allocator(p_instance: NDIlib_recv_instance_t, p_opaque: *mut ::std::os::raw::c_void, p_allocator: NDIlib_audio_alloc_t, p_deallocator: NDIlib_audio_free_t);
    }

    /// Notification that an asynchronously sent video frame is no longer in use.
    struct SendCompletion in send_completion: "asynchronous send completion" {
        fn NDIlib_send_set_video_async_completion(p_instance: NDIlib_send_instance_t, p_opaque: *mut ::std::os::raw::c_void, p_deallocator: NDIlib_video_send_async_completion_t);
    }

    /// Matching of a receiver's audio to its video timestamps.
    struct AvSync in av_sync: "AV synchronization" {
        fn NDIlib_avsync_create(p_receiver: NDIlib_recv_instance_t) -> NDIlib_avsync_instance_t;
        fn NDIlib_avsync_destroy(p_avsync: NDIlib_avsync_instance_t);
        fn NDIlib_avsync_synchronize(p_avsync: NDIlib_avsync_instance_t, p_video_frame: *const NDIlib_video_frame_v2_t, p_audio_frame: *mut NDIlib_audio_frame_v3_t) -> NDIlib_avsync_ret_e;
        fn NDIlib_avsync_free_audio(p_avsync: NDIlib_avsync_instance_t, p_audio_frame: *mut NDIlib_audio_frame_v3_t);
    }

    /// Clocks locked to a reference source.
    struct Genlock in genlock: "genlock" {
        fn NDIlib_genlock_create(p_src_settings: *const NDIlib_source_t, p_config_data: *const ::std::os::raw::c_char) -> NDIlib_genlock_instance_t;
        fn NDIlib_genlock_destroy(p_instance: NDIlib_genlock_instance_t);
        fn NDIlib_genlock_connect(p_instance: NDIlib_genlock_instance_t, p_src_settings: *const NDIlib_source_t);
        fn NDIlib_genlock_is_active(p_instance: NDIlib_genlock_instance_t) -> bool;
        fn NDIlib_genlock_wait_video(p_instance: NDIlib_genlock_instance_t, p_video_data: *const NDIlib_video_frame_v2_t) -> bool;
        fn NDIlib_genlock_wait_audio(p_instance: NDIlib_genlock_instance_t, p_audio_data: *const NDIlib_audio_frame_v3_t) -> bool;
    }

    /// Keyboard, mouse, touch and clipboard control of a source.
    struct Kvm in kvm: "KVM control" {
        fn NDIlib_recv_kvm_is_supported(p_instance: NDIlib_recv_instance_t) -> bool;
        fn NDIlib_recv_kvm_send_left_mouse_click(p_instance: NDIlib_recv_instance_t) -> bool;
        fn NDIlib_recv_kvm_send_middle_mouse_click(p_instance: NDIlib_recv_instance_t) -> bool;
        fn NDIlib_recv_kvm_send_right_mouse_click(p_instance: NDIlib_recv_instance_t) -> bool;
        fn NDIlib_recv_kvm_send_left_mouse_release(p_instance: NDIlib_recv_instance_t) -> bool;
        fn NDIlib_recv_kvm_send_middle_mouse_release(p_instance: NDIlib_recv_instance_t) -> bool;
        fn NDIlib_recv_kvm_send_right_mouse_release(p_instance: NDIlib_recv_instance_t) -> bool;
        fn NDIlib_recv_kvm_send_vertical_mouse_wheel(p_instance: NDIlib_recv_instance_t, no_units: f32) -> bool;
        fn NDIlib_recv_kvm_send_horizontal_mouse_wheel(p_instance: NDIlib_recv_instance_t, no_units: f32) -> bool;
        fn NDIlib_recv_kvm_send_mouse_position(p_instance: NDIlib_recv_instance_t, posn: *const f32) -> bool;
        fn NDIlib_recv_kvm_send_clipboard_contents(p_instance: NDIlib_recv_instance_t, p_clipboard_contents: *const ::std::os::raw::c_char) -> bool;
        fn NDIlib_recv_kvm_send_touch_positions(p_instance: NDIlib_recv_instance_t, no_posns: ::std::os::raw::c_int, posns: *const f32) -> bool;
        fn NDIlib_recv_kvm_send_touch_finish(p_instance: NDIlib_recv_instance_t) -> bool;
        fn NDIlib_recv_kvm_send_keyboard_press(p_instance: NDIlib_recv_instance_t, key_sym_value: ::std::os::raw::c_int) -> bool;
        fn NDIlib_recv_kvm_send_keyboard_release(p_instance: NDIlib_recv_instance_t, key_sym_value: ::std::os::raw::c_int) -> bool;
    }
}

#[cfg(all(test, ndi_has_advanced_header))]
#[path = "advanced_coverage_tests.rs"]
mod coverage_tests;
