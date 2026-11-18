mod tests {
    use super::super::*;
    use crate::{capture::Guard, test_alloc::allocation_count};

    fn video() -> CompressedFrame<VideoPacketFormat> {
        CompressedFrame::<VideoPacketFormat>::new(
            VideoPacketFormat::new(VideoCodec::H264, 16, 16).unwrap(),
            PacketEnvelope::encode(EnvelopeCodec::H264, 0, 0, false, &[1; 8], &[]).unwrap(),
        )
        .unwrap()
    }

    fn audio() -> CompressedFrame<AudioPacketFormat> {
        CompressedFrame::<AudioPacketFormat>::new(
            AudioPacketFormat::new(AudioCodec::Aac, 48_000, 2, 1024)
                .unwrap()
                .with_protected(true),
            testing::aac_packet(48_000, 2, &[1; 8]),
        )
        .unwrap()
    }

    #[test]
    fn moving_conversions_preserve_allocations_and_metadata() {
        let mut sent = video();
        sent.set_metadata("<video/>").unwrap();
        let pointer = sent.data().as_ptr();
        let metadata = sent.metadata().unwrap().as_ptr();
        let (received, allocations) =
            allocation_count(|| ReceivedPacket::<VideoPacketFormat>::from(sent));
        assert_eq!(allocations, 0);
        let (sent, allocations) =
            allocation_count(|| CompressedFrame::<VideoPacketFormat>::try_from(received).unwrap());
        assert_eq!(allocations, 0);
        assert_eq!(sent.data().as_ptr(), pointer);
        assert_eq!(sent.metadata().unwrap().as_ptr(), metadata);
        let (received, allocations) =
            allocation_count(|| ReceivedPacket::<VideoPacketFormat>::from(sent));
        assert_eq!(allocations, 0);
        assert_eq!(received.data().as_ptr(), pointer);

        let mut sent = audio();
        sent.set_metadata("<audio/>").unwrap();
        let pointer = sent.data().as_ptr();
        let metadata = sent.metadata().unwrap().as_ptr();
        let (received, allocations) =
            allocation_count(|| ReceivedPacket::<AudioPacketFormat>::from(sent));
        assert_eq!(allocations, 0);
        let (sent, allocations) =
            allocation_count(|| CompressedFrame::<AudioPacketFormat>::try_from(received).unwrap());
        assert_eq!(allocations, 0);
        assert!(sent.format().is_protected());
        assert_eq!(sent.metadata().unwrap().as_ptr(), metadata);
        let (received, allocations) =
            allocation_count(|| ReceivedPacket::<AudioPacketFormat>::from(sent));
        assert_eq!(allocations, 0);
        assert_eq!(received.data().as_ptr(), pointer);
    }

    #[test]
    fn received_packet_metadata_refusals_preserve_both_allocations() {
        fn returned(text: String, refusal: impl FnOnce(String) -> Rejected<String>) {
            let pointer = text.as_ptr();
            let capacity = text.capacity();
            let length = text.len();
            let text = refusal(text).into_inner();
            assert_eq!(
                (text.as_ptr(), text.capacity(), text.len()),
                (pointer, capacity, length)
            );
        }
        for text in [
            "bad\0metadata".to_owned(),
            "x".repeat(crate::frames::MAX_METADATA_BYTES),
        ] {
            let mut video = ReceivedPacket::<VideoPacketFormat>::from(video());
            video.set_metadata("<old/>").unwrap();
            let pointer = video.metadata().unwrap().as_ptr();
            returned(text.clone(), |text| video.set_metadata(text).unwrap_err());
            assert_eq!(video.metadata(), Some("<old/>"));
            assert_eq!(video.metadata().unwrap().as_ptr(), pointer);
            let mut audio = ReceivedPacket::<AudioPacketFormat>::from(audio());
            audio.set_metadata("<old/>").unwrap();
            let pointer = audio.metadata().unwrap().as_ptr();
            returned(text, |text| audio.set_metadata(text).unwrap_err());
            assert_eq!(audio.metadata(), Some("<old/>"));
            assert_eq!(audio.metadata().unwrap().as_ptr(), pointer);
        }
    }

    fn later_header(mut bytes: Vec<u8>) -> Vec<u8> {
        bytes.splice(ENVELOPE_HEADER_BYTES..ENVELOPE_HEADER_BYTES, [7; 4]);
        bytes[..4].copy_from_slice(&48i32.to_ne_bytes());
        bytes
    }

    #[test]
    fn later_video_headers_roundtrip_and_refuse_before_copying() {
        let source = video();
        let bytes = later_header(source.into_data());
        let mut raw = VideoPacketFormat::new(VideoCodec::H264, 32, 24)
            .unwrap()
            .raw();
        raw.p_data = bytes.as_ptr().cast_mut();
        raw.__bindgen_anon_1.data_size_in_bytes = PacketLen::new(bytes.len()).unwrap().to_sdk();
        raw.p_metadata = c"<received/>".as_ptr();
        // SAFETY: bytes and the terminated metadata remain live until the guard drops.
        let guard = unsafe { Guard::with_test_free(std::ptr::null_mut(), raw, |_, _| {}) };
        let borrowed = CompressedRef::<VideoPacketFormat>::new(guard).unwrap();
        let received = borrowed.to_owned();
        assert_eq!(received.data(), bytes);
        assert_eq!(
            borrowed.envelope(),
            Some(PacketEnvelope::parse(&bytes).unwrap())
        );
        assert_eq!(received.envelope(), borrowed.envelope());
        let pointer = received.data().as_ptr();
        let refused = CompressedFrame::<VideoPacketFormat>::try_from(received)
            .unwrap_err()
            .into_inner();
        assert_eq!(refused.data().as_ptr(), pointer);
        assert_eq!(refused.data(), bytes);
        assert_eq!(refused.metadata(), Some("<received/>"));
        let mut destination = video();
        destination.set_metadata("<original/>").unwrap();
        let original = destination.clone();
        let pointer = destination.data().as_ptr();
        assert!(borrowed.try_copy_into(&mut destination).is_err());
        assert!(CompressedFrame::<VideoPacketFormat>::try_from(&borrowed).is_err());
        assert_eq!(destination.data(), original.data());
        assert_eq!(destination.data().as_ptr(), pointer);
        assert_eq!(destination.format(), original.format());
        assert_eq!(destination.metadata(), original.metadata());
        assert_eq!(destination.timestamp(), original.timestamp());
    }

    #[test]
    fn later_audio_headers_roundtrip_and_refuse_before_copying() {
        let mut source = audio();
        let mut raw = source.raw();
        let bytes = later_header(source.into_data());
        raw.p_data = bytes.as_ptr().cast_mut();
        raw.__bindgen_anon_1.data_size_in_bytes = PacketLen::new(bytes.len()).unwrap().to_sdk();
        raw.no_channels = 6;
        raw.p_metadata = c"<received/>".as_ptr();
        // SAFETY: bytes and terminated metadata remain live until the guard drops.
        let guard = unsafe { Guard::with_test_free(std::ptr::null_mut(), raw, |_, _| {}) };
        let borrowed = CompressedRef::<AudioPacketFormat>::new(guard).unwrap();
        let received = borrowed.to_owned();
        assert_eq!(received.data(), bytes);
        assert_eq!(
            borrowed.envelope(),
            Some(PacketEnvelope::parse(&bytes).unwrap())
        );
        assert_eq!(received.envelope(), borrowed.envelope());
        let pointer = received.data().as_ptr();
        let refused = CompressedFrame::<AudioPacketFormat>::try_from(received)
            .unwrap_err()
            .into_inner();
        assert_eq!(refused.data().as_ptr(), pointer);
        assert_eq!(refused.data(), bytes);
        let mut destination = audio();
        destination.set_metadata("<original/>").unwrap();
        let original = destination.clone();
        let pointer = destination.data().as_ptr();
        assert!(borrowed.try_copy_into(&mut destination).is_err());
        assert!(CompressedFrame::<AudioPacketFormat>::try_from(&borrowed).is_err());
        assert_eq!(destination.data(), original.data());
        assert_eq!(destination.data().as_ptr(), pointer);
        assert_eq!(
            destination.format().num_channels(),
            original.format().num_channels()
        );
        assert_eq!(destination.metadata(), original.metadata());
        assert_eq!(destination.timestamp(), original.timestamp());
    }

    #[test]
    fn borrowed_send_copies_reuse_capacity_and_replace_all_observations() {
        let mut source = video();
        source.set_metadata("<new/>").unwrap();
        source.set_timecode(Timecode::from_ticks(123));
        let mut raw = source.raw();
        raw.timestamp = 456;
        // SAFETY: source storage remains live and unchanged through the borrow.
        let guard = unsafe { Guard::with_test_free(std::ptr::null_mut(), raw, |_, _| {}) };
        let borrowed = CompressedRef::<VideoPacketFormat>::new(guard).unwrap();
        let mut destination = video();
        destination.set_metadata("<long-original/>").unwrap();
        let pointer = destination.data().as_ptr();
        let (result, allocations) = allocation_count(|| borrowed.try_copy_into(&mut destination));
        result.unwrap();
        assert_eq!(allocations, 0);
        assert_eq!(destination.data().as_ptr(), pointer);
        assert_eq!(destination.format(), borrowed.format());
        assert_eq!(destination.timecode(), borrowed.timecode());
        assert_eq!(destination.timestamp(), borrowed.timestamp());
        assert_eq!(destination.metadata(), borrowed.metadata());
        assert_eq!(destination.metadata_status(), borrowed.metadata_status());

        let mut source = CompressedFrame::<AudioPacketFormat>::new(
            AudioPacketFormat::new(AudioCodec::Opus, 24_000, 6, 480)
                .unwrap()
                .with_protected(true),
            vec![1; 8],
        )
        .unwrap();
        source.set_timecode(Timecode::from_ticks(789));
        let mut raw = source.raw();
        raw.timestamp = 987;
        // SAFETY: source storage remains live and unchanged through the borrow.
        let guard = unsafe { Guard::with_test_free(std::ptr::null_mut(), raw, |_, _| {}) };
        let borrowed = CompressedRef::<AudioPacketFormat>::new(guard).unwrap();
        let mut destination = audio();
        destination.set_metadata("<old/>").unwrap();
        let pointer = destination.data().as_ptr();
        let (result, allocations) = allocation_count(|| borrowed.try_copy_into(&mut destination));
        result.unwrap();
        assert_eq!(allocations, 0);
        assert_eq!(destination.data().as_ptr(), pointer);
        assert_eq!(destination.format(), borrowed.format());
        assert_eq!(destination.timecode(), borrowed.timecode());
        assert_eq!(destination.timestamp(), borrowed.timestamp());
        assert_eq!(destination.metadata(), borrowed.metadata());
        assert_eq!(destination.metadata_status(), borrowed.metadata_status());
    }
}
