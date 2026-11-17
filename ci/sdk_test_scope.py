#!/usr/bin/env python3
"""Guard three explicit groups of repeated compiler and fixed Rust tests.

The default runtime scope skips audited cases in all-feature compatibility
runs, whose header/target/feature build has a full primary SDK owner. The
synthetic-headers scope leaves its one fixed header fixture matrix to the
current Advanced primary row per host. The fixed-graph scope leaves three
unchanging nested compiler graphs to each SDK-free all-feature owner rather
than repeating them in its default and File-only passes.

Workflow guards verify those owners. This helper guards the actual libtest
inventory. New tests stay included, even in an audited source file.
"""
import argparse
from pathlib import Path
import sys

# Exact audited functions, never module/file prefixes. These lists describe
# independence from the *selected runtime*, not from headers, target or features.
AUDITED = {
    # trybuild compiles API/ownership fixtures; it never executes their Ndi code.
    'tests/compile_contracts.rs': ('compile_contracts',),
    'tests/advanced_contracts.rs': ('advanced_ownership_contracts',),
    # This nested capabilities test replaces NDI_SDK_DIR with synthetic headers.
    # Its only outer-header requirement is eligibility/libclang availability.
    'tests/advanced_headers.rs': (
        'partial_advanced_headers_disable_only_the_missing_declarations',
    ),
    # Real-module rustc control/rejection probes. nested_cargo defaults to the
    # compiler host; send ownership explicitly requests the Advanced graph.
    'tests/internal_capture_permissions.rs': (
        'attached_modes_fail_the_internal_capture_entry_points',
    ),
    'tests/internal_descriptor_parts.rs': (
        'frame_parts_require_the_matching_descriptor_validator',
    ),
    'tests/internal_finder_snapshot_guard.rs': (
        'a_foreign_mutex_cannot_supply_the_finder_snapshot_guard',
    ),
    'tests/internal_send_ownership.rs': (
        'native_loans_and_slot_keys_require_their_owner',
    ),
    # The consumer uses the row target and active features. The other case runs
    # only Tokio's integer future and inspects a type, without constructing Ndi.
    'tests/glob_import_tokio.rs': (
        'root_glob_import_keeps_external_crates_unambiguous',
        'glob_import_leaves_the_tokio_crate_nameable',
    ),
    # Synthetic SDK trees, archive bytes, vendored declarations and rustc-only
    # build.rs/rlib probes. No selected installation or runtime is opened.
    'tests/build_configuration.rs': (
        'advanced_availability_discards_missing_functions_types_and_constants_before_resolution',
        'an_sdk_is_optional_but_an_explicit_root_must_be_one',
        'the_selected_static_archives_generate_typed_runtime_editions',
        'an_installation_is_advanced_when_it_has_the_advanced_header',
        'only_headers_without_the_mit_grant_are_generated_locally',
        'header_patterns_escape_every_separator',
        'only_apple_mobile_targets_link_statically',
        'static_targets_keep_each_supported_slice_and_deployment_variable',
        'static_target_cfg_must_agree_with_its_exact_triple',
        'unsupported_apple_abis_fail_before_the_build_script_opens_an_sdk',
        'mobile_file_is_the_sole_archive_provider_and_old_sdk_remains_usable',
        'the_static_archive_of_the_sdk_decides_the_edition',
        'declaration_scan_agrees_with_a_full_parse_of_the_vendored_declarations',
        'static_map_requires_both_a_declaration_and_an_actual_archive_export',
        'directory_preserves_actual_entry_spelling_on_every_host',
        'directory_rejects_distinct_matching_aliases',
        'directory_accepts_same_target_aliases_with_declared_preference',
        'apple_static_validator_distinguishes_device_from_simulator_not_just_cpu',
        'static_addresses_include_only_defined_external_symbols',
        'apple_thinning_selects_validated_bytes_for_both_fat_widths_and_byte_orders',
        'apple_fat_tables_reject_ambiguous_out_of_bounds_and_mismatched_slices',
        'rustc_can_bundle_the_materialized_archive_without_an_apple_sdk',
    ),
    # Generic has!/lacks! bounds and empty functions prove positive and negative
    # auto traits. The row's feature/header/target changes still get full owners.
    'tests/public_auto_traits.rs': (
        'split_views_stay_on_the_borrowing_thread',
        'crate_root_types_keep_their_auto_traits',
        'feature_error_payloads_keep_their_auto_traits',
        'image_encoding_types_keep_their_auto_traits',
        'tokio_types_keep_their_auto_traits',
        'smol_types_keep_their_auto_traits',
        'discovery_types_keep_their_auto_traits',
        'advanced_modes_types_keep_their_auto_traits',
        'advanced_types_keep_their_auto_traits',
        'file_types_keep_their_auto_traits',
        'media_descriptions_are_hashable_values',
    ),
    # Builders, owned/borrowed buffers, metadata and validated packet values.
    # send_tail_room is a target cfg constant, not a runtime capability query.
    'tests/api_ergonomics.rs': (
        'video_construction_uses_the_same_defaults_and_preserves_explicit_timing',
        'raw_frames_share_metadata_and_timecode_editing',
        'compressed_and_scatter_frames_share_owned_frame_editing',
    ),
    'tests/frame_storage.rs': (
        'rejected_builders_keep_original_storage_and_can_be_repaired',
        'rejected_lengths_do_not_change_buffers_and_swapping_reuses_both_allocations',
        'complete_send_size_includes_scan_rows_and_stride_and_enables_direct_sending',
        'compressed_and_scatter_round_trips_preserve_payload_pools',
    ),
    'tests/media_sources.rs': (
        'borrowed_planar_audio_can_be_inspected_generically_and_erased',
        'borrowed_video_can_be_inspected_generically_and_erased',
        'borrowed_audio_rejects_invalid_layouts_and_metadata_before_native_access',
        'concrete_and_erased_video_accessors_agree_for_every_application_layout',
        'concrete_and_erased_audio_accessors_agree_for_every_application_layout',
        'one_padded_row_keeps_active_data_separate_from_storage_and_sdk_tail',
    ),
    # Options retain values only. AdvancedConfig::parse uses serde_json and Arc;
    # RuntimeOptions/FileApiOptions are never used to open an installation.
    'tests/options_templates.rs': (
        'cloned_templates_can_be_retargeted_without_rebuilding_other_settings',
        'clearing_an_override_restores_none_instead_of_an_empty_override',
        'advanced_config_overrides_can_be_removed_from_reused_templates',
        'file_templates_can_retarget_sources_names_and_inherited_settings',
        'image_templates_can_change_format_and_retain_processing_choices',
    ),
    'tests/configuration_readback.rs': (
        'receiver_options_report_presets_and_last_setter_wins',
        'other_core_settings_are_stored_without_runtime_selection',
        'discovery_settings_preserve_unset_values',
        'file_settings_keep_optional_rollover_distinct',
        'image_settings_report_stored_choices',
        'advanced_configuration_is_borrowed_without_native_capability',
        'options_equality_compares_independently_constructed_settings',
    ),
    # Fixed report values and wrapping sequences; no Receiver or Ndi is created.
    'src/receiver.rs': tuple('receiver::latest_report_tests::' + name for name in (
        'publishing_coalesces_and_taking_consumes_the_latest_value',
        'the_current_report_can_be_replayed_after_it_is_taken',
        'an_older_report_cannot_follow_a_newer_report',
        'only_the_newest_abandoned_value_is_replayed',
        'taking_a_newer_publication_does_not_readmit_an_older_report',
        'replay_order_survives_sequence_wrap',
    )),
    # Dummy instance storage and thread-local fake callbacks. Real-header table
    # dispatch and header-absent TestSender remain distinct primary builds.
    'src/advanced/kvm_tests.rs': tuple('advanced::kvm::ownership_tests::' + name for name in (
        'empty_touch_finishes_and_failed_lifts_are_retried',
        'rejected_releases_are_retained_and_drop_retries_only_the_inputs_still_pressed',
        'every_held_input_combination_retries_only_failed_releases',
    )),
    # Reserved HashMap/Vec storage and allocation accounting with fixed inputs.
    'src/advanced/reserved_map.rs': tuple('advanced::reserved_map::tests::' + name for name in (
        'insertion_and_removal_reuse_reserved_storage_across_churn_and_rebuilds',
        'a_duplicate_returns_the_original_key_and_value_without_replacing_storage',
        'a_refused_insertion_never_initializes_the_value',
        'the_reserved_bound_refuses_new_items_without_allocating',
        'draining_preserves_pairs_and_keeps_the_storage_for_reuse',
        'an_impossible_reservation_is_fallible',
    )),
}

TARGET_OSES = {
    'x86_64-unknown-linux-gnu': 'linux',
    'i686-unknown-linux-gnu': 'linux',
    'aarch64-unknown-linux-gnu': 'linux',
    'x86_64-pc-windows-msvc': 'windows',
    'i686-pc-windows-msvc': 'windows',
    'aarch64-apple-darwin': 'macos',
    'x86_64-apple-darwin': 'macos',
}
OS_ONLY = {
    'directory_accepts_same_target_aliases_with_declared_preference': {'linux', 'macos'},
    'rustc_can_bundle_the_materialized_archive_without_an_apple_sdk': {'linux'},
}
HEADER_TEST = 'partial_advanced_headers_disable_only_the_missing_declarations'
# A compiler-only or truncated inventory must not masquerade as the full suite.
NATIVE_WITNESS = 'handles_keep_runtime_alive_until_last_drop_and_allow_reinitialization'
FIXED_GRAPH = (
    'attached_modes_fail_the_internal_capture_entry_points',
    'frame_parts_require_the_matching_descriptor_validator',
    'native_loans_and_slot_keys_require_their_owner',
)
SCOPES = ('runtime', 'synthetic-headers', 'fixed-graph')


def expected_tests(target=None, headers=None, scope='runtime'):
    """Exact cases with a retained owner for this specific repeated invocation."""
    if scope == 'fixed-graph':
        return sorted(FIXED_GRAPH)
    if scope == 'synthetic-headers':
        return [HEADER_TEST]
    if scope != 'runtime':
        raise ValueError(f'unsupported SDK test scope: {scope}')
    if target not in TARGET_OSES:
        raise ValueError(f'unsupported SDK scope target: {target}')
    if headers not in {'standard', 'advanced'}:
        raise ValueError(f'unsupported header edition: {headers}')
    names = [name for group in AUDITED.values() for name in group]
    if len(names) != len(set(names)):
        raise ValueError('the audited policy lists a function more than once')
    return sorted(name for name in names
                  if (name != HEADER_TEST or headers == 'advanced')
                  and (name not in OS_ONLY or TARGET_OSES[target] in OS_ONLY[name]))


def skip_arguments(inventory, target=None, headers=None, scope='runtime'):
    """Return safe global libtest --skip arguments; retain every unknown test."""
    names = [line[:-6] for line in inventory.splitlines() if line.endswith(': test')]
    expected = expected_tests(target, headers, scope)
    if names.count(NATIVE_WITNESS) != 1:
        raise ValueError(f'inventory must contain one native lifecycle test: {NATIVE_WITNESS}')
    for name in expected:
        # Libtest --skip matches substrings globally, across all test binaries
        # and doctests. Count occurrences as well as distinct spellings.
        matches = [candidate for candidate in names if name in candidate]
        if matches != [name]:
            raise ValueError(f'audited test {name!r} must match exactly once, without collateral tests; '
                             f'found {matches!r}')
    if scope == 'runtime':
        gated = set(OS_ONLY) | {HEADER_TEST}
        for name in gated.difference(expected):
            if name in names:
                raise ValueError(f'unexpected test {name!r} for {target}, {headers} headers')
        if 'absent_feature_error_variants_are_unavailable' in names:
            raise ValueError('compatibility scope requires the all-features inventory')
    return [argument for name in expected for argument in ('--skip', name)]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--inventory', type=Path, required=True)
    parser.add_argument('--scope', choices=SCOPES, default='runtime')
    parser.add_argument('--target', choices=TARGET_OSES)
    parser.add_argument('--headers', choices=('standard', 'advanced'))
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args(argv)
    if args.scope == 'runtime' and (args.target is None or args.headers is None):
        parser.error('runtime scope requires --target and --headers')
    try:
        inventory = args.inventory.read_text(encoding='utf-8')
        arguments = skip_arguments(inventory, args.target, args.headers, args.scope)
        args.output.write_bytes(''.join(argument + '\n' for argument in arguments).encode('utf-8'))
    except (OSError, ValueError) as error:
        parser.error(str(error))
    print(f'CI {args.scope} scope: skipping {len(arguments) // 2} audited fixed-graph tests '
          f'of {sum(line.endswith(": test") for line in inventory.splitlines())} listed; '
          'all unclassified tests and doctests remain included.', file=sys.stderr)


if __name__ == '__main__':
    main()
