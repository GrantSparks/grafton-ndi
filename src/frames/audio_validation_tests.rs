use super::*;
use crate::{AudioCountKind, FrameDefect, LayoutDefect};

fn defect<T>(result: Result<T>) -> Option<FrameDefect> {
    match result {
        Ok(_) => None,
        Err(Error::InvalidFrame(cause)) => Some(cause),
        Err(error) => panic!("unexpected error: {error:?}"),
    }
}

/// Pin both the cause and its priority before consolidating the shared rules.
/// These checks classify descriptors only: no pointer is dereferenced.
#[test]
fn returned_and_captured_audio_keep_exact_causes_and_validation_order() {
    let mut sample = 0.0f32;
    let count = |value: i32, kind| {
        FrameDefect::Layout(LayoutDefect::AudioCount {
            value: value.into(),
            kind,
        })
    };
    let rate = |value: i32| FrameDefect::Layout(LayoutDefect::AudioRate(value.into()));
    for sample_rate in [-1, 0, 1, i32::MAX] {
        for channels in [-1, 0, 1, i32::MAX] {
            for samples in [-1, 0, 1] {
                for has_data in [false, true] {
                    for supported in [false, true] {
                        let fourcc = if supported {
                            FourCc::from(AudioFormat::Fltp).raw()
                        } else {
                            0xDEAD_BEEF
                        };
                        let raw = NDIlib_audio_frame_v3_t {
                            sample_rate,
                            no_channels: channels,
                            no_samples: samples,
                            FourCC: fourcc,
                            p_data: if has_data {
                                (&mut sample as *mut f32).cast()
                            } else {
                                ptr::null_mut()
                            },
                            __bindgen_anon_1: NDIlib_audio_frame_v3_t__bindgen_ty_1 {
                                channel_stride_in_bytes: if samples == 0 { 0 } else { 4 },
                            },
                            ..Default::default()
                        };
                        let unknown = FrameDefect::UnknownAudioFormat { fourcc };
                        let expected = if samples == 0 {
                            Some(count(0, AudioCountKind::Samples))
                        } else if !has_data {
                            Some(FrameDefect::NullAudioData)
                        } else if !supported {
                            Some(unknown.clone())
                        } else if sample_rate <= 0 {
                            Some(rate(sample_rate))
                        } else if channels <= 0 {
                            Some(count(channels, AudioCountKind::Channels))
                        } else if samples < 0 {
                            Some(count(samples, AudioCountKind::Samples))
                        } else if channels == i32::MAX {
                            Some(FrameDefect::Layout(LayoutDefect::AudioBytes(
                                (i32::MAX as u64 * 4).into(),
                            )))
                        } else {
                            None
                        };
                        assert_eq!(defect(validate_audio_layout(&raw)), expected, "rate={sample_rate}, channels={channels}, samples={samples}, data={has_data}, supported={supported}");
                        let returned = if samples != 0 {
                            expected
                        } else if has_data {
                            Some(FrameDefect::EmptyAudioHasData)
                        } else if sample_rate < 0 {
                            Some(rate(sample_rate))
                        } else if channels < 0 {
                            Some(count(channels, AudioCountKind::Channels))
                        } else if (sample_rate == 0) != (channels == 0) {
                            Some(FrameDefect::EmptyAudioFormat {
                                sample_rate,
                                channels,
                            })
                        } else if !supported {
                            Some(unknown)
                        } else {
                            None
                        };
                        assert_eq!(defect(validate_returned_audio(&raw)), returned, "rate={sample_rate}, channels={channels}, samples={samples}, data={has_data}, supported={supported}");
                    }
                }
            }
        }
    }
}

#[test]
fn audio_extent_requires_checked_counts_and_keeps_exact_byte_boundaries() {
    let extent: fn(SdkQuantity, SdkQuantity, u64) -> LayoutResult<usize> = checked_audio_extent;
    let quantity = |value: usize| SdkQuantity::new(value as i64).unwrap();
    let one = quantity(1);
    let limit = MAX_AUDIO_BYTES / size_of::<f32>();
    assert_eq!(extent(one, quantity(limit), u64::MAX), Ok(limit));
    assert_eq!(
        extent(one, quantity(limit + 1), 0),
        Err(LayoutDefect::AudioBytes(((limit as u64 + 1) * 4).into()))
    );
    // Extent arithmetic remains defined across the entire SDK count range.
    let channels = quantity(i32::MAX as usize);
    assert_eq!(extent(channels, one, 0), Ok(1));
    assert_eq!(
        extent(channels, one, 1),
        Err(LayoutDefect::AudioBytes((i32::MAX as u64 * 4).into()))
    );
    assert_eq!(
        extent(channels, one, u64::MAX),
        Err(LayoutDefect::AudioBytes(u64::MAX.into()))
    );
}
