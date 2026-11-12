// Shared by the build-time declaration selector and the runtime tables.
//
// Each entry names its table, the field of `AdvancedApi` that holds it, and
// either the `RuntimeFeature` it is gated and reported under or, for a table
// that is its own gate, the name it is reported under.
advanced_tables! {
    /// Sender creation with a per-instance JSON configuration.
    struct SenderConfiguration in sender_configuration: AdvancedSenderConfiguration {
        fn NDIlib_send_create_v2(p_create_settings: *const NDIlib_send_create_t, p_config_data: *const ::std::os::raw::c_char) -> NDIlib_send_instance_t;
    }

    /// Receiver creation with a per-instance JSON configuration.
    struct ReceiverConfiguration in receiver_configuration: AdvancedReceiverConfiguration {
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

    /// Router creation with a per-instance JSON configuration.
    struct RouterConfiguration in router_configuration: "Advanced router configuration" {
        fn NDIlib_routing_create_v2(p_create_settings: *const NDIlib_routing_create_t, p_config_data: *const ::std::os::raw::c_char) -> NDIlib_routing_instance_t;
    }

    /// Metadata a router replays to each new connection.
    struct RouterConnectionMetadata in router_connection_metadata: "router connection metadata" {
        fn NDIlib_routing_add_connection_metadata(p_instance: NDIlib_routing_instance_t, p_metadata: *const NDIlib_metadata_frame_t);
        fn NDIlib_routing_clear_connection_metadata(p_instance: NDIlib_routing_instance_t);
    }

    /// Sender advertiser creation with a per-instance JSON configuration.
    struct SenderAdvertiserConfiguration in sender_advertiser_configuration: "Advanced sender advertiser configuration" {
        fn NDIlib_send_advertiser_create_ex(p_create_settings: *const NDIlib_send_advertiser_create_t, p_config_data: *const ::std::os::raw::c_char) -> NDIlib_send_advertiser_instance_t;
    }

    /// Receiver advertiser creation with a per-instance JSON configuration.
    struct ReceiverAdvertiserConfiguration in receiver_advertiser_configuration: "Advanced receiver advertiser configuration" {
        fn NDIlib_recv_advertiser_create_ex(p_create_settings: *const NDIlib_recv_advertiser_create_t, p_config_data: *const ::std::os::raw::c_char) -> NDIlib_recv_advertiser_instance_t;
    }

    /// Sender listener creation with a per-instance JSON configuration.
    struct SenderListenerConfiguration in sender_listener_configuration: "Advanced sender listener configuration" {
        fn NDIlib_send_listener_create_ex(p_create_settings: *const NDIlib_send_listener_create_t, p_config_data: *const ::std::os::raw::c_char) -> NDIlib_send_listener_instance_t;
    }

    /// Receiver listener creation with a per-instance JSON configuration.
    struct ReceiverListenerConfiguration in receiver_listener_configuration: "Advanced receiver listener configuration" {
        fn NDIlib_recv_listener_create_ex(p_create_settings: *const NDIlib_recv_listener_create_t, p_config_data: *const ::std::os::raw::c_char) -> NDIlib_recv_listener_instance_t;
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
