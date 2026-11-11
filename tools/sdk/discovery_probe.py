#!/usr/bin/env python3
"""Independently compare production discovery ABI records with target SDK C headers."""
import argparse
import json
import pathlib
import re
import subprocess
import sys
import tempfile

from audit import sha256, write_json

ROOT = pathlib.Path(__file__).resolve().parents[2]


def extract_record(source, name, exported):
    match = re.search(r'#\[repr\(C\)\]\s*struct\s+' + re.escape(name) + r'\s*\{[^{}]*\}', source)
    if not match:
        raise ValueError('Expected one flat repr(C) production record: ' + name)
    text = re.sub(r'\bstruct\s+' + re.escape(name) + r'\b', 'pub struct ' + exported, match[0], count=1)
    # Visibility is the only field transformation; ABI types/order/attributes
    # come directly from production code, not an independent duplicated model.
    return re.sub(r'(?m)^(\s*)([A-Za-z_]\w*):', r'\1pub \2:', text)


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
        listener = (a.repo/'src/discovery/listener.rs').read_text()
        advertiser = (a.repo/'src/discovery/advertiser.rs').read_text()
        definitions = ['use std::ffi::c_char;',
                       extract_record(listener, 'Create', 'ListenerCreate'),
                       extract_record(advertiser, 'Create', 'AdvertiserCreate')]
        for name in ['RawEvent', 'RawSender', 'RawReceiver']:
            definitions.append(extract_record(listener, name, name))
        for field, alias in [('streams','ReceiverTypeTag'),('commands','ReceiverCommandTag')]:
            pointer = re.search(r'\b'+field+r':\s*\*const\s+([^,]+)', listener)
            if not pointer:
                raise ValueError('Cannot identify production receiver tag type: ' + field)
            definitions.append('pub type '+alias+' = '+pointer[1]+';')
        records = [
            ('NDIlib_send_listener_create_t', 'ListenerCreate', [('p_url_address','url')]),
            ('NDIlib_recv_listener_create_t', 'ListenerCreate', [('p_url_address','url')]),
            ('NDIlib_send_advertiser_create_t', 'AdvertiserCreate', [('p_url_address','url')]),
            ('NDIlib_recv_advertiser_create_t', 'AdvertiserCreate', [('p_url_address','url')]),
            ('NDIlib_listener_event', 'RawEvent', [('p_uuid','uuid'),('p_name','name'),('p_value','value')]),
            ('NDIlib_sender_t','RawSender', [('p_uuid','uuid'),('p_name','name'),('p_metadata','metadata'),('p_address','address'),('port','port'),('p_groups','groups'),('num_groups','num_groups'),('events_subscribed','subscribed')]),
            ('NDIlib_receiver_t','RawReceiver', [('p_uuid','uuid'),('p_name','name'),('p_input_uuid','input_uuid'),('p_input_name','input_name'),('p_address','address'),('p_streams','streams'),('num_streams','num_streams'),('p_commands','commands'),('num_commands','num_commands'),('events_subscribed','subscribed')]),
            ('NDIlib_receiver_type_e','ReceiverTypeTag',[]),
            ('NDIlib_receiver_command_e','ReceiverCommandTag',[]),
        ]
        spec = ['schema = 1']
        for c, rust, fields in records:
            spec += ['[[record]]','c = '+json.dumps(c),'rust = '+json.dumps(rust),'fields = [']
            spec += ['  { c = '+json.dumps(cf)+', rust = '+json.dumps(rf)+' },' for cf,rf in fields]
            spec += [']']
        with tempfile.TemporaryDirectory(prefix='ndi-discovery-abi-') as temporary:
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
                evidence['production_sources'] = {str(path): sha256(a.repo/path) for path in [
                    pathlib.Path('src/discovery/listener.rs'), pathlib.Path('src/discovery/advertiser.rs')]}
                write_json(a.output, evidence)
            if result.returncode:
                ap.exit(result.returncode)
    except (OSError, ValueError) as e:
        ap.exit(1, 'ERROR: '+str(e)+'\n')


if __name__ == '__main__':
    main()
