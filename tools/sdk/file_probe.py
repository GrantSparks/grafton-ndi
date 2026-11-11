#!/usr/bin/env python3
"""Compare actual private File ABI records against SDK C headers without execution."""
import argparse
import json
import pathlib
import re
import subprocess
import sys
import tempfile

from audit import sha256, write_json

ROOT = pathlib.Path(__file__).resolve().parents[2]
RECORDS = {
    'Source': 'NDIlib_file_recorder_source_t',
    'VideoTrack': 'NDIlib_file_video_track_format_t',
    'SourceFormat': 'NDIlib_file_source_format_t',
    'PlayerOptions': 'NDIlib_file_player_create_t',
    'RecorderOptions': 'NDIlib_file_recorder_create_t',
    'PlayerEvent': 'NDIlib_file_player_event_t',
    'RecorderEvent': 'NDIlib_file_recorder_event_t',
    'FileInfo': 'NDIlib_file_player_file_info_t',
    'PlayerStats': 'NDIlib_file_player_stats_t',
    'RecorderStats': 'NDIlib_file_recorder_stats_t',
    'Metadata': 'NDIlib_metadata_frame_t',
}
RENAMED_FIELDS = {'kind': 'type', 'path': 'p_path', 'message': 'p_message'}


def production_records(source):
    # Fail closed when syntax changes; this extracts verbatim definitions,
    # including derives, field types/order and array extents. Only visibility
    # changes so a standalone probe can access private production records.
    pattern = re.compile(r'#\[repr\(C\)\]\s*(?:#\[[^\n]+\]\s*)*'
                         r'pub\(super\) struct (?P<name>\w+)\s*\{(?P<body>[^{}]*)\}')
    found = list(pattern.finditer(source))
    names = [item['name'] for item in found]
    if len(names) != len(set(names)) or set(names) != set(RECORDS):
        raise ValueError('Production File ABI records changed; explicitly review probe mapping: ' + repr(names))
    if source.count('#[repr(C)]') != len(found):
        raise ValueError('Unparsed production File repr(C) definition requires review')
    definitions, records = ['use std::ffi::c_char;'], []
    for item in found:
        definitions.append(item[0].replace('pub(super) struct', 'pub struct', 1))
        fields = re.findall(r'(?m)^\s*pub (\w+):\s*[^\n]+,\s*$', item['body'])
        if len(fields) != len([line for line in item['body'].splitlines() if line.strip()]):
            raise ValueError('Unparsed production File field requires review: ' + item['name'])
        records.append({'c': RECORDS[item['name']], 'rust': item['name'], 'fields': fields})
    return definitions, records


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--sdk-root', type=pathlib.Path, required=True)
    ap.add_argument('--repo', type=pathlib.Path, default=ROOT)
    ap.add_argument('--target', required=True)
    ap.add_argument('--cc', default='cc')
    ap.add_argument('--rustc', default='rustc')
    ap.add_argument('--cc-arg', action='append', default=[])
    ap.add_argument('--rustc-arg', action='append', default=[])
    ap.add_argument('--output', type=pathlib.Path, required=True)
    a = ap.parse_args()
    try:
        a.output.unlink(missing_ok=True)
        source = a.repo/'src/file/ffi.rs'
        definitions, records = production_records(source.read_text())
        spec = ['schema = 1', 'headers = ["Processing.NDI.Lib.File.h"]']
        for item in records:
            spec += ['[[record]]', 'c = '+json.dumps(item['c']), 'rust = '+json.dumps(item['rust']),
                     'field_sizes = true', 'fields = [']
            for field in item['fields']:
                spec.append('  { c = '+json.dumps(RENAMED_FIELDS.get(field, field))+', rust = '+json.dumps(field)+' },')
            spec += [']']
        with tempfile.TemporaryDirectory(prefix='ndi-file-abi-') as temporary:
            path = pathlib.Path(temporary)
            (path/'actual.rs').write_text('\n'.join(definitions)+'\n')
            (path/'spec.toml').write_text('\n'.join(spec)+'\n')
            command = [sys.executable, str(ROOT/'tools/sdk/abi_probe.py'), '--sdk-root', str(a.sdk_root),
                       '--rust-bindings', str(path/'actual.rs'), '--spec', str(path/'spec.toml'),
                       '--target', a.target, '--cc', a.cc, '--rustc', a.rustc, '--output', str(a.output)]
            command += ['--cc-arg='+value for value in a.cc_arg]
            command += ['--rustc-arg='+value for value in a.rustc_arg]
            result = subprocess.run(command)
            if a.output.is_file():
                evidence = json.loads(a.output.read_text())
                evidence['production_source'] = 'src/file/ffi.rs'
                evidence['production_source_sha256'] = sha256(source)
                evidence['production_record_count'] = len(records)
                write_json(a.output, evidence)
            if result.returncode:
                ap.exit(result.returncode)
    except (OSError, ValueError) as error:
        ap.exit(1, 'ERROR: '+str(error)+'\n')


if __name__ == '__main__':
    main()
