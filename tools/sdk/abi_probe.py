#!/usr/bin/env python3
"""Compare target C/Rust ABI constants from object sections; execute no target code."""
import argparse
import hashlib
import json
import pathlib
import re
import shutil
import subprocess
import tempfile
import tomllib

from audit import find_tool, make_dependencies, run, sha256, tool_identity, write_json, INCLUDE_ENV
import os

ROOT = pathlib.Path(__file__).resolve().parents[2]
IDENTIFIER = re.compile(r'[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*\Z')


def checked_identifier(value):
    if not IDENTIFIER.fullmatch(value):
        raise ValueError('Invalid ABI type/field/constant identifier: ' + value)
    return value


def expressions(spec):
    result = [('pointer.size', 'sizeof(void*)', 'std::mem::size_of::<*const ()>()')]
    for record in spec.get('record', []):
        ctype = checked_identifier(record['c'])
        rtype = checked_identifier(record.get('rust', ctype))
        result += [(ctype + '.size', f'sizeof({ctype})', f'std::mem::size_of::<raw::{rtype}>()'),
                   (ctype + '.align', f'_Alignof({ctype})', f'std::mem::align_of::<raw::{rtype}>()')]
        for field in record.get('fields', []):
            cfield = checked_identifier(field['c'])
            rfield = checked_identifier(field.get('rust', cfield))
            result.append((ctype + '.' + cfield, f'offsetof({ctype}, {cfield})',
                           f'std::mem::offset_of!(raw::{rtype}, {rfield})'))
            if record.get('field_sizes'):
                result.append((ctype + '.' + cfield + '.size', f'sizeof((({ctype}*)0)->{cfield})',
                               '{ let value = std::mem::MaybeUninit::<raw::' + rtype + '>::uninit(); '
                               'unsafe { ndi_field_size(std::ptr::addr_of!((*value.as_ptr()).' + rfield + ')) } }'))
    for item in spec.get('constant', []):
        cexpr, rexpr = checked_identifier(item['c']), checked_identifier(item['rust'])
        result.append((cexpr, cexpr, 'raw::' + rexpr))
    keys = [item[0] for item in result]
    if len(keys) != len(set(keys)):
        raise ValueError('Duplicate ABI probe keys')
    return result


def arch_name(target):
    arch = target.split('-')[0]
    return {'aarch64': 'aarch64', 'arm64': 'aarch64', 'i686': 'i386', 'i586': 'i386'}.get(
        arch, 'arm' if arch.startswith('arm') else arch)


def object_identity(reader, path, target):
    text = run([reader, '--file-headers', str(path)]).stdout
    arch = re.search(r'^Arch: (\S+)', text, re.M)
    fmt = re.search(r'^Format: (\S+)', text, re.M)
    if not arch or not fmt:
        raise RuntimeError('Cannot establish compiled target object identity')
    observed = arch_name(arch[1])
    expected = arch_name(target)
    expected_format = 'Mach-O' if '-apple-' in target else 'COFF' if '-windows-' in target else 'elf'
    if observed != expected or not fmt[1].lower().startswith(expected_format.lower()):
        raise RuntimeError(f'Wrong target object: {arch[1]} / {fmt[1]}, expected {target}')
    return {'architecture': arch[1], 'format': fmt[1]}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--sdk-root', type=pathlib.Path, required=True)
    ap.add_argument('--rust-bindings', type=pathlib.Path, required=True,
                    help='Actual standalone generated/curated Rust ABI module used by the build')
    ap.add_argument('--spec', type=pathlib.Path, default=ROOT / 'api/abi-probes.toml')
    ap.add_argument('--target', required=True)
    ap.add_argument('--cc', default='cc')
    ap.add_argument('--rustc', default='rustc')
    ap.add_argument('--cc-arg', action='append', default=[])
    ap.add_argument('--rustc-arg', action='append', default=[])
    ap.add_argument('--include-header', action='append', default=[])
    ap.add_argument('--output', type=pathlib.Path, required=True)
    a = ap.parse_args()
    stage = 'configuration'
    try:
        a.output.unlink(missing_ok=True)
        with a.spec.open('rb') as f:
            spec = tomllib.load(f)
        if spec.get('schema') != 1:
            raise ValueError('Unsupported ABI spec schema')
        include = next((a.sdk_root / p for p in ['include', 'Include'] if (a.sdk_root / p).is_dir()), None)
        if include is None:
            raise ValueError('SDK include directory not found')
        reader, copy = find_tool('llvm-readobj'), find_tool('llvm-objcopy')
        if not reader or not copy:
            raise ValueError('LLVM readobj/objcopy are required for target object inspection')
        values = expressions(spec)
        section = '__DATA,__ndi_abi' if '-apple-' in a.target else '.ndi_abi'
        dump_section = '__DATA,__ndi_abi' if '-apple-' in a.target else '.ndi_abi'
        headers = ['Processing.NDI.Lib.h', *spec.get('headers', []), *a.include_header]
        if any(not re.fullmatch(r'[A-Za-z0-9_./-]+', h) or '..' in pathlib.PurePath(h).parts for h in headers):
            raise ValueError('Invalid include header')
        c_source = '#include <stddef.h>\n#include <stdint.h>\n' + ''.join(f'#include "{h}"\n' for h in dict.fromkeys(headers))
        c_source += f'__attribute__((used, section("{section}"))) const uint64_t ndi_abi_values[] = {{\n'
        c_source += ',\n'.join('(uint64_t)(' + c + ')' for _, c, _ in values) + '\n};\n'
        bindings = a.rust_bindings.resolve(strict=True)
        rust_source = '#![allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals, unused_imports)]\n'
        rust_source += 'mod raw { include!(' + json.dumps(str(bindings)) + '); }\n'
        rust_source += 'const fn ndi_field_size<T>(_: *const T) -> usize { std::mem::size_of::<T>() }\n'
        rust_source += '#[used]\n#[no_mangle]\n#[link_section = ' + json.dumps(section) + ']\n'
        rust_source += f'pub static ndi_abi_values: [u64; {len(values)}] = [\n'
        rust_source += ',\n'.join('(' + r + ') as u64' for _, _, r in values) + '\n];\n'
        with tempfile.TemporaryDirectory(prefix='ndi-abi-') as temporary:
            tmp = pathlib.Path(temporary)
            (tmp / 'probe.c').write_text(c_source)
            (tmp / 'probe.rs').write_text(rust_source)
            c_cmd = [a.cc, '-std=c11', '-Werror', '-I', str(include.resolve()), *a.cc_arg,
                     '-c', str(tmp / 'probe.c'), '-o', str(tmp / 'c.o')]
            r_cmd = [a.rustc, '--edition=2021', '--target', a.target, '--crate-type=lib', '--emit=obj',
                     *a.rustc_arg, str(tmp / 'probe.rs'), '-o', str(tmp / 'rust.o')]
            stage = 'c-compile'
            run(c_cmd)
            stage = 'rust-compile'
            run(r_cmd)
            stage = 'dependency-inventory'
            dependencies = run([a.cc, '-std=c11', '-I', str(include.resolve()), *a.cc_arg,
                                '-M', '-MT', 'probe', str(tmp/'probe.c')]).stdout
            transitive = {}
            for value in make_dependencies(dependencies, target='probe'):
                path = pathlib.Path(value).resolve(strict=True)
                if path == tmp/'probe.c':
                    continue
                key = '<SDK>/' + str(path.relative_to(a.sdk_root.resolve())) if path.is_relative_to(a.sdk_root.resolve()) else str(path)
                transitive[key] = sha256(path)
            stage = 'object-identity'
            identities = {lang: object_identity(reader, tmp / (lang + '.o'), a.target) for lang in ['c', 'rust']}
            stage = 'section-extraction'
            for lang in ['c', 'rust']:
                run([copy, '--dump-section', dump_section + '=' + str(tmp / (lang + '.bin')), str(tmp / (lang + '.o'))])
            stage = 'comparison'
            c_bytes, rust_bytes = (tmp / 'c.bin').read_bytes(), (tmp / 'rust.bin').read_bytes()
            if len(c_bytes) != len(values) * 8 or len(rust_bytes) != len(values) * 8:
                raise RuntimeError('Probe section size differs from expected fixed u64 array')
            mismatches = [key for i, (key, _, _) in enumerate(values) if c_bytes[i*8:(i+1)*8] != rust_bytes[i*8:(i+1)*8]]
            stage = 'record-identity'
            result = {'schema': 1, 'target': a.target, 'passed': not mismatches,
                      'comparison': 'independent target C/Rust object constant sections; target code not executed',
                      'probe_count': len(values), 'mismatches': mismatches, 'object_identity': identities,
                      'spec_sha256': sha256(a.spec), 'rust_bindings_sha256': sha256(bindings),
                      'c_probe_sha256': hashlib.sha256(c_source.encode()).hexdigest(),
                      'rust_probe_sha256': hashlib.sha256(rust_source.encode()).hexdigest(),
                      'c_compiler': tool_identity(a.cc), 'rust_compiler': tool_identity(a.rustc),
                      'c_args': a.cc_arg, 'rust_args': a.rustc_arg,
                      'c_command': [part.replace(str(tmp), '<PROBE>') for part in c_cmd],
                      'rust_command': [part.replace(str(tmp), '<PROBE>') for part in r_cmd],
                      'transitive_c_headers': transitive,
                      'include_environment': {name: os.environ.get(name) for name in INCLUDE_ENV},
                      'tool_sha256': sha256(pathlib.Path(__file__)),
                      'include_headers': headers,
                      'values': {key: {'c_bytes': c_bytes[i*8:(i+1)*8].hex(), 'rust_bytes': rust_bytes[i*8:(i+1)*8].hex()}
                                 for i, (key, _, _) in enumerate(values)},
                      'limits': 'Checks selected layouts/values; not all function signatures, runtime ownership or behavior.'}
            write_json(a.output, result)
            stage = 'comparison'
            print(f'{len(values)} target ABI probes; {len(mismatches)} mismatches')
            if mismatches:
                raise RuntimeError('C/Rust ABI mismatch: ' + ', '.join(mismatches))
    except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired) as e:
        # The CI summary accepts only this fixed stage vocabulary. The complete
        # underlying error remains in the private local log, never an artifact.
        ap.exit(1, f'ABI_PROBE_STAGE={stage}\nERROR: {e}\n')


if __name__ == '__main__':
    main()
