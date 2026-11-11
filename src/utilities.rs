//! Checked interleaved audio inputs and native FLTP conversion helpers.
//!
//! Integer conversion uses an explicit reference level: NDI's floating-point
//! reference range is not a promise that received audio stays within [-1, 1].
//! These helpers do not normalize audio or preserve metadata in interleaved
//! descriptors, which have no metadata or timestamp fields.

use crate::{
    frames::validate_packed_audio_dimensions,
    sys::{self, bindings::*},
    AudioFormat, AudioFrame, Error, Result, Sender, NDI,
};

/// Full-scale integer audio level in dB above NDI's +4 dBu reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioReferenceLevel(i32);

impl AudioReferenceLevel {
    /// SDK recommendation for typical outgoing integer audio: 0 dB.
    pub const SEND: Self = Self(0);
    /// SDK recommendation for received integer audio: 20 dB of headroom.
    /// This lowers the apparent level in files but reduces clipping.
    pub const RECEIVE_HEADROOM: Self = Self(20);

    /// Select the native SDK's explicit reference-level value in dB.
    #[must_use]
    pub const fn from_db(db: i32) -> Self {
        Self(db)
    }

    /// Return the native reference level in dB.
    #[must_use]
    pub const fn db(self) -> i32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy)]
struct AudioShape {
    rate: i32,
    channels: i32,
    samples: i32,
    count: usize,
}

impl AudioShape {
    fn new(rate: i32, channels: i32, samples: i32) -> Result<Self> {
        if rate <= 0 {
            return Err(Error::InvalidFrame(
                "audio sample rate must be positive".into(),
            ));
        }
        // Share FLTP's checked stride and 64 MiB payload ceiling. This also
        // bounds conversion destinations for 16-bit source data.
        validate_packed_audio_dimensions(channels, samples)?;
        let count = (channels as usize)
            .checked_mul(samples as usize)
            .ok_or_else(|| Error::InvalidFrame("interleaved sample count overflow".into()))?;
        Ok(Self {
            rate,
            channels,
            samples,
            count,
        })
    }

    fn from_planar(frame: &AudioFrame) -> Result<Self> {
        if frame.format() != AudioFormat::FLTP {
            return Err(Error::InvalidFrame(
                "native audio conversion requires FLTP".into(),
            ));
        }
        Self::new(
            frame.sample_rate(),
            frame.num_channels(),
            frame.num_samples(),
        )
    }

    fn check_output(self, length: usize) -> Result<()> {
        if length < self.count {
            return Err(Error::InvalidFrame(format!(
                "audio destination has {length} samples, requires {}",
                self.count
            )));
        }
        Ok(())
    }

    fn check_planar(self, frame: &AudioFrame) -> Result<()> {
        Self::from_planar(frame)?;
        if frame.sample_rate() != self.rate
            || frame.num_channels() != self.channels
            || frame.num_samples() != self.samples
        {
            return Err(Error::InvalidFrame(
                "planar destination rate, channels and samples must match the interleaved source"
                    .into(),
            ));
        }
        Ok(())
    }

    fn allocate(self, timecode: i64) -> Result<AudioFrame> {
        AudioFrame::builder()
            .sample_rate(self.rate)
            .channels(self.channels)
            .samples(self.samples)
            .timecode(timecode)
            .build()
    }
}

macro_rules! interleaved_input {
    ($name:ident, $sample:ty, $raw:ident $(, $reference:ident)?) => {
        /// Borrowed, validated interleaved audio. Construction does not load NDI.
        ///
        /// Samples are ordered sample-first, then channel. The slice must contain
        /// exactly `channels * samples` elements; the equivalent FLTP allocation
        /// must fit the library's 64 MiB audio limit. Metadata is not represented.
        #[derive(Debug, Clone, Copy)]
        pub struct $name<'a> {
            shape: AudioShape,
            data: &'a [$sample],
            timecode: i64,
            $($reference: AudioReferenceLevel,)?
        }

        impl<'a> $name<'a> {
            /// Validate the header and complete initialized source slice.
            pub fn new(rate: i32, channels: i32, samples: i32, data: &'a [$sample] $(, $reference: AudioReferenceLevel)?) -> Result<Self> {
                let shape = AudioShape::new(rate, channels, samples)?;
                if data.len() != shape.count {
                    return Err(Error::InvalidFrame(format!("interleaved audio has {} samples, expected {}", data.len(), shape.count)));
                }
                Ok(Self { shape, data, timecode: NDIlib_send_timecode_synthesize, $($reference,)? })
            }

            /// Override the synthesized timecode, in native 100 ns units.
            #[must_use]
            pub fn with_timecode(mut self, timecode: i64) -> Self { self.timecode = timecode; self }
            /// Sample rate in Hz.
            pub fn sample_rate(&self) -> i32 { self.shape.rate }
            /// Number of interleaved channels.
            pub fn channels(&self) -> i32 { self.shape.channels }
            /// Number of samples per channel.
            pub fn samples(&self) -> i32 { self.shape.samples }
            /// Native timecode, including the synthesis sentinel when requested.
            pub fn timecode(&self) -> i64 { self.timecode }
            /// Complete initialized interleaved samples.
            pub fn data(&self) -> &'a [$sample] { self.data }

            fn raw(&self) -> $raw {
                $raw {
                    sample_rate: self.shape.rate,
                    no_channels: self.shape.channels,
                    no_samples: self.shape.samples,
                    timecode: self.timecode,
                    p_data: self.data.as_ptr().cast_mut(),
                    $($reference: self.$reference.db(),)?
                }
            }
        }
    };
}

interleaved_input!(
    InterleavedAudioI16,
    i16,
    NDIlib_audio_frame_interleaved_16s_t,
    reference_level
);
interleaved_input!(
    InterleavedAudioI32,
    i32,
    NDIlib_audio_frame_interleaved_32s_t,
    reference_level
);
interleaved_input!(
    InterleavedAudioF32,
    f32,
    NDIlib_audio_frame_interleaved_32f_t
);

/// Native audio conversion operations, resolved together from one live runtime.
///
/// Construction fails if any current v3 conversion helper is missing. It does
/// not silently substitute old v2 operations or Rust's existing f32 builder
/// conversion. Methods take caller-owned output storage for reuse.
pub struct AudioConverter {
    _ndi: NDI,
    audio: &'static sys::InterleavedAudio,
}

macro_rules! conversions {
    ($input:ident, $sample:ty, $raw:ident, $to:ident, $from:ident, $to_method:ident, $from_method:ident, $into_method:ident $(, $reference:ident)?) => {
        /// Convert FLTP samples into the active prefix of reusable interleaved storage.
        /// Returns the number of elements written; excess destination elements are
        /// untouched. Integer output can clip depending on the explicit reference
        /// level. On native failure the active prefix may be partially modified.
        pub fn $to_method(&self, source: &AudioFrame, destination: &mut [$sample] $(, $reference: AudioReferenceLevel)?) -> Result<usize> {
            let shape = AudioShape::from_planar(source)?;
            shape.check_output(destination.len())?;
            let source = source.to_raw();
            let mut destination = $raw {
                sample_rate: shape.rate, no_channels: shape.channels, no_samples: shape.samples,
                timecode: source.timecode, p_data: destination.as_mut_ptr(),
                $($reference: $reference.db(),)?
            };
            // SAFETY: Both typed buffers cover the validated dimensions, do not
            // alias, and remain live throughout this synchronous conversion.
            if !unsafe { self.audio.$to(&source, &mut destination) } {
                return Err(Error::NativeOperationFailed { operation: stringify!($to_method) });
            }
            Ok(shape.count)
        }

        /// Allocate a tightly packed FLTP frame with the source timecode.
        /// Interleaved headers have no metadata or timestamp; those output fields
        /// use the ordinary AudioFrame defaults.
        pub fn $from_method(&self, source: &$input<'_>) -> Result<AudioFrame> {
            let mut destination = source.shape.allocate(source.timecode)?;
            self.$into_method(source, &mut destination)?;
            Ok(destination)
        }

        /// Replace samples in an existing FLTP frame with matching dimensions and
        /// sample rate. Its timecode, metadata and timestamp remain unchanged.
        /// On native failure sample storage may be partially modified.
        pub fn $into_method(&self, source: &$input<'_>, destination: &mut AudioFrame) -> Result<()> {
            source.shape.check_planar(destination)?;
            let source = source.raw();
            let mut raw = destination.to_raw();
            raw.p_data = destination.data_mut().as_mut_ptr().cast();
            // SAFETY: Source and destination have matching checked dimensions;
            // the exclusively borrowed output is initialized and correctly aligned.
            if !unsafe { self.audio.$from(&source, &mut raw) } {
                return Err(Error::NativeOperationFailed { operation: stringify!($into_method) });
            }
            Ok(())
        }
    };
}

impl AudioConverter {
    /// Resolve the complete current conversion group before exposing operations.
    pub fn new(ndi: &NDI) -> Result<Self> {
        Ok(Self {
            audio: ndi.table()?,
            _ndi: ndi.clone(),
        })
    }

    conversions!(
        InterleavedAudioI16,
        i16,
        NDIlib_audio_frame_interleaved_16s_t,
        NDIlib_util_audio_to_interleaved_16s_v3,
        NDIlib_util_audio_from_interleaved_16s_v3,
        to_interleaved_i16,
        from_interleaved_i16,
        from_interleaved_i16_into,
        reference_level
    );
    conversions!(
        InterleavedAudioI32,
        i32,
        NDIlib_audio_frame_interleaved_32s_t,
        NDIlib_util_audio_to_interleaved_32s_v3,
        NDIlib_util_audio_from_interleaved_32s_v3,
        to_interleaved_i32,
        from_interleaved_i32,
        from_interleaved_i32_into,
        reference_level
    );
    conversions!(
        InterleavedAudioF32,
        f32,
        NDIlib_audio_frame_interleaved_32f_t,
        NDIlib_util_audio_to_interleaved_32f_v3,
        NDIlib_util_audio_from_interleaved_32f_v3,
        to_interleaved_f32,
        from_interleaved_f32,
        from_interleaved_f32_into
    );
}

macro_rules! send_interleaved {
    ($method:ident, $input:ident, $send:ident) => {
        /// Synchronously send validated interleaved audio using the selected SDK.
        /// Source samples may be reused when this method returns. The interleaved
        /// SDK descriptors do not carry metadata or receive timestamps.
        pub fn $method(&self, frame: &$input<'_>) -> Result<()> {
            let ndi = self.runtime();
            let send = ndi.table::<sys::InterleavedSend>()?;
            let raw = frame.raw();
            // SAFETY: the sender instance is live for this borrow and the
            // validated source slice outlives the synchronous call.
            unsafe { send.$send(self.native_instance(), &raw) };
            Ok(())
        }
    };
}

impl Sender {
    send_interleaved!(
        send_audio_interleaved_i16,
        InterleavedAudioI16,
        NDIlib_util_send_send_audio_interleaved_16s
    );
    send_interleaved!(
        send_audio_interleaved_i32,
        InterleavedAudioI32,
        NDIlib_util_send_send_audio_interleaved_32s
    );
    send_interleaved!(
        send_audio_interleaved_f32,
        InterleavedAudioF32,
        NDIlib_util_send_send_audio_interleaved_32f
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaved_validation_rejects_bad_dimensions_lengths_and_limits() {
        for (rate, channels, samples) in [
            (0, 2, 1),
            (48000, 0, 1),
            (48000, 2, 0),
            (48000, -1, 1),
            (48000, i32::MAX, i32::MAX),
            (48000, 1, 16_777_217),
        ] {
            assert!(InterleavedAudioF32::new(rate, channels, samples, &[]).is_err());
            assert!(InterleavedAudioI16::new(
                rate,
                channels,
                samples,
                &[],
                AudioReferenceLevel::SEND
            )
            .is_err());
            assert!(InterleavedAudioI32::new(
                rate,
                channels,
                samples,
                &[],
                AudioReferenceLevel::SEND
            )
            .is_err());
        }
        assert!(InterleavedAudioF32::new(48000, 2, 2, &[0.0; 3]).is_err());
        assert!(InterleavedAudioF32::new(48000, 2, 2, &[0.0; 5]).is_err());
        let samples = [0.0f32; 4];
        let frame = InterleavedAudioF32::new(48000, 2, 2, &samples).unwrap();
        assert_eq!(frame.data().as_ptr(), samples.as_ptr());
        assert_eq!(frame.channels(), 2);
        assert_eq!(frame.samples(), 2);
        assert_eq!(AudioReferenceLevel::SEND.db(), 0);
        assert_eq!(AudioReferenceLevel::RECEIVE_HEADROOM.db(), 20);
    }

    #[test]
    fn native_float_conversion_preserves_channel_order_and_reuses_storage() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let converter = AudioConverter::new(&ndi).unwrap();
        let samples = [0.125, -0.5, 0.25, -0.25, 0.5, -0.125];
        let input = InterleavedAudioF32::new(48000, 2, 3, &samples)
            .unwrap()
            .with_timecode(1234);
        let frame = converter.from_interleaved_f32(&input).unwrap();
        assert_eq!(frame.data(), &[0.125, 0.25, 0.5, -0.5, -0.25, -0.125]);
        assert_eq!(frame.timecode(), 1234);
        assert_eq!(frame.metadata(), None);
        let mut output = [99.0; 8];
        assert_eq!(
            converter.to_interleaved_f32(&frame, &mut output).unwrap(),
            samples.len()
        );
        assert_eq!(&output[..6], &samples);
        assert_eq!(&output[6..], &[99.0; 2]);
        assert!(converter.to_interleaved_f32(&frame, &mut [0.0; 5]).is_err());

        let mut destination = AudioFrame::builder()
            .channels(2)
            .samples(3)
            .timecode(5678)
            .timestamp(9012)
            .metadata("<keep/>")
            .build()
            .unwrap();
        let pointer = destination.data().as_ptr();
        converter
            .from_interleaved_f32_into(&input, &mut destination)
            .unwrap();
        assert_eq!(destination.data().as_ptr(), pointer);
        assert_eq!(destination.data(), frame.data());
        assert_eq!(destination.timecode(), 5678);
        assert_eq!(destination.timestamp(), 9012);
        assert_eq!(destination.metadata(), Some("<keep/>"));
        let mut wrong_dimensions = AudioFrame::builder()
            .channels(1)
            .samples(6)
            .build()
            .unwrap();
        assert!(converter
            .from_interleaved_f32_into(&input, &mut wrong_dimensions)
            .is_err());
    }

    #[test]
    fn native_integer_conversion_respects_explicit_reference_level() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let converter = AudioConverter::new(&ndi).unwrap();
        let i16_samples = [8192i16, -16384, 16384, -8192];
        let i16_input =
            InterleavedAudioI16::new(48000, 2, 2, &i16_samples, AudioReferenceLevel::SEND).unwrap();
        let frame = converter.from_interleaved_i16(&i16_input).unwrap();
        // Native integer conversion scales against the positive integer maximum,
        // rather than assuming that 32768 maps exactly to reference amplitude.
        for (value, integer) in frame.data().iter().zip([8192i16, 16384, -16384, -8192]) {
            assert!((*value - f32::from(integer) / f32::from(i16::MAX)).abs() < 1e-6);
        }
        let mut roundtrip = [0i16; 4];
        converter
            .to_interleaved_i16(&frame, &mut roundtrip, AudioReferenceLevel::SEND)
            .unwrap();
        // The SDK's i686 quantizer truncates these samples one count toward
        // zero; its x86_64 path rounds back exactly. Native conversion does
        // not promise a bit-exact integer roundtrip through floating point.
        for (actual, expected) in roundtrip.iter().zip(i16_samples) {
            assert!((i32::from(*actual) - i32::from(expected)).abs() <= 1);
        }
        let mut headroom = [0i16; 4];
        converter
            .to_interleaved_i16(&frame, &mut headroom, AudioReferenceLevel::RECEIVE_HEADROOM)
            .unwrap();
        for (normal, attenuated) in roundtrip.iter().zip(headroom) {
            assert!((f64::from(*normal) / 10.0 - f64::from(attenuated)).abs() <= 1.0);
        }

        let i32_samples = [536_870_912i32, -1_073_741_824, 1_073_741_824, -536_870_912];
        let i32_input =
            InterleavedAudioI32::new(48000, 2, 2, &i32_samples, AudioReferenceLevel::SEND).unwrap();
        let frame = converter.from_interleaved_i32(&i32_input).unwrap();
        assert_eq!(frame.data(), &[0.25, 0.5, -0.5, -0.25]);
        let mut roundtrip = [0i32; 4];
        converter
            .to_interleaved_i32(&frame, &mut roundtrip, AudioReferenceLevel::SEND)
            .unwrap();
        assert_eq!(roundtrip, i32_samples);
    }

    #[test]
    fn native_interleaved_send_helpers_accept_checked_sources() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let name = crate::test_support::unique_name("grafton isolated utility validation");
        let options = crate::SenderOptions::builder(name)
            .clock_audio(false)
            .clock_video(false)
            .build();
        let sender = Sender::new(&ndi, &options).unwrap();
        let i16_input =
            InterleavedAudioI16::new(48000, 2, 2, &[1, -1, 2, -2], AudioReferenceLevel::SEND)
                .unwrap();
        let i32_input =
            InterleavedAudioI32::new(48000, 2, 2, &[1, -1, 2, -2], AudioReferenceLevel::SEND)
                .unwrap();
        let f32_input = InterleavedAudioF32::new(48000, 2, 2, &[0.1, -0.1, 0.2, -0.2]).unwrap();
        sender.send_audio_interleaved_i16(&i16_input).unwrap();
        sender.send_audio_interleaved_i32(&i32_input).unwrap();
        sender.send_audio_interleaved_f32(&f32_input).unwrap();
    }
}
