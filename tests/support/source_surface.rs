// Run each accessor through both concrete and type-erased public surfaces.
macro_rules! video_source_surface {
    ($frame:expr, $data:expr, $source:path, $owned:ty) => {{
        let frame = $frame;
        let source: &dyn $source = frame;
        assert_eq!(frame.width(), source.width());
        assert_eq!(frame.height(), source.height());
        assert_eq!(frame.pixel_format(), source.pixel_format());
        assert_eq!(frame.frame_rate(), source.frame_rate());
        assert_eq!(frame.picture_aspect_ratio(), source.picture_aspect_ratio());
        assert_eq!(frame.scan_type(), source.scan_type());
        assert_eq!(frame.timecode(), source.timecode());
        assert_eq!(frame.timestamp(), source.timestamp());
        assert_eq!(frame.line_stride_bytes(), source.line_stride_bytes());
        assert_eq!(frame.metadata(), source.metadata());
        assert_eq!(frame.metadata_status(), source.metadata_status());
        assert_eq!(frame.color_info().unwrap(), source.color_info().unwrap());
        assert_eq!($data, source.data());
        assert_eq!(frame.sends_in_place(), source.sends_in_place());
        for index in 0..frame.planes().len() + 1 {
            assert_eq!(
                frame
                    .plane(index)
                    .map(|plane| plane.rows().map(<[u8]>::to_vec).collect::<Vec<_>>()),
                source
                    .plane(index)
                    .map(|plane| plane.rows().map(<[u8]>::to_vec).collect::<Vec<_>>())
            );
        }
        assert_eq!(
            frame
                .planes()
                .map(|plane| plane.rows().map(<[u8]>::to_vec).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            source
                .planes()
                .map(|plane| plane.rows().map(<[u8]>::to_vec).collect::<Vec<_>>())
                .collect::<Vec<_>>()
        );
        let concrete_copy = frame.to_owned();
        let erased_copy = source.to_owned();
        assert_eq!(concrete_copy.data(), erased_copy.data());
        assert_eq!(concrete_copy.timestamp(), erased_copy.timestamp());
        assert_eq!(concrete_copy.metadata(), erased_copy.metadata());
        let mut concrete = <$owned>::builder().resolution(2, 2).build().unwrap();
        let mut erased = concrete.clone();
        frame.copy_into(&mut concrete);
        source.copy_into(&mut erased);
        assert_eq!(concrete.data(), erased.data());
        assert_eq!(concrete.frame_rate(), erased.frame_rate());
        assert_eq!(
            concrete.picture_aspect_ratio(),
            erased.picture_aspect_ratio()
        );
        assert_eq!(concrete.scan_type(), erased.scan_type());
        assert_eq!(concrete.timestamp(), erased.timestamp());
        assert_eq!(concrete.timecode(), erased.timecode());
        assert_eq!(concrete.metadata(), erased.metadata());
        assert_eq!(concrete.metadata_status(), erased.metadata_status());
    }};
}

macro_rules! audio_source_surface {
    ($frame:expr, $data:expr, $source:path, $owned:ty) => {{
        let frame = $frame;
        let source: &dyn $source = frame;
        assert_eq!(frame.sample_rate(), source.sample_rate());
        assert_eq!(frame.num_channels(), source.num_channels());
        assert_eq!(frame.num_samples(), source.num_samples());
        assert_eq!(frame.format(), source.format());
        assert_eq!(frame.timecode(), source.timecode());
        assert_eq!(frame.timestamp(), source.timestamp());
        assert_eq!(frame.channel_stride_bytes(), source.channel_stride_bytes());
        assert_eq!(frame.metadata(), source.metadata());
        assert_eq!(frame.metadata_status(), source.metadata_status());
        assert_eq!($data, source.data());
        for index in 0..=frame.num_channels() as usize {
            assert_eq!(frame.channel_data(index), source.channel_data(index));
        }
        assert_eq!(
            frame.channels().collect::<Vec<_>>(),
            source.channels().collect::<Vec<_>>()
        );
        let concrete_copy = frame.to_owned();
        let erased_copy = source.to_owned();
        assert_eq!(concrete_copy.data(), erased_copy.data());
        assert_eq!(concrete_copy.timestamp(), erased_copy.timestamp());
        assert_eq!(concrete_copy.metadata(), erased_copy.metadata());
        let mut concrete = <$owned>::builder()
            .num_channels(1)
            .num_samples(1)
            .build()
            .unwrap();
        let mut erased = concrete.clone();
        frame.copy_into(&mut concrete);
        source.copy_into(&mut erased);
        assert_eq!(concrete.data(), erased.data());
        assert_eq!(concrete.sample_rate(), erased.sample_rate());
        assert_eq!(concrete.timecode(), erased.timecode());
        assert_eq!(concrete.timestamp(), erased.timestamp());
        assert_eq!(concrete.metadata(), erased.metadata());
        assert_eq!(concrete.metadata_status(), erased.metadata_status());
    }};
}
