//! External-caller workflows for cloning, retargeting and resetting options.
use grafton_ndi::{
    discovery::{AdvertiserOptions, ListenerOptions, RecvAdvertisement},
    routing::RouterOptions,
    FinderOptions, ReceiverBandwidth, ReceiverOptions, RuntimeOptions, SenderOptions, Source,
};

#[test]
fn cloned_templates_can_be_retargeted_without_rebuilding_other_settings() {
    let receiver =
        ReceiverOptions::snapshot_preset(Source::new("Camera A")).name("Multiview input");
    let another = receiver
        .clone()
        .source(Source::new("Camera B"))
        .clear_name();
    assert_eq!(receiver.configured_source().name(), "Camera A");
    assert_eq!(another.configured_source().name(), "Camera B");
    assert_eq!(another.configured_bandwidth(), ReceiverBandwidth::Lowest);
    assert_eq!(
        another.configured_color_format(),
        receiver.configured_color_format()
    );
    assert_eq!(
        another.configured_allow_video_fields(),
        receiver.configured_allow_video_fields()
    );
    assert_eq!(another.configured_name(), None);

    let sender = SenderOptions::new("Program A")
        .groups("Studio")
        .clock_audio(false)
        .clock_video(false);
    let another = sender.clone().name("Program B").clear_groups();
    assert_eq!(sender.configured_name(), "Program A");
    assert_eq!(another.configured_name(), "Program B");
    assert!(!another.configured_clock_audio());
    assert!(!another.configured_clock_video());
    assert_eq!(another.configured_groups(), None);

    let router = RouterOptions::new("Router A").groups("Studio");
    let another = router.clone().name("Router B").clear_groups();
    assert_eq!(router.configured_groups(), Some("Studio"));
    assert_eq!(another.configured_name(), "Router B");
    assert_eq!(another.configured_groups(), None);
}

#[test]
fn clearing_an_override_restores_none_instead_of_an_empty_override() {
    let finder = FinderOptions::new().groups("").extra_ips("192.0.2.1");
    assert_eq!(finder.configured_groups(), Some(""));
    let inherited = finder.clone().clear_groups().clear_extra_ips();
    assert_eq!(inherited.configured_groups(), None);
    assert_eq!(inherited.configured_extra_ips(), None);
    assert_eq!(finder.configured_extra_ips(), Some("192.0.2.1"));

    assert_eq!(
        AdvertiserOptions::new()
            .servers("")
            .clear_servers()
            .configured_servers(),
        None
    );
    assert_eq!(
        ListenerOptions::new()
            .server("")
            .clear_server()
            .configured_server(),
        None
    );
    let advertisement = RecvAdvertisement::new()
        .allow_monitoring(true)
        .allow_controlling(true)
        .input_group("Camera A");
    let ungrouped = advertisement.clear_input_group();
    assert_eq!(ungrouped.configured_input_group(), None);
    assert!(ungrouped.configured_monitoring());
    assert!(ungrouped.configured_controlling());

    let runtime = RuntimeOptions::new()
        .library_path("/opt/ndi/libndi.so")
        .edition(grafton_ndi::SdkEdition::Advanced)
        .clear_library_path()
        .clear_edition();
    assert_eq!(runtime.configured_library_path(), None);
    assert_eq!(runtime.configured_edition(), None);
}

#[cfg(feature = "advanced")]
#[test]
fn advanced_config_overrides_can_be_removed_from_reused_templates() {
    use grafton_ndi::advanced::{AdvancedConfig, AllocatorLimits};
    let config = AdvancedConfig::parse("{}").unwrap();
    assert!(FinderOptions::new()
        .config(config.clone())
        .clear_config()
        .configured_config()
        .is_none());
    assert!(ReceiverOptions::new(Source::new("Camera"))
        .config(config.clone())
        .clear_config()
        .configured_config()
        .is_none());
    assert!(SenderOptions::new("Program")
        .config(config.clone())
        .clear_config()
        .configured_config()
        .is_none());
    assert!(RouterOptions::new("Router")
        .config(config.clone())
        .clear_config()
        .configured_config()
        .is_none());
    assert!(AdvertiserOptions::new()
        .config(config.clone())
        .clear_config()
        .configured_config()
        .is_none());
    assert!(ListenerOptions::new()
        .config(config)
        .clear_config()
        .configured_config()
        .is_none());

    let limits = AllocatorLimits::new()
        .max_generations(3)
        .max_allocations(8)
        .max_bytes(4096)
        .max_concurrent_calls(2)
        .max_allocation_bytes(1024);
    // A caller can derive a smaller template using only the public read surface.
    let small = limits.max_bytes(limits.configured_max_bytes() / 2);
    assert_eq!(small.configured_max_generations(), 3);
    assert_eq!(small.configured_max_allocations(), 8);
    assert_eq!(small.configured_max_bytes(), 2048);
    assert_eq!(small.configured_max_concurrent_calls(), 2);
    assert_eq!(small.configured_max_allocation_bytes(), 1024);
}

#[cfg(feature = "file-io")]
#[test]
fn file_templates_can_retarget_sources_names_and_inherited_settings() {
    use grafton_ndi::file::{
        FileNaming, FileRuntimeOptions, PlayerOptions, RecorderOptions, SourceSpec,
    };
    let player = PlayerOptions::new("Player A").groups("Studio");
    let renamed = player.clone().name("Player B").clear_groups();
    assert_eq!(renamed.configured_name(), "Player B");
    assert_eq!(renamed.configured_groups(), None);
    assert_eq!(player.configured_groups(), Some("Studio"));

    let recorder = RecorderOptions::new(SourceSpec::new("Camera A").unwrap())
        .format_change_rollover(true)
        .proxy(false);
    let retargeted = recorder
        .clone()
        .source(SourceSpec::new("Camera B").unwrap())
        .naming(FileNaming::Exact)
        .clear_format_change_rollover();
    assert_eq!(retargeted.configured_source().name(), Some("Camera B"));
    assert_eq!(retargeted.configured_format_change_rollover(), None);
    assert!(!retargeted.configured_proxy());
    assert_eq!(recorder.configured_source().name(), Some("Camera A"));

    let runtime = FileRuntimeOptions::new()
        .library_path("/opt/ndi/libndi-file.so")
        .clear_library_path();
    assert_eq!(runtime.configured_library_path(), None);
}

#[cfg(feature = "image-encoding")]
#[test]
fn image_templates_can_change_format_and_retain_processing_choices() {
    use grafton_ndi::{AlphaMode, Downscale, ImageFormat, ImageOptions};
    let png = ImageOptions::new(ImageFormat::Png)
        .alpha(AlphaMode::Discard)
        .downscale(Downscale::Quarter);
    let jpeg = png.clone().format(ImageFormat::Jpeg(85));
    assert_eq!(png.configured_format(), ImageFormat::Png);
    assert_eq!(jpeg.configured_format(), ImageFormat::Jpeg(85));
    assert_eq!(jpeg.configured_alpha(), AlphaMode::Discard);
    assert_eq!(jpeg.configured_downscale(), Downscale::Quarter);
}
