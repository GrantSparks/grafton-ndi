"""Exact real-SDK core tests executed individually by the native confinement runner."""

CORE_IGNORE_REASON = "Requires real NDI SDK and reviewed network confinement"

# These names remain present for every supported Cargo feature selection.
# windows_dll_check preserves its existing non-Windows no-op diagnostic.
CORE_TESTS = {
    'diagnose_windows_ci': (
        'test_ndi_environment',
    ),
    'empty_sender_name': (
        'empty_name_uses_sdk_generated_sender_name',
    ),
    'framesync_capture_boundary': (
        'attached_receiver_permits_controls_audio_metadata_and_raw_capture_after_recovery',
    ),
    'grafton_ndi': (
        'finder::tests::concurrent_snapshots_keep_sdk_buffers_exclusive_until_copied',
        'frames::tests::planar_444_rows_copy_without_padding_and_preserve_runtime_proof',
        'frames::tests::planar_444_runtime_proof_outlives_explicit_runtime_handles',
        'framesync::tests::audio_request_reuse_and_rejection_at_both_sdk_entry_points',
        'framesync::tests::receiver_modes_preserve_resource_order_and_exactly_once_destruction',
        'receiver::tests::capture_contention_obeys_zero_and_bounded_budgets',
        'receiver::tests::concurrent_status_pollers_take_once_and_later_publications_survive',
        'receiver::tests::media_source_and_unknown_events_survive_legacy_status_polling',
        'receiver::tests::media_status_is_retained_coalesced_and_consumed_once',
        'receiver::tests::outbound_controls_accept_invocation_without_a_source',
        'receiver::tests::ptz_rejects_invalid_commands_before_calling_sdk',
        'receiver::tests::receive_64_modes_require_the_selected_runtime_before_native_creation',
        'receiver::tests::receive_delay_native_changed_flag_and_capability_failure',
        'receiver::tests::receiver_allocated_strings_free_once_even_when_utf8_is_invalid',
        'receiver::tests::receiver_mode_transitions_preserve_pending_status_and_core_identity',
        'receiver::tests::status_publication_during_waits_is_reported_and_newer_publication_survives',
        'receiver::tests::status_wait_rechecks_publication_and_preserves_it_on_errors',
        'sender::tests::callback_panic_after_submission_keeps_new_owned_frame_recoverable',
        'sender::tests::completion_callback_can_reenter_shared_flush_without_deadlock',
        'sender::tests::completion_handler_can_be_replaced_and_cleared',
        'sender::tests::metadata_send_methods_reuse_validated_text_with_sdk',
        'sender::tests::mixed_sync_flush_and_copy_sends_notify_once_and_preserve_recovery',
        'sender::tests::owned_async_recycles_original_pixel_and_metadata_allocations',
        'sender::tests::sender_drop_notifies_once_for_copy',
        'tests::runtime_tests::test_concurrent_clone_does_not_panic',
        'tests::runtime_tests::test_concurrent_drop_does_not_panic',
        'tests::runtime_tests::test_is_running_consistency',
        'tests::runtime_tests::test_mixed_acquire_release_concurrent',
        'timestamp::tests::native_timestamp_has_utc_100ns_units_or_reports_missing_capability',
        'utilities::tests::native_float_conversion_preserves_channel_order_and_reuses_storage',
        'utilities::tests::native_integer_conversion_respects_explicit_reference_level',
        'utilities::tests::native_interleaved_send_helpers_accept_checked_sources',
        'video_conversion::tests::invalid_buffers_and_mismatched_frames_are_rejected_before_writes',
        'video_conversion::tests::native_complete_groups_roundtrip_with_independent_sample_oracle',
    ),
    'runtime_lifecycle': (
        'handles_keep_runtime_alive_until_last_drop_and_allow_reinitialization',
    ),
    'windows_dll_check': (
        'test_ndi_dll_availability',
    ),
}
