//! Checked interleaved audio inputs and native FLTP conversion helpers.
//!
//! Integer conversion uses an explicit reference level: NDI's floating-point
//! reference range is not a promise that received audio stays within [-1, 1].
//! These helpers do not normalize audio or preserve metadata in interleaved
//! descriptors, which have no metadata or timestamp fields.

use crate::{
    frames::AudioPartsExt,
    frames::{AudioSource, ValidatedAudioLayout},
    sender::AudioSendMode,
    sys::{self, bindings::*},
    AudioFrame, Error, Ndi, Result, Sender, Timecode,
};

/// Full-scale integer audio level in dB above NDI's +4 dBu reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// Checked FLTP dimensions shared by interleaved inputs and planar frames.
#[derive(Debug, Clone, Copy)]
struct AudioShape(ValidatedAudioLayout);

impl AudioShape {
    /// The shape of `len` interleaved samples in `num_channels` channels.
    fn interleaved(sample_rate: u32, num_channels: u32, len: usize) -> Result<Self> {
        // Check channels through the same count rule as planar audio before
        // dividing. An empty source then reaches that rule as zero samples.
        let channels = crate::frames::validate_audio_count(
            num_channels.into(),
            crate::AudioCountKind::Channels,
        )?;
        if !len.is_multiple_of(channels.to_usize()) {
            return Err(Error::InvalidFrame(
                crate::FrameDefect::InterleavedAudioLength {
                    actual: len,
                    channels: num_channels,
                },
            ));
        }
        let samples = len / channels.to_usize();
        let num_samples = u32::try_from(samples).map_err(|_| {
            // Any count beyond u32 also exceeds the FLTP payload ceiling.
            // Keep that size refusal distinct from a non-multiple length.
            crate::LayoutDefect::AudioBytes(
                (len as u64).saturating_mul(size_of::<f32>() as u64).into(),
            )
        })?;
        // Keep the channel proof from the divisibility check. Only the new
        // rate and sample count need checking before the shared extent rule.
        Ok(Self(ValidatedAudioLayout::from_quantities(
            crate::AudioFormat::Fltp,
            crate::frames::validate_audio_sample_rate(sample_rate.into())?,
            channels,
            crate::frames::validate_audio_count(
                num_samples.into(),
                crate::AudioCountKind::Samples,
            )?,
            None,
        )?))
    }

    /// The shape of a planar source whose layout was validated at construction
    /// or capture: a sample rate in
    /// `1..=i32::MAX`, positive channel and sample counts whose packed size is
    /// within the shared payload limit, and exactly `channels * samples`
    /// samples. Nothing is checked again.
    fn from_planar(frame: &(impl AudioSource + ?Sized)) -> Self {
        Self(*frame.layout().packed().as_layout())
    }

    fn check_output(self, length: usize) -> Result<()> {
        if length < self.0.sample_count() {
            return Err(Error::InvalidFrame(
                crate::FrameDefect::AudioDestinationLength {
                    actual: length,
                    minimum: self.0.sample_count(),
                },
            ));
        }
        Ok(())
    }

    /// Require a destination frame of exactly this shape, so that the native
    /// conversion writes every sample of it and nothing beyond.
    fn check_planar(self, frame: &AudioFrame) -> Result<()> {
        if frame.sample_rate() != self.0.sample_rate()
            || frame.num_channels() != self.0.num_channels()
            || frame.num_samples() != self.0.num_samples()
        {
            return Err(Error::InvalidFrame(crate::FrameDefect::PlanarAudioMismatch));
        }
        Ok(())
    }

    fn allocate(self, timecode: Timecode) -> AudioFrame {
        AudioFrame::from_validated_silence(self.0, timecode)
    }

    /// The SDK's `sample_rate`, `no_channels` and `no_samples`.
    fn to_sdk(self) -> (i32, i32, i32) {
        (
            self.0.sdk_sample_rate(),
            self.0.sdk_num_channels(),
            self.0.sdk_num_samples(),
        )
    }
}

macro_rules! interleaved_input {
    ($name:ident, $sample:ty, $raw:ident $(, $reference:ident)?) => {
        /// Borrowed, validated interleaved audio. Construction does not load NDI.
        ///
        /// Samples are ordered sample-first, then channel. The number of
        /// samples per channel is the slice's length divided by the channel
        /// count, which must divide it exactly; the equivalent FLTP
        /// allocation must fit the library's 64 MiB audio limit. Metadata is
        /// not represented.
        #[derive(Clone, Copy)]
        pub struct $name<'a> {
            shape: AudioShape,
            data: &'a [$sample],
            timecode: Timecode,
            $($reference: AudioReferenceLevel,)?
        }

        impl std::fmt::Debug for $name<'_> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let mut summary = f.debug_struct(stringify!($name));
                summary.field("sample_rate", &self.sample_rate())
                    .field("num_channels", &self.num_channels())
                    .field("num_samples", &self.num_samples())
                    .field("timecode", &self.timecode());
                $(summary.field(stringify!($reference), &self.$reference);)?
                summary.field("data (samples)", &self.data.len()).finish()
            }
        }

        impl<'a> $name<'a> {
            /// Validate the header and the complete initialized source slice.
            ///
            /// # Errors
            ///
            /// [`Error::InvalidFrame`] for a sample rate outside
            /// `1..=i32::MAX`, zero channels or samples, a `data` length
            /// that is not a multiple of `num_channels`, or audio larger
            /// than the 64 MiB limit.
            pub fn new(sample_rate: u32, num_channels: u32, data: &'a [$sample] $(, $reference: AudioReferenceLevel)?) -> Result<Self> {
                let shape = AudioShape::interleaved(sample_rate, num_channels, data.len())?;
                Ok(Self { shape, data, timecode: Timecode::SYNTHESIZE, $($reference,)? })
            }

            /// Return this audio with `timecode` instead of
            /// [`Timecode::SYNTHESIZE`].
            #[must_use]
            pub fn with_timecode(mut self, timecode: Timecode) -> Self { self.timecode = timecode; self }
            /// Sample rate in Hz.
            pub fn sample_rate(&self) -> u32 { self.shape.0.sample_rate() }
            /// Number of interleaved channels.
            pub fn num_channels(&self) -> u32 { self.shape.0.num_channels() }
            /// Number of samples per channel.
            pub fn num_samples(&self) -> u32 { self.shape.0.num_samples() }
            /// The timecode.
            pub fn timecode(&self) -> Timecode { self.timecode }
            /// Complete initialized interleaved samples.
            pub fn data(&self) -> &'a [$sample] { self.data }

            fn raw(&self) -> $raw {
                let (sample_rate, no_channels, no_samples) = self.shape.to_sdk();
                $raw {
                    sample_rate,
                    no_channels,
                    no_samples,
                    timecode: self.timecode.ticks(),
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

impl InterleavedAudioF32<'_> {
    pub(crate) fn layout(&self) -> ValidatedAudioLayout {
        self.shape.0
    }
}

/// Native audio conversion operations, resolved together from one live runtime.
///
/// Construction fails if any current v3 conversion helper is missing. Methods
/// take caller-owned output storage for reuse. For a runtime-free floating-point
/// conversion, use [`AudioFrame::from_interleaved`].
pub struct AudioConverter {
    audio: crate::runtime::Bound<sys::InterleavedAudio>,
}

macro_rules! conversions {
    ($input:ident, $sample:ty, $raw:ident, $to:ident, $from:ident, $(#[$to_attr:meta])* $to_method:ident, $(#[$from_attr:meta])* $from_method:ident, $(#[$into_attr:meta])* $into_method:ident => $convert_method:ident $(, $reference:ident)?) => {
        /// Convert FLTP samples into the active prefix of reusable interleaved storage.
        /// Returns the number of elements written; excess destination elements are
        /// untouched. Integer output can clip depending on the explicit reference
        /// level. On native failure the active prefix may be partially modified.
        /// Accepts every [`AudioSource`], preserving the original channel stride
        /// without allocating or first copying captured samples into owned FLTP.
        $(#[$to_attr])*
        pub fn $to_method(&self, source: &(impl AudioSource + ?Sized), destination: &mut [$sample] $(, $reference: AudioReferenceLevel)?) -> Result<usize> {
            let shape = AudioShape::from_planar(source);
            shape.check_output(destination.len())?;
            let source = source.audio_descriptor();
            let (sample_rate, no_channels, no_samples) = shape.to_sdk();
            let mut destination = $raw {
                sample_rate, no_channels, no_samples,
                timecode: source.timecode, p_data: destination.as_mut_ptr(),
                $($reference: $reference.db(),)?
            };
            // SAFETY: Both typed buffers cover the validated dimensions, do not
            // alias, and remain live throughout this synchronous conversion.
            unsafe { self.audio.named().$to(&source, &mut destination) }.performed()?;
            Ok(shape.0.sample_count())
        }

        /// Allocate a tightly packed FLTP frame with the source timecode.
        /// Interleaved headers have no metadata or timestamp; those output fields
        /// use the ordinary AudioFrame defaults.
        $(#[$from_attr])*
        pub fn $from_method(&self, source: &$input<'_>) -> Result<AudioFrame> {
            let mut destination = source.shape.allocate(source.timecode);
            self.$convert_method(source, &mut destination)?;
            Ok(destination)
        }

        /// Replace samples in an existing FLTP frame with matching dimensions and
        /// sample rate. Its timecode, metadata and timestamp remain unchanged.
        /// On native failure sample storage may be partially modified.
        $(#[$into_attr])*
        pub fn $into_method(&self, source: &$input<'_>, destination: &mut AudioFrame) -> Result<()> {
            source.shape.check_planar(destination)?;
            self.$convert_method(source, destination)
        }

        // The destination either comes from this source's validated shape or
        // has passed check_planar in the public reuse method.
        fn $convert_method(&self, source: &$input<'_>, destination: &mut AudioFrame) -> Result<()> {
            let source = source.raw();
            let mut raw = destination.audio_descriptor();
            raw.p_data = destination.data_mut().as_mut_ptr().cast();
            // SAFETY: Source and destination have matching checked dimensions;
            // the exclusively borrowed output is initialized and correctly aligned.
            unsafe { self.audio.named().$from(&source, &mut raw) }.performed()?;
            Ok(())
        }
    };
}

impl std::fmt::Debug for AudioConverter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioConverter")
            .field("runtime", self.audio.ndi().info())
            .finish_non_exhaustive()
    }
}

impl AudioConverter {
    /// Resolve the complete current conversion group before exposing operations.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedCapability`] when the runtime does not
    /// provide the complete native audio-conversion group.
    pub fn new(ndi: &Ndi) -> Result<Self> {
        Ok(Self { audio: ndi.bind()? })
    }

    conversions!(
        InterleavedAudioI16,
        i16,
        NDIlib_audio_frame_interleaved_16s_t,
        NDIlib_util_audio_to_interleaved_16s_v3,
        NDIlib_util_audio_from_interleaved_16s_v3,
        #[doc(alias = "NDIlib_util_audio_to_interleaved_16s_v3")]
        to_interleaved_i16,
        #[doc(alias = "NDIlib_util_audio_from_interleaved_16s_v3")]
        from_interleaved_i16,
        #[doc(alias = "NDIlib_util_audio_from_interleaved_16s_v3")]
        from_interleaved_i16_into => convert_from_interleaved_i16,
        reference_level
    );
    conversions!(
        InterleavedAudioI32,
        i32,
        NDIlib_audio_frame_interleaved_32s_t,
        NDIlib_util_audio_to_interleaved_32s_v3,
        NDIlib_util_audio_from_interleaved_32s_v3,
        #[doc(alias = "NDIlib_util_audio_to_interleaved_32s_v3")]
        to_interleaved_i32,
        #[doc(alias = "NDIlib_util_audio_from_interleaved_32s_v3")]
        from_interleaved_i32,
        #[doc(alias = "NDIlib_util_audio_from_interleaved_32s_v3")]
        from_interleaved_i32_into => convert_from_interleaved_i32,
        reference_level
    );
    conversions!(
        InterleavedAudioF32,
        f32,
        NDIlib_audio_frame_interleaved_32f_t,
        NDIlib_util_audio_to_interleaved_32f_v3,
        NDIlib_util_audio_from_interleaved_32f_v3,
        #[doc(alias = "NDIlib_util_audio_to_interleaved_32f_v3")]
        to_interleaved_f32,
        #[doc(alias = "NDIlib_util_audio_from_interleaved_32f_v3")]
        from_interleaved_f32,
        #[doc(alias = "NDIlib_util_audio_from_interleaved_32f_v3")]
        from_interleaved_f32_into => convert_from_interleaved_f32
    );
}

macro_rules! send_interleaved {
    ($(#[$attr:meta])* $method:ident, $input:ident, $send:ident) => {
        /// Synchronously send validated interleaved audio using the selected SDK.
        /// Source samples may be reused when this method returns. The interleaved
        /// SDK descriptors do not carry metadata or receive timestamps.
        ///
        /// # Errors
        ///
        /// Returns [`Error::UnsupportedCapability`] when the runtime does not
        /// provide the complete interleaved-audio send group.
        $(#[$attr])*
        pub fn $method(&self, frame: &$input<'_>) -> Result<()> {
            let ndi = self.runtime();
            let send = ndi.table::<sys::InterleavedSend>()?;
            let raw = frame.raw();
            // SAFETY: the sender instance is live for this borrow and the
            // validated source slice outlives the synchronous call.
            unsafe { send.$send(self.handle().instance(), &raw) };
            Ok(())
        }
    };
}

/// Interleaved audio goes through the SDK's audio send, so every mode that
/// sends uncompressed audio has it.
impl<M: AudioSendMode> Sender<M> {
    send_interleaved!(
        #[doc(alias = "NDIlib_util_send_send_audio_interleaved_16s")]
        send_audio_interleaved_i16,
        InterleavedAudioI16,
        NDIlib_util_send_send_audio_interleaved_16s
    );
    send_interleaved!(
        #[doc(alias = "NDIlib_util_send_send_audio_interleaved_32s")]
        send_audio_interleaved_i32,
        InterleavedAudioI32,
        NDIlib_util_send_send_audio_interleaved_32s
    );
    send_interleaved!(
        #[doc(alias = "NDIlib_util_send_send_audio_interleaved_32f")]
        send_audio_interleaved_f32,
        InterleavedAudioF32,
        NDIlib_util_send_send_audio_interleaved_32f
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaved_debug_summarizes_every_sample_type() {
        fn summaries(samples: usize) -> Vec<String> {
            let i16s = vec![1i16; 2 * samples];
            let i32s = vec![2i32; 2 * samples];
            let floats = vec![0.25; 2 * samples];
            vec![
                format!(
                    "{:?}",
                    InterleavedAudioI16::new(48_000, 2, &i16s, AudioReferenceLevel::SEND).unwrap()
                ),
                format!(
                    "{:?}",
                    InterleavedAudioI32::new(
                        48_000,
                        2,
                        &i32s,
                        AudioReferenceLevel::RECEIVE_HEADROOM
                    )
                    .unwrap()
                ),
                format!(
                    "{:?}",
                    InterleavedAudioF32::new(48_000, 2, &floats).unwrap()
                ),
            ]
        }
        let small = summaries(3);
        assert!(small[0].contains("reference_level: AudioReferenceLevel(0)"));
        assert!(small[1].contains("reference_level: AudioReferenceLevel(20)"));
        crate::test_support::assert_bounded_debug(&small, &summaries(48_000));
    }

    #[test]
    fn captured_audio_sources_lend_the_original_strided_descriptor() {
        use crate::capture::Guard;
        use crate::{AudioFrameRef, FrameSyncAudioRef};
        use std::{
            mem::MaybeUninit,
            sync::atomic::{AtomicUsize, Ordering},
        };
        let mut samples = [MaybeUninit::<f32>::uninit(); 11];
        for (start, values) in [(0, [0.125, 0.25, 0.5]), (8, [-0.125, -0.25, -0.5])] {
            for (index, value) in values.into_iter().enumerate() {
                samples[start + index].write(value);
            }
        }
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 44_100,
            no_channels: 2,
            no_samples: 3,
            timecode: 1234,
            FourCC: crate::FourCc::from(crate::AudioFormat::Fltp).raw(),
            p_data: samples.as_mut_ptr().cast(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 32,
            },
            p_metadata: c"<audio/>".as_ptr(),
            timestamp: 4567,
        };
        let frees = AtomicUsize::new(0);
        let instance = (&frees as *const AtomicUsize).cast_mut().cast();
        // SAFETY: the initialized channels, uninitialized inter-channel padding,
        // and static metadata stay live until both non-owning guards are dropped.
        let receiver = unsafe {
            AudioFrameRef::new(Guard::with_test_free(instance, raw, |instance, _| {
                (*(instance as *const AtomicUsize)).fetch_add(1, Ordering::SeqCst);
            }))
        }
        .unwrap();
        let framesync = unsafe {
            FrameSyncAudioRef::new(Guard::with_test_free(
                instance.cast(),
                raw,
                |instance, _| {
                    (*(instance as *const AtomicUsize)).fetch_add(1, Ordering::SeqCst);
                },
            ))
        }
        .unwrap();
        fn verify(source: &(impl AudioSource + ?Sized), raw: &NDIlib_audio_frame_v3_t) {
            let ((descriptor, shape), allocations) = crate::test_alloc::allocation_count(|| {
                (source.audio_descriptor(), AudioShape::from_planar(source))
            });
            assert_eq!(allocations, 0);
            assert_eq!(descriptor.p_data, raw.p_data);
            assert_eq!(descriptor.p_metadata, raw.p_metadata);
            assert_eq!(descriptor.timecode, raw.timecode);
            assert_eq!(descriptor.timestamp, raw.timestamp);
            assert_eq!(
                unsafe { descriptor.__bindgen_anon_1.channel_stride_in_bytes },
                32
            );
            assert_eq!(
                shape.0.sample_count(),
                6,
                "padding is not an interleaved destination sample"
            );
            assert!(shape.check_output(5).is_err());
            assert!(shape.check_output(6).is_ok());
            let owned = source.owned_copy();
            assert_eq!(owned.data(), &[0.125, 0.25, 0.5, -0.125, -0.25, -0.5]);
            assert_eq!(owned.metadata(), Some("<audio/>"));
            assert_eq!(owned.timecode().ticks(), 1234);
            assert_eq!(owned.timestamp().unwrap().ticks(), 4567);
        }
        assert!(receiver.data().is_none());
        assert!(framesync.data().is_none());
        verify(&receiver, &raw);
        verify(&framesync, &raw);
        assert_eq!(frees.load(Ordering::SeqCst), 0);
        drop(receiver);
        drop(framesync);
        assert_eq!(frees.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn native_strided_captures_convert_all_sample_types_without_planar_copies() {
        use crate::capture::Guard;
        use crate::{AudioFrameRef, FrameSyncAudioRef};
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let converter = AudioConverter::new(&ndi).unwrap();
        let samples: [f32; 11] = [
            0.25, -0.125, 0.5, 91.0, 92.0, 93.0, 94.0, 95.0, -0.25, 0.125, -0.5,
        ];
        let raw = NDIlib_audio_frame_v3_t {
            sample_rate: 44_100,
            no_channels: 2,
            no_samples: 3,
            timecode: 1234,
            FourCC: crate::FourCc::from(crate::AudioFormat::Fltp).raw(),
            p_data: samples.as_ptr().cast_mut().cast(),
            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                channel_stride_in_bytes: 32,
            },
            p_metadata: c"<audio/>".as_ptr(),
            timestamp: 4567,
        };
        // SAFETY: both synthetic guards borrow the initialized storage and static
        // metadata for this test; neither callback frees independently owned data.
        let receiver = unsafe {
            AudioFrameRef::new(Guard::with_test_free(std::ptr::null_mut(), raw, |_, _| {}))
        }
        .unwrap();
        let framesync = unsafe {
            FrameSyncAudioRef::new(Guard::with_test_free(std::ptr::null_mut(), raw, |_, _| {}))
        }
        .unwrap();
        fn verify(converter: &AudioConverter, source: &(impl AudioSource + ?Sized)) {
            let owned = source.owned_copy();
            let mut floats = [99.0; 8];
            let mut expected = [99.0; 8];
            converter.to_interleaved_f32(&owned, &mut expected).unwrap();
            // Warm native lazy state before counting wrapper allocations.
            converter.to_interleaved_f32(source, &mut floats).unwrap();
            let (count, allocations) = crate::test_alloc::allocation_count(|| {
                converter.to_interleaved_f32(source, &mut floats).unwrap()
            });
            assert_eq!((count, allocations), (6, 0));
            assert_eq!(floats, expected);
            assert_eq!(&floats[..6], &[0.25, -0.25, -0.125, 0.125, 0.5, -0.5]);
            for level in [
                AudioReferenceLevel::SEND,
                AudioReferenceLevel::RECEIVE_HEADROOM,
            ] {
                let mut actual = [99i16; 8];
                let mut expected = [99i16; 8];
                converter
                    .to_interleaved_i16(&owned, &mut expected, level)
                    .unwrap();
                converter
                    .to_interleaved_i16(source, &mut actual, level)
                    .unwrap();
                let (count, allocations) = crate::test_alloc::allocation_count(|| {
                    converter
                        .to_interleaved_i16(source, &mut actual, level)
                        .unwrap()
                });
                assert_eq!((count, allocations), (6, 0));
                assert_eq!(actual, expected);
                let mut actual = [99i32; 8];
                let mut expected = [99i32; 8];
                converter
                    .to_interleaved_i32(&owned, &mut expected, level)
                    .unwrap();
                converter
                    .to_interleaved_i32(source, &mut actual, level)
                    .unwrap();
                let (count, allocations) = crate::test_alloc::allocation_count(|| {
                    converter
                        .to_interleaved_i32(source, &mut actual, level)
                        .unwrap()
                });
                assert_eq!((count, allocations), (6, 0));
                assert_eq!(actual, expected);
            }
            let mut too_short = [77.0; 5];
            assert!(converter
                .to_interleaved_f32(source, &mut too_short)
                .is_err());
            assert_eq!(too_short, [77.0; 5]);
        }
        verify(&converter, &receiver);
        verify(&converter, &framesync);
        let (borrowed, allocations) = crate::test_alloc::allocation_count(|| {
            crate::BorrowedAudioFrame::builder(&samples, 44_100, 2, 3)
                .channel_stride_bytes(32)
                .metadata(c"<audio/>")
                .timecode(Timecode::from_ticks(1234))
                .build()
                .unwrap()
        });
        assert_eq!(allocations, 0);
        assert_eq!(borrowed.audio_descriptor().p_data, raw.p_data);
        assert_eq!(borrowed.audio_descriptor().p_metadata, raw.p_metadata);
        verify(&converter, &borrowed);
        verify(&converter, &borrowed as &dyn AudioSource);
    }

    #[test]
    fn interleaved_validation_rejects_bad_dimensions_lengths_and_limits() {
        let samples = vec![0.0f32; 16_777_217];
        use crate::{AudioCountKind, FrameDefect, LayoutDefect};
        let i16_samples = vec![0i16; samples.len()];
        let i32_samples = vec![0i32; samples.len()];
        let layout = FrameDefect::Layout;
        let length = |actual, channels| FrameDefect::InterleavedAudioLength { actual, channels };
        let count = |kind| {
            layout(LayoutDefect::AudioCount {
                kind,
                value: 0.into(),
            })
        };
        fn defect<T: std::fmt::Debug>(result: Result<T>) -> FrameDefect {
            match result {
                Err(Error::InvalidFrame(defect)) => defect,
                other => panic!("expected an invalid frame, got {other:?}"),
            }
        }
        for (rate, channels, len, expected) in [
            (0, 2, 2, layout(LayoutDefect::AudioRate(0.into()))),
            (
                1 << 31,
                2,
                2,
                layout(LayoutDefect::AudioRate((1u32 << 31).into())),
            ),
            (48000, 0, 2, count(AudioCountKind::Channels)),
            (48000, 2, 0, count(AudioCountKind::Samples)),
            (48000, 2, 3, length(3, 2)),
            (48000, 3, 2, length(2, 3)),
            (
                48000,
                u32::MAX,
                2,
                layout(LayoutDefect::AudioCount {
                    value: u32::MAX.into(),
                    kind: AudioCountKind::Channels,
                }),
            ),
            (
                48000,
                1,
                16_777_217,
                layout(LayoutDefect::AudioBytes(67_108_868u64.into())),
            ),
        ] {
            assert_eq!(
                defect(InterleavedAudioF32::new(rate, channels, &samples[..len])),
                expected
            );
            assert_eq!(
                defect(InterleavedAudioI16::new(
                    rate,
                    channels,
                    &i16_samples[..len],
                    AudioReferenceLevel::SEND,
                )),
                expected
            );
            assert_eq!(
                defect(InterleavedAudioI32::new(
                    rate,
                    channels,
                    &i32_samples[..len],
                    AudioReferenceLevel::SEND,
                )),
                expected
            );
        }
        // These counts can be checked without constructing an enormous slice.
        #[cfg(target_pointer_width = "64")]
        assert_eq!(
            defect(AudioShape::interleaved(48_000, 1, u32::MAX as usize + 1)),
            layout(LayoutDefect::AudioBytes(((u32::MAX as u64 + 1) * 4).into()))
        );
        let samples = [0.0f32; 6];
        let frame = InterleavedAudioF32::new(48000, 2, &samples).unwrap();
        assert_eq!(frame.data().as_ptr(), samples.as_ptr());
        assert_eq!(frame.sample_rate(), 48000);
        assert_eq!(frame.num_channels(), 2);
        assert_eq!(frame.num_samples(), 3);
        assert_eq!(frame.timecode(), Timecode::SYNTHESIZE);
        let frame = InterleavedAudioF32::new(48000, 3, &samples).unwrap();
        assert_eq!(frame.num_samples(), 2);
        assert_eq!(
            frame.with_timecode(Timecode::from_ticks(7)).raw().timecode,
            7
        );
        assert_eq!(AudioReferenceLevel::SEND.db(), 0);
        assert_eq!(AudioReferenceLevel::RECEIVE_HEADROOM.db(), 20);
    }

    #[test]
    fn native_conversion_refusals_name_the_entry_point_for_each_sample_type() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        macro_rules! stubs {
            ($to:ident, $from:ident, $raw:ty) => {
                unsafe extern "C" fn $to(_: *const NDIlib_audio_frame_v3_t, _: *mut $raw) -> bool {
                    false
                }
                unsafe extern "C" fn $from(
                    _: *const $raw,
                    _: *mut NDIlib_audio_frame_v3_t,
                ) -> bool {
                    false
                }
            };
        }
        stubs!(to_i16, from_i16, NDIlib_audio_frame_interleaved_16s_t);
        stubs!(to_i32, from_i32, NDIlib_audio_frame_interleaved_32s_t);
        stubs!(to_f32, from_f32, NDIlib_audio_frame_interleaved_32f_t);
        static TABLE: sys::InterleavedAudio = sys::InterleavedAudio {
            NDIlib_util_audio_to_interleaved_16s_v3: to_i16,
            NDIlib_util_audio_from_interleaved_16s_v3: from_i16,
            NDIlib_util_audio_to_interleaved_32s_v3: to_i32,
            NDIlib_util_audio_from_interleaved_32s_v3: from_i32,
            NDIlib_util_audio_to_interleaved_32f_v3: to_f32,
            NDIlib_util_audio_from_interleaved_32f_v3: from_f32,
        };
        let converter = AudioConverter {
            audio: crate::runtime::Bound::for_test(ndi, &TABLE),
        };
        let mut frame = AudioFrame::builder()
            .sample_rate(48_000)
            .num_channels(1)
            .num_samples(1)
            .build()
            .unwrap();
        let i16_input =
            InterleavedAudioI16::new(48_000, 1, &[0], AudioReferenceLevel::SEND).unwrap();
        let i32_input =
            InterleavedAudioI32::new(48_000, 1, &[0], AudioReferenceLevel::SEND).unwrap();
        let f32_input = InterleavedAudioF32::new(48_000, 1, &[0.0]).unwrap();
        let cases = [
            (
                converter
                    .to_interleaved_i16(&frame, &mut [0], AudioReferenceLevel::SEND)
                    .map(|_| ()),
                c"NDIlib_util_audio_to_interleaved_16s_v3",
            ),
            (
                converter
                    .to_interleaved_i32(&frame, &mut [0], AudioReferenceLevel::SEND)
                    .map(|_| ()),
                c"NDIlib_util_audio_to_interleaved_32s_v3",
            ),
            (
                converter.to_interleaved_f32(&frame, &mut [0.0]).map(|_| ()),
                c"NDIlib_util_audio_to_interleaved_32f_v3",
            ),
            (
                converter.from_interleaved_i16(&i16_input).map(|_| ()),
                c"NDIlib_util_audio_from_interleaved_16s_v3",
            ),
            (
                converter.from_interleaved_i32(&i32_input).map(|_| ()),
                c"NDIlib_util_audio_from_interleaved_32s_v3",
            ),
            (
                converter.from_interleaved_f32(&f32_input).map(|_| ()),
                c"NDIlib_util_audio_from_interleaved_32f_v3",
            ),
            (
                converter.from_interleaved_i16_into(&i16_input, &mut frame),
                c"NDIlib_util_audio_from_interleaved_16s_v3",
            ),
            (
                converter.from_interleaved_i32_into(&i32_input, &mut frame),
                c"NDIlib_util_audio_from_interleaved_32s_v3",
            ),
            (
                converter.from_interleaved_f32_into(&f32_input, &mut frame),
                c"NDIlib_util_audio_from_interleaved_32f_v3",
            ),
        ];
        for (result, expected) in cases {
            assert!(
                matches!(result, Err(Error::NativeOperationFailed { operation }) if operation == expected)
            );
        }
    }

    #[test]
    fn native_float_conversion_preserves_channel_order_and_reuses_storage() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let converter = AudioConverter::new(&ndi).unwrap();
        let samples = [0.125, -0.5, 0.25, -0.25, 0.5, -0.125];
        let input = InterleavedAudioF32::new(48000, 2, &samples)
            .unwrap()
            .with_timecode(Timecode::from_ticks(1234));
        // Warm the native helper before counting the wrapper's destination.
        drop(converter.from_interleaved_f32(&input).unwrap());
        let (frame, allocations) =
            crate::test_alloc::allocation_count(|| converter.from_interleaved_f32(&input).unwrap());
        assert_eq!(allocations, 1);
        let rust_frame = AudioFrame::from_interleaved(&input);
        assert_eq!(rust_frame.data(), frame.data());
        assert_eq!(rust_frame.timecode(), frame.timecode());
        assert_eq!(frame.data(), &[0.125, 0.25, 0.5, -0.5, -0.25, -0.125]);
        assert_eq!(frame.timecode(), Timecode::from_ticks(1234));
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
            .num_channels(2)
            .num_samples(3)
            .timecode(Timecode::from_ticks(5678))
            .metadata("<keep/>")
            .build()
            .unwrap();
        let pointer = destination.data().as_ptr();
        converter
            .from_interleaved_f32_into(&input, &mut destination)
            .unwrap();
        assert_eq!(destination.data().as_ptr(), pointer);
        assert_eq!(destination.data(), frame.data());
        assert_eq!(destination.timecode(), Timecode::from_ticks(5678));
        assert_eq!(destination.timestamp(), None);
        assert_eq!(destination.metadata(), Some("<keep/>"));
        let mut wrong_dimensions = AudioFrame::builder()
            .num_channels(1)
            .num_samples(6)
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
            InterleavedAudioI16::new(48000, 2, &i16_samples, AudioReferenceLevel::SEND).unwrap();
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
            InterleavedAudioI32::new(48000, 2, &i32_samples, AudioReferenceLevel::SEND).unwrap();
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
        let options = crate::SenderOptions::new(name)
            .clock_audio(false)
            .clock_video(false);
        let sender = Sender::new(&ndi, &options).unwrap();
        let i16_input =
            InterleavedAudioI16::new(48000, 2, &[1, -1, 2, -2], AudioReferenceLevel::SEND).unwrap();
        let i32_input =
            InterleavedAudioI32::new(48000, 2, &[1, -1, 2, -2], AudioReferenceLevel::SEND).unwrap();
        let f32_input = InterleavedAudioF32::new(48000, 2, &[0.1, -0.1, 0.2, -0.2]).unwrap();
        sender.send_audio_interleaved_i16(&i16_input).unwrap();
        sender.send_audio_interleaved_i32(&i32_input).unwrap();
        sender.send_audio_interleaved_f32(&f32_input).unwrap();
    }
}
