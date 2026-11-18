//! Native ownership and recorder calls.

use std::{marker::PhantomData, ptr::NonNull, rc::Rc};

use super::{ledger::StopOwed, CheckedRecorderOptions};
use crate::{
    file::{
        out_param,
        types::{track_flags, FileText},
        FileBound, RecorderStats, RecordingTarget, RecordingTimecode, SourceFormat, SourceSpec,
    },
    frames::MetadataPartsExt,
    sys::{self, bindings::*},
    Error, MetadataSource, Result,
};

// Every string and structure passed to the File library is kept only for the
// call that receives it. The SDK headers give a lifetime for each pointer the
// library returns and none for any it is given, and say of the source strings
// the recorder reports that the recorder owns them: what it is given, it
// copies. `sdk_tests` holds the library to that, by overwriting each string as
// soon as the call it was passed to returns.

fn raw_source(source: &SourceSpec) -> NDIlib_file_recorder_source_t {
    let pointer =
        |text: &Option<FileText>| text.as_ref().map_or(std::ptr::null(), |text| text.as_ptr());
    NDIlib_file_recorder_source_t {
        p_ndi_name: pointer(&source.name),
        p_url_address: pointer(&source.url),
    }
}

pub(super) struct NativeRecorder {
    recording: FileBound<sys::FileRecording>,
    handle: NonNull<NDIlib_file_recorder_instance_type>,
    // Neither the NDI File Format API documentation nor
    // Processing.NDI.Lib.File.Recorder.h says on which threads a recorder may
    // be created, used and destroyed, or whether calls on one recorder may
    // overlap; the header mentions concurrent recorders only for the memory
    // each one takes. Without that contract a recorder stays on its creating
    // thread, and AsyncFileRecorder creates, uses and destroys one on a
    // thread of its own.
    _thread: PhantomData<Rc<()>>,
}

impl NativeRecorder {
    pub(super) fn capture(&mut self, wait_ms: u32) -> Result<Option<NDIlib_file_recorder_event_t>> {
        out_param::capture_event(
            |event| {
                // SAFETY: the recorder is live and the helper keeps guarded
                // event storage writable through the call.
                unsafe {
                    self.recording.NDIlib_file_recorder_capture_event(
                        self.handle.as_ptr(),
                        event,
                        wait_ms,
                    )
                }
            },
            |event| event.type_,
            NDIlib_file_recorder_event_type_e_NDIlib_file_recorder_event_type_none,
            "the recorder returned one event type and stored another",
        )
    }

    pub(super) fn stop(&mut self, _stop: StopOwed) {
        // SAFETY: the recorder is live and the consumed token is the one
        // obligation minted by its successful native start.
        unsafe {
            self.recording
                .NDIlib_file_recorder_stop(self.handle.as_ptr())
        };
    }

    pub(super) fn is_recording(&self) -> bool {
        // SAFETY: the recorder is live for this borrow.
        unsafe {
            self.recording
                .NDIlib_file_recorder_is_recording(self.handle.as_ptr())
        }
    }

    pub(super) fn dropped_event_count(&self) -> u64 {
        // SAFETY: the recorder is live for this borrow.
        unsafe {
            self.recording
                .NDIlib_file_recorder_get_dropped_event_count(self.handle.as_ptr())
        }
    }

    pub(super) fn stats(&self) -> Result<Option<RecorderStats>> {
        out_param::checked_fill(|stats| {
            // SAFETY: the recorder is live and the helper keeps guarded stats
            // storage writable through the call.
            unsafe {
                self.recording
                    .NDIlib_file_recorder_get_stats(self.handle.as_ptr(), stats)
            }
        })
        .map(|stats| stats.map(|stats| RecorderStats::from_sdk(&stats)))
    }

    pub(super) fn source_format(&self) -> Result<Option<SourceFormat>> {
        out_param::checked_fill(|format| {
            // SAFETY: the recorder is live and the helper keeps guarded
            // format storage writable through the call.
            unsafe {
                self.recording
                    .NDIlib_file_recorder_get_source_format(self.handle.as_ptr(), format)
            }
        })?
        .map(|format| SourceFormat::from_sdk(&format).map_err(Error::UnexpectedSdkResult))
        .transpose()
    }

    pub(super) fn create(
        recording: FileBound<sys::FileRecording>,
        options: &CheckedRecorderOptions,
    ) -> Result<Self> {
        let resolved = &options.resolved;
        let settings = NDIlib_file_recorder_create_t {
            source_to_connect_to: raw_source(&options.source),
            track_flags: track_flags(options.tracks),
            record_float_audio_planar_32f: options.planar_audio,
            disable_auto_rollover_on_format_change: !resolved.format_change_rollover,
            p_output_filename_pattern: resolved
                .pattern
                .as_ref()
                .map_or(std::ptr::null(), |pattern| pattern.as_ptr()),
            disable_proxy_track: !options.proxy,
            rollover_max_file_size_bytes: options.rollover_size,
        };
        // SAFETY: `settings` and the strings it points to, owned by `options`
        // and `resolved`, are valid for the call.
        let handle =
            unsafe { recording.named().NDIlib_file_recorder_create(&settings) }.created()?;
        Ok(Self {
            recording,
            handle,
            _thread: PhantomData,
        })
    }

    pub(super) fn recording(&self) -> FileBound<sys::FileRecording> {
        self.recording.clone()
    }

    pub(super) fn start(
        &mut self,
        target: &RecordingTarget,
        timecode: RecordingTimecode,
    ) -> Result<()> {
        let prefix = target.prefix.as_ptr();
        // SAFETY: the recorder is live, and `prefix` points into `target`,
        // which the caller lends for longer than the call.
        let accepted = unsafe {
            match timecode {
                RecordingTimecode::Preserve => self
                    .recording
                    .named()
                    .NDIlib_file_recorder_start(self.handle.as_ptr(), prefix),
                RecordingTimecode::GenerateFrom(start) => self
                    .recording
                    .named()
                    .NDIlib_file_recorder_start_with_timecode(
                        self.handle.as_ptr(),
                        prefix,
                        start.ticks(),
                    ),
            }
        };
        accepted.performed()
    }

    pub(super) fn set_source(&mut self, source: &SourceSpec) -> Result<()> {
        let raw = raw_source(source);
        // SAFETY: the recorder is live, and `raw` and the strings it points
        // to, owned by `source`, are valid for the call.
        let accepted = unsafe {
            self.recording
                .named()
                .NDIlib_file_recorder_set_source(self.handle.as_ptr(), &raw)
        };
        accepted.performed()
    }

    pub(super) fn rollover(&mut self, target: &RecordingTarget) -> Result<()> {
        // SAFETY: the recorder is live, and the prefix points into `target`,
        // which the caller lends for longer than the call. The SDK copies it.
        let accepted = unsafe {
            self.recording
                .named()
                .NDIlib_file_recorder_rollover(self.handle.as_ptr(), target.prefix.as_ptr())
        };
        accepted.performed()
    }

    pub(super) fn inject_metadata<S: MetadataSource + ?Sized>(
        &mut self,
        metadata: &S,
    ) -> Result<()> {
        let frame = metadata.metadata_raw();
        // SAFETY: the recorder is live. `frame.p_data` points to the
        // NUL-terminated text that `metadata` owns, `frame.length` bytes with
        // the terminator, within the crate's metadata cap. `metadata` is
        // borrowed for the whole of this synchronous call, and
        // `Processing.NDI.Lib.File.Recorder.h` has the SDK take the frame as
        // `const` and copy the payload before returning, so it neither keeps
        // nor writes through `p_data`.
        let injected = unsafe {
            self.recording
                .named()
                .NDIlib_file_recorder_inject_metadata(self.handle.as_ptr(), &frame)
        };
        injected.performed()
    }
}

impl Drop for NativeRecorder {
    fn drop(&mut self) {
        // SAFETY: the recorder is live and is destroyed exactly once, on the
        // thread that created it.
        unsafe {
            self.recording
                .NDIlib_file_recorder_destroy(self.handle.as_ptr())
        };
    }
}
