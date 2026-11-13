//! Owned storage returns to the application on rejection and after frame use.
use grafton_ndi::{
    AudioFrame, AudioLayout, BorrowedVideoFrame, Error, PixelFormat, ScanType, VideoFrame,
};

#[test]
fn rejected_builders_keep_original_storage_and_can_be_repaired() {
    let mut pixels = Vec::with_capacity(4096);
    pixels.resize(16 * 16 * 4, 3);
    let (pointer, capacity) = (pixels.as_ptr(), pixels.capacity());
    let rejected = VideoFrame::builder()
        .resolution(0, 16)
        .data(pixels)
        .build()
        .unwrap_err();
    assert!(matches!(rejected.error(), Error::InvalidFrame(_)));
    let builder = rejected.into_inner();
    assert_eq!(builder.supplied_data().unwrap().as_ptr(), pointer);
    let frame = builder.resolution(16, 16).build().unwrap();
    let pixels = frame.into_data();
    assert_eq!((pixels.as_ptr(), pixels.capacity()), (pointer, capacity));
    assert_eq!(pixels, vec![3; 1024]);

    let mut samples = Vec::with_capacity(128);
    samples.extend_from_slice(&[1.0, 3.0, 2.0, 4.0]);
    let (pointer, capacity) = (samples.as_ptr(), samples.capacity());
    let rejected = AudioFrame::builder()
        .num_channels(2)
        .num_samples(2)
        .layout(AudioLayout::Interleaved)
        .data(samples)
        .metadata("invalid\0metadata")
        .build()
        .unwrap_err();
    assert!(matches!(rejected.error(), Error::InvalidCString(_)));
    let builder = rejected.into_inner();
    assert_eq!(builder.supplied_data(), Some(&[1.0, 3.0, 2.0, 4.0][..]));
    let samples = builder.into_data().unwrap();
    assert_eq!((samples.as_ptr(), samples.capacity()), (pointer, capacity));
    let frame = AudioFrame::builder()
        .num_channels(2)
        .num_samples(2)
        .layout(AudioLayout::Interleaved)
        .data(samples)
        .metadata("invalid\0metadata")
        .clear_metadata()
        .build()
        .unwrap();
    assert_eq!(frame.data(), [1.0, 2.0, 3.0, 4.0]);
}

#[test]
fn rejected_lengths_do_not_change_buffers_and_swapping_reuses_both_allocations() {
    let invalid = vec![9; 5];
    let pointer = invalid.as_ptr();
    let returned = VideoFrame::builder()
        .resolution(16, 16)
        .data(invalid)
        .build()
        .unwrap_err()
        .into_inner()
        .into_data()
        .unwrap();
    assert_eq!((returned.as_ptr(), returned.len()), (pointer, 5));

    let mut repairable = Vec::with_capacity(4096);
    repairable.resize(1023, 7);
    let pointer = repairable.as_ptr();
    let mut builder = VideoFrame::builder()
        .resolution(16, 16)
        .data(repairable)
        .build()
        .unwrap_err()
        .into_inner();
    let mut repairable = builder.take_data().unwrap();
    repairable.push(7);
    assert!(builder.supplied_data().is_none());
    let repaired = builder.data(repairable).build().unwrap();
    assert_eq!(repaired.data().as_ptr(), pointer);

    let mut first = Vec::with_capacity(8192);
    first.resize(1024, 1);
    let mut second = Vec::with_capacity(4096);
    second.resize(1024, 2);
    let (first_pointer, first_capacity) = (first.as_ptr(), first.capacity());
    let (second_pointer, second_capacity) = (second.as_ptr(), second.capacity());
    let mut frame = VideoFrame::builder()
        .resolution(16, 16)
        .data(first)
        .build()
        .unwrap();
    let first = frame.swap_data(second).unwrap();
    assert_eq!(
        (first.as_ptr(), first.capacity()),
        (first_pointer, first_capacity)
    );
    assert_eq!(frame.data().as_ptr(), second_pointer);
    frame.swap_data(vec![0; 3]).unwrap_err();
    assert_eq!(frame.data().as_ptr(), second_pointer);
    let second = frame.into_data();
    assert_eq!(
        (second.as_ptr(), second.capacity()),
        (second_pointer, second_capacity)
    );

    let mut original = Vec::with_capacity(256);
    original.resize(32, 1.0);
    let (pointer, capacity) = (original.as_ptr(), original.capacity());
    let mut audio = AudioFrame::builder()
        .num_channels(2)
        .num_samples(16)
        .data(original)
        .build()
        .unwrap();
    let original = audio.swap_data(vec![2.0; 32]).unwrap();
    assert_eq!(
        (original.as_ptr(), original.capacity()),
        (pointer, capacity)
    );
    let rejected = audio.swap_data(vec![7.0; 3]).unwrap_err();
    assert_eq!(rejected.into_inner(), [7.0; 3]);
    let samples = audio.into_data();
    assert_eq!(samples, [2.0; 32]);
}

#[test]
fn complete_send_size_includes_scan_rows_and_stride_and_enables_direct_sending() {
    for format in [
        PixelFormat::Uyvy,
        PixelFormat::Uyva,
        PixelFormat::Bgra,
        PixelFormat::I420,
    ] {
        for scan in [
            ScanType::Progressive,
            ScanType::Interlaced,
            ScanType::Field0,
            ScanType::Field1,
        ] {
            for height in [2, 4, 6] {
                let stride = format.line_stride_bytes(16).unwrap() + 16;
                let allocation = format
                    .send_buffer_len(16, height, Some(stride), scan)
                    .unwrap();
                let buffer = vec![0; allocation];
                let frame = BorrowedVideoFrame::builder(&buffer, 16, height, format)
                    .line_stride_bytes(stride)
                    .scan_type(scan)
                    .allow_send_from_padding_and_tail(true)
                    .build()
                    .unwrap();
                assert!(frame.sends_in_place(), "{format:?} {scan:?} {height}");
                let packed_size = format.buffer_len(16, height).unwrap();
                assert!(allocation >= packed_size + format.send_tail_room());
                if format.send_tail_room() > 0 && scan != ScanType::Progressive && height % 4 != 0 {
                    assert!(allocation >= packed_size + format.send_tail_room() + 2 * stride);
                }
            }
        }
    }
    assert!(PixelFormat::Bgra
        .send_buffer_len(0, 2, None, ScanType::Progressive)
        .is_err());
    assert!(PixelFormat::Bgra
        .send_buffer_len(2, 2, Some(usize::MAX), ScanType::Progressive)
        .is_err());
}

#[cfg(feature = "advanced")]
#[test]
fn compressed_and_scatter_round_trips_preserve_payload_pools() {
    use grafton_ndi::advanced::{
        AudioCodec, CompressedAudioFrame, CompressedVideoFrame, ScatterAudioFrame,
        ScatterVideoFrame, VideoCodec, VideoPacketFormat,
    };
    let format = VideoPacketFormat::new(VideoCodec::SpeedHq422, 1920, 1080).unwrap();
    let mut bytes = Vec::with_capacity(4096);
    bytes.extend_from_slice(&[0, 0, 0, 0, 1, 2, 3]);
    let (pointer, capacity) = (bytes.as_ptr(), bytes.capacity());
    let bytes = CompressedVideoFrame::new(format, bytes)
        .unwrap()
        .into_data();
    assert_eq!((bytes.as_ptr(), bytes.capacity()), (pointer, capacity));
    let mut tail = Vec::with_capacity(2048);
    tail.extend_from_slice(&[4, 5]);
    let (tail_pointer, tail_capacity) = (tail.as_ptr(), tail.capacity());
    let blocks = ScatterVideoFrame::new(format, vec![bytes, tail])
        .unwrap()
        .into_blocks();
    assert_eq!(
        (blocks[0].as_ptr(), blocks[0].capacity()),
        (pointer, capacity)
    );
    assert_eq!(
        (blocks[1].as_ptr(), blocks[1].capacity()),
        (tail_pointer, tail_capacity)
    );

    let mut bytes = Vec::with_capacity(1024);
    bytes.extend_from_slice(&[1, 2, 3]);
    let (pointer, capacity) = (bytes.as_ptr(), bytes.capacity());
    let bytes = CompressedAudioFrame::new(AudioCodec::Opus, 48_000, 2, 960, bytes)
        .unwrap()
        .into_data();
    assert_eq!((bytes.as_ptr(), bytes.capacity()), (pointer, capacity));
    let blocks = ScatterAudioFrame::new(AudioCodec::Opus, 48_000, 2, 960, vec![bytes])
        .unwrap()
        .into_blocks();
    assert_eq!(
        (blocks[0].as_ptr(), blocks[0].capacity()),
        (pointer, capacity)
    );
    let bytes = blocks.into_iter().next().unwrap();
    let rejected = CompressedAudioFrame::new(AudioCodec::Opus, 48_000, 0, 960, bytes)
        .unwrap_err()
        .into_inner();
    assert_eq!(
        (rejected.as_ptr(), rejected.capacity()),
        (pointer, capacity)
    );
    // AAC metadata/envelope validation occurs after its storage owner is created.
    let rejected = CompressedAudioFrame::new(AudioCodec::Aac, 48_000, 2, 1024, rejected)
        .unwrap_err()
        .into_inner();
    assert_eq!(
        (rejected.as_ptr(), rejected.capacity()),
        (pointer, capacity)
    );
}
