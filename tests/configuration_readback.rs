//! Configuration inspection is SDK-free and reports stored requests.
use grafton_ndi::{
    discovery::{AdvertiserOptions, ListenerOptions, RecvAdvertisement, SendAdvertisement},
    routing::RouterOptions,
    FinderOptions, FrameSyncAudioRequest, ReceiverBandwidth, ReceiverColorFormat, ReceiverOptions,
    RuntimeOptions, SdkEdition, SenderOptions, Source,
};

#[test]
fn receiver_options_report_presets_and_last_setter_wins() {
    let options = ReceiverOptions::snapshot_preset(Source::new("Venue (Audio)"))
        .bandwidth(ReceiverBandwidth::AudioOnly)
        .name("first")
        .name("venue-audio");
    assert_eq!(options.configured_source().name(), "Venue (Audio)");
    assert_eq!(options.configured_bandwidth(), ReceiverBandwidth::AudioOnly);
    assert_eq!(
        options.configured_color_format(),
        ReceiverColorFormat::RgbxRgba
    );
    assert!(!options.configured_allow_video_fields());
    assert_eq!(options.configured_name(), Some("venue-audio"));
    assert_eq!(
        ReceiverOptions::new(Source::new("default")).configured_name(),
        None
    );
}

#[test]
fn other_core_settings_are_stored_without_runtime_selection() {
    let finder = FinderOptions::new()
        .show_local_sources(false)
        .groups("Studio")
        .extra_ips("127.0.0.1");
    assert!(!finder.configured_show_local_sources());
    assert_eq!(finder.configured_groups(), Some("Studio"));
    assert_eq!(finder.configured_extra_ips(), Some("127.0.0.1"));

    let sender = SenderOptions::new("Program")
        .groups("Main")
        .clock_audio(false)
        .clock_video(false);
    assert_eq!(sender.configured_name(), "Program");
    assert_eq!(sender.configured_groups(), Some("Main"));
    assert!(!sender.configured_clock_audio());
    assert!(!sender.configured_clock_video());

    let router = RouterOptions::new("Route").groups("Control");
    assert_eq!(router.configured_name(), "Route");
    assert_eq!(router.configured_groups(), Some("Control"));

    let runtime = RuntimeOptions::new()
        .library_path("/tmp/libndi.so")
        .edition(SdkEdition::Standard);
    assert_eq!(
        runtime.configured_library_path(),
        Some(std::path::Path::new("/tmp/libndi.so"))
    );
    assert_eq!(runtime.configured_edition(), Some(SdkEdition::Standard));

    let request = FrameSyncAudioRequest::capture_at(48_000, 1, 480).unwrap();
    assert_eq!(
        (
            request.sample_rate(),
            request.num_channels(),
            request.num_samples()
        ),
        (48_000, 1, 480)
    );
    assert_eq!(request.num_samples(), 480);
}

#[test]
fn discovery_settings_preserve_unset_values() {
    assert_eq!(ListenerOptions::new().configured_server(), None);
    assert_eq!(
        ListenerOptions::new()
            .server("127.0.0.1")
            .configured_server(),
        Some("127.0.0.1")
    );
    assert_eq!(
        AdvertiserOptions::new()
            .servers("one,two")
            .configured_servers(),
        Some("one,two")
    );
    assert!(SendAdvertisement::new()
        .allow_monitoring(true)
        .configured_monitoring());
    let receiver = RecvAdvertisement::new()
        .allow_monitoring(true)
        .allow_controlling(true)
        .input_group("Studio");
    assert!(receiver.configured_monitoring());
    assert!(receiver.configured_controlling());
    assert_eq!(receiver.configured_input_group(), Some("Studio"));
}

#[cfg(feature = "file-io")]
#[test]
fn file_settings_keep_optional_rollover_distinct() {
    use grafton_ndi::file::{
        AsyncFileOptions, FileNaming, FileRuntimeOptions, PlayerOptions, RecorderOptions,
        SourceSpec,
    };
    use grafton_ndi::MediaTypes;
    use std::{path::Path, time::Duration};

    let runtime = FileRuntimeOptions::new().library_path("/tmp/file.so");
    assert_eq!(
        runtime.configured_library_path(),
        Some(Path::new("/tmp/file.so"))
    );
    let player = PlayerOptions::new("Replay").groups("Program");
    assert_eq!(player.configured_name(), "Replay");
    assert_eq!(player.configured_groups(), Some("Program"));
    let source = SourceSpec::new("Camera").unwrap();
    let base = RecorderOptions::new(source.clone()).naming(FileNaming::Exact);
    assert_eq!(base.configured_source(), &source);
    assert_eq!(base.configured_format_change_rollover(), None);
    assert_eq!(base.configured_naming(), &FileNaming::Exact);
    let explicit = base
        .format_change_rollover(false)
        .tracks(MediaTypes::AUDIO)
        .planar_audio(true)
        .proxy(false)
        .rollover_size(1024)
        .drop_wait(Duration::from_secs(2));
    assert_eq!(explicit.configured_format_change_rollover(), Some(false));
    assert_eq!(explicit.configured_tracks(), MediaTypes::AUDIO);
    assert!(explicit.configured_planar_audio());
    assert!(!explicit.configured_proxy());
    assert_eq!(explicit.configured_rollover_size(), 1024);
    assert_eq!(explicit.configured_drop_wait(), Duration::from_secs(2));
    let async_options = AsyncFileOptions::new()
        .command_capacity(7)
        .event_capacity(9);
    assert_eq!(
        (
            async_options.configured_command_capacity(),
            async_options.configured_event_capacity()
        ),
        (7, 9)
    );
}

#[cfg(feature = "image-encoding")]
#[test]
fn image_settings_report_stored_choices() {
    use grafton_ndi::{
        AlphaMode, ColorPolicy, Downscale, HdrPolicy, ImageFormat, ImageOptions, PngCompression,
        SampleDepth,
    };
    let options = ImageOptions::new(ImageFormat::Png)
        .color(ColorPolicy::Sdk)
        .depth(SampleDepth::Sixteen)
        .alpha(AlphaMode::Keep)
        .hdr(HdrPolicy::Refuse)
        .downscale(Downscale::Half)
        .png_compression(PngCompression::Fast);
    assert_eq!(options.configured_format(), ImageFormat::Png);
    assert_eq!(options.configured_color(), ColorPolicy::Sdk);
    assert_eq!(options.configured_depth(), SampleDepth::Sixteen);
    assert_eq!(options.configured_alpha(), AlphaMode::Keep);
    assert_eq!(options.configured_hdr(), HdrPolicy::Refuse);
    assert_eq!(options.configured_downscale(), Downscale::Half);
    assert_eq!(options.configured_png_compression(), PngCompression::Fast);
}

#[cfg(feature = "advanced")]
#[test]
fn advanced_configuration_is_borrowed_without_native_capability() {
    use grafton_ndi::advanced::{AdvancedConfig, QueueOptions};
    let config = AdvancedConfig::parse(r#"{"test":true}"#).unwrap();
    let options = ReceiverOptions::new(Source::new("x")).config(config.clone());
    assert_eq!(
        options.configured_config().unwrap().expose_json(),
        config.expose_json()
    );
    assert!(FinderOptions::new().configured_config().is_none());
    assert!(SenderOptions::new("x").configured_config().is_none());
    assert!(RouterOptions::new("x").configured_config().is_none());
    assert!(ListenerOptions::new().configured_config().is_none());
    assert!(AdvertiserOptions::new().configured_config().is_none());
    let queue = QueueOptions::new().max_frames(3).max_bytes(2048);
    assert_eq!(
        (queue.configured_max_frames(), queue.configured_max_bytes()),
        (3, 2048)
    );
}
