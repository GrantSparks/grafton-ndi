#!/usr/bin/env python3
"""Offline SDK inventory/diff. Never loads SDK libraries or executes SDK code."""
import argparse
import csv
import hashlib
import functools
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tomllib

SCHEMA = 2
REPOSITORY = pathlib.Path(__file__).resolve().parents[2]
INCLUDE_ENV = ('CPATH', 'C_INCLUDE_PATH', 'CPLUS_INCLUDE_PATH', 'SDKROOT', 'MACOSX_DEPLOYMENT_TARGET')


def run(args, *, stdin=None, check=True):
    p = subprocess.run(args, input=stdin, text=True, capture_output=True, timeout=180)
    if check and p.returncode:
        raise RuntimeError(f'{args[0]} failed ({p.returncode}): {p.stderr[:4000]}')
    return p


def sha256(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for b in iter(lambda: f.read(1024 * 1024), b''):
            h.update(b)
    return h.hexdigest()


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


@functools.lru_cache(maxsize=None)
def tool_identity(command):
    executable = pathlib.Path(shutil.which(command) or command).resolve(strict=True)
    # rustup chooses a tool from argv[0]; invoking its resolved rustup path would
    # report the manager instead of the selected compiler.
    version = run([command, '--version']).stdout.splitlines()[0]
    if pathlib.Path(command).name == 'rustc':
        compiler = pathlib.Path(run([command, '--print', 'sysroot']).stdout.strip()) / 'bin/rustc'
        if compiler.is_file():
            executable = compiler.resolve()
    return {'path': str(executable), 'sha256': sha256(executable),
            'version': version}


def find_tool(name):
    return shutil.which(name) or next((shutil.which(name + '-' + str(n)) for n in range(25, 10, -1)
                                      if shutil.which(name + '-' + str(n))), None)


def make_dependencies(output, *, target):
    """Decode Clang/GCC Make prerequisites, which are not shell words.

    Clang emits host-native separators and leaves ordinary backslashes intact.
    It escapes spaces and #, doubles $, and leaves quotes literal. In particular,
    shell parsing would destroy Windows paths and POSIX filenames with quotes.
    The caller supplies the exact simple target passed through -MT, so drive
    colons in prerequisites cannot be mistaken for the rule delimiter.
    This follows clang/lib/Frontend/DependencyFile.cpp::PrintFilename (Make).
    """
    logical = re.sub(r'\\\r?\n', ' ', output).lstrip()
    prefix = target + ':'
    if not re.fullmatch(r'[A-Za-z_]\w*', target) or not logical.startswith(prefix):
        raise ValueError('Unexpected compiler dependency rule target')
    lines = logical[len(prefix):].split('\n')
    if any(line.strip() for line in lines[1:]):
        raise ValueError('Expected one compiler dependency rule')
    text = lines[0]
    result, token = [], []
    index = 0
    while index < len(text):
        char = text[index]
        if char in ' \t\r':
            if token:
                result.append(''.join(token)); token = []
            index += 1
        elif char == '\\':
            end = index
            while end < len(text) and text[end] == '\\':
                end += 1
            count = end - index
            following = text[end:end+1]
            if following == ' ' and count % 2:
                # Clang doubles preceding separators before an escaped space.
                token.append('\\' * (count // 2) + ' ')
                index = end + 1
            elif following == '#':
                # Clang/GCC add only one escape before #, even after a slash.
                token.append('\\' * (count - 1) + '#')
                index = end + 1
            else:
                token.append('\\' * count)
                index = end
        elif char == '$':
            if text[index:index+2] != '$$':
                raise ValueError('Unescaped dollar in compiler dependency rule')
            token.append('$'); index += 2
        elif char == '#':
            raise ValueError('Unescaped comment in compiler dependency rule')
        else:
            token.append(char); index += 1
    if token:
        result.append(''.join(token))
    return result


def semantic(entry):
    def canonical(value):
        if isinstance(value, dict):
            # Clang 18 spells the default VisibilityAttr payload explicitly;
            # older Clang JSON contains only the attribute kind. Preserve the
            # raw snapshot, attribute presence and every non-default/unknown
            # payload. Exact header hashes independently gate source changes.
            return {key: canonical(item) for key, item in value.items()
                    if not (value.get('kind') == 'VisibilityAttr'
                            and key == 'visibility' and item == 'default')}
        if isinstance(value, list):
            return [canonical(item) for item in value]
        return value

    return canonical({k: v for k, v in entry.items()
                      if k not in {'locations', 'parameters', 'location_evidence'}})


def ast_attributes(node, root):
    # Do not retain AST addresses or source offsets. Clang versions may expose
    # different attribute payloads; header hashes also gate source-only changes.
    def clean(value):
        if isinstance(value, dict):
            return {k: clean(v) for k, v in value.items() if k not in {'id', 'loc', 'range'}}
        if isinstance(value, list):
            return [clean(v) for v in value]
        return normalized(value, root) if isinstance(value, str) else value
    return [clean(ch) for ch in node.get('inner', []) if ch['kind'].endswith('Attr')]


def normalized(text, root):
    text = text.replace(str(root), '<SDK>')
    return re.sub(r'\((?:anonymous|unnamed) at [^)]*\)', '(anonymous)', text)


def enum_value(node):
    if node.get('kind') == 'ConstantExpr' and 'value' in node:
        return node['value']
    for child in node.get('inner', []):
        value = enum_value(child)
        if value is not None:
            return value
    return None


def constant_expression(node, root):
    # Retain expression structure (including signs/casts), without pretending that
    # locating one integer literal evaluates the full C constant expression.
    result = {k: node[k] for k in ['kind', 'value', 'opcode', 'castKind'] if k in node}
    if 'type' in node:
        result['type'] = normalized(node['type'].get('qualType', ''), root)
    if 'referencedDecl' in node:
        result['reference'] = node['referencedDecl'].get('name')
    if node.get('inner'):
        result['inner'] = [constant_expression(ch, root) for ch in node['inner']]
    return result


def record_fields(node, root):
    fields = []
    for child in node.get('inner', []):
        if child['kind'] == 'RecordDecl':
            fields.append({'kind': child.get('tagUsed', 'record'),
                           'name': child.get('name'),
                           'fields': record_fields(child, root)})
        elif child['kind'] == 'FieldDecl' and not child.get('isImplicit'):
            fields.append({'name': child.get('name'),
                           'type': normalized(child['type']['qualType'], root),
                           'canonical_type': normalized(child['type'].get('desugaredQualType', child['type']['qualType']), root),
                           'attributes': ast_attributes(child, root),
                           'bitfield': bool(child.get('isBitfield')),
                           'bit_width': enum_value(child) if child.get('isBitfield') else None})
    return fields


def locations(node, header_bytes, root):
    loc = node.get('loc', {})
    loc = loc.get('expansionLoc', loc)
    offset, name = loc.get('offset'), node.get('name', '')
    found = []
    explicit = loc.get('file')
    if offset is not None:
        token = name.encode()
        # Clang elides repeated source filenames; exact source-offset/token matches
        # recover the file without relying on mutable AST JSON printer state.
        for file, data in header_bytes.items():
            if explicit and pathlib.Path(explicit).resolve() != root / file:
                continue
            if data[offset:offset + len(token)] == token:
                found.append({'file': file, 'line': data[:offset].count(b'\n') + 1,
                              'column': offset - data.rfind(b'\n', 0, offset)})
    if len(found) != 1:
        raise RuntimeError(f'Ambiguous or unresolved declaration location: {node.get("kind")} {name}: {found}')
    return found


def clang_inventory(root, include, headers, clang, target, extra, extra_headers):
    main = next((p for p in include.iterdir() if p.name.lower() == 'processing.ndi.lib.h'), None)
    if main is None:
        raise RuntimeError('Cannot locate Processing.NDI.Lib.h')
    base = [clang, '-x', 'c', '-std=c11', '-I', str(include)]
    if target:
        base += ['--target=' + target]
    base += extra
    source = f'#include "{main.name}"\n' + ''.join(f'#include "{name}"\n' for name in extra_headers)
    ast_result = run(base + ['-Xclang', '-ast-dump=json', '-fsyntax-only', '-'], stdin=source)
    ast = json.loads(ast_result.stdout)
    header_bytes = {str(p.relative_to(root)): p.read_bytes() for p in headers}
    declarations = {}
    for node in ast.get('inner', []):
        name, kind = node.get('name', ''), node.get('kind')
        if not name.startswith('NDIlib_') or kind not in {'FunctionDecl', 'RecordDecl', 'EnumDecl', 'TypedefDecl', 'VarDecl'}:
            continue
        entry = {'kind': kind, 'name': name, 'locations': locations(node, header_bytes, root)}
        if kind == 'FunctionDecl':
            entry['signature'] = normalized(node['type']['qualType'], root)
            entry['parameters'] = [{'name': ch.get('name'), 'type': normalized(ch['type']['qualType'], root)}
                                   for ch in node.get('inner', []) if ch['kind'] == 'ParmVarDecl']
        elif kind == 'RecordDecl':
            entry.update(tag=node.get('tagUsed'), complete=bool(node.get('completeDefinition')),
                         fields=record_fields(node, root))
        elif kind == 'EnumDecl':
            constants, previous = [], -1
            for ch in node.get('inner', []):
                if ch['kind'] != 'EnumConstantDecl':
                    continue
                value = enum_value(ch)
                if value is None:
                    if any(not c['kind'].endswith('Attr') for c in ch.get('inner', [])):
                        raise RuntimeError(f'Unevaluated explicit enum initializer: {ch["name"]}')
                    value = str(previous + 1)
                previous = int(value)
                constants.append({'name': ch['name'], 'value': value})
            entry['constants'] = constants
        else:
            entry['type'] = normalized(node.get('type', {}).get('qualType', ''), root)
            if kind == 'VarDecl':
                entry['initializer'] = [constant_expression(ch, root) for ch in node.get('inner', []) if not ch['kind'].endswith('Attr')]
        entry['attributes'] = ast_attributes(node, root)
        key = kind + ':' + name
        if key not in declarations or entry.get('complete', True):
            declarations[key] = entry
    layouts_result = run(base + ['-Xclang', '-fdump-record-layouts-complete', '-fsyntax-only', '-'], stdin=source)
    layouts = {}
    for block in layouts_result.stdout.split('*** Dumping AST Record Layout'):
        lines = block.strip().splitlines()
        if not lines:
            continue
        match = re.fullmatch(r'\s*0 \| (struct|union) (NDIlib_\w+)', lines[0])
        if match:
            layouts[match[2]] = [normalized(line.rstrip(), root) for line in lines]
    macro_result = run(base + ['-dM', '-E', '-'], stdin=source)
    macros = {}
    for line in macro_result.stdout.splitlines():
        match = re.match(r'#define ((?:NDI|PROCESSINGNDI)\w*)(.*)', line)
        if match:
            macros[match[1]] = normalized(match[2].strip(), root)
    compiler = run([clang, '--version']).stdout.splitlines()[0]
    compiler_target = run([clang, '-dumpmachine']).stdout.strip()
    dependencies = run(base + ['-M', '-MT', 'audit', '-'], stdin=source).stdout
    # Clang emits Make escaping, including escaped spaces in SDK paths.
    deps = make_dependencies(dependencies, target='audit')
    dependency_hashes = {}
    for dep in sorted(set(deps)):
        path = pathlib.Path(dep).resolve()
        if path.is_file():
            key = '<SDK>/' + str(path.relative_to(root)) if path.is_relative_to(root) else str(path)
            dependency_hashes[key] = sha256(path)
    resource_dir = run([clang, '-print-resource-dir']).stdout.strip()
    return {'compiler': compiler, 'target': target or compiler_target, 'language': 'c11',
            'compiler_identity': tool_identity(clang), 'resource_dir': resource_dir,
            'transitive_headers': dependency_hashes,
            'include_environment': {k: os.environ.get(k) for k in INCLUDE_ENV},
            'command': [normalized(x, root) for x in base],
            'extra_clang_args': extra, 'extra_headers': extra_headers, 'main_header': str(main.relative_to(root)),
            'diagnostics': normalized(ast_result.stderr, root),
            'declarations': declarations, 'record_layouts': layouts, 'macros': macros}


def binary_inventory(path, root):
    entry = {'sha256': sha256(path), 'size': path.stat().st_size}
    with path.open('rb') as stream:
        magic = stream.read(8)
    if magic.startswith(b'\x7fELF') and shutil.which('readelf'):
        header = run(['readelf', '-h', str(path)]).stdout
        entry['format'] = 'ELF'
        entry['machine'] = next((line.split(':', 1)[1].strip() for line in header.splitlines() if 'Machine:' in line), None)
        entry['class'] = next((line.split(':', 1)[1].strip() for line in header.splitlines() if 'Class:' in line), None)
        symbols = run(['readelf', '--dyn-syms', '--wide', str(path)]).stdout
        names, versioned_imports = set(), set()
        for line in symbols.splitlines():
            fields = line.split()
            if len(fields) < 8 or not fields[0].rstrip(':').isdigit():
                continue
            if fields[6] != 'UND' and fields[4] in {'GLOBAL', 'WEAK'} and fields[5] in {'DEFAULT', 'PROTECTED'} and fields[7].startswith('NDIlib_'):
                names.add(fields[7])
            elif fields[6] == 'UND' and '@' in fields[7]:
                versioned_imports.add(fields[7])
        dynamic = run(['readelf', '--dynamic', '--wide', str(path)]).stdout
        entry['ndi_exports'] = sorted(names)
        entry['versioned_imports'] = sorted(versioned_imports)
        entry['needed'] = sorted(re.findall(r'\(NEEDED\).*?\[([^]]+)\]', dynamic))
        entry['soname'] = re.findall(r'\(SONAME\).*?\[([^]]+)\]', dynamic)
        entry['inspection'] = 'readelf static inspection only; binary not loaded'
        entry['evidence_kind'] = 'elf-dynamic-exports'
        entry['inspection_tool'] = tool_identity('readelf')
    elif magic.startswith(b'MZ') and find_tool('llvm-readobj'):
        reader = find_tool('llvm-readobj')
        result = run([reader, '--file-headers', '--coff-exports', '--coff-imports', str(path)]).stdout
        entry.update(format='PE', evidence_kind='pe-export-table',
                     inspection_tool=tool_identity(reader),
                     ndi_exports=sorted(set(re.findall(r'Export \{.*?Name: (_?NDIlib_\w+)', result, re.S))),
                     needed=sorted(set(re.findall(r'Import \{\s*Name: ([^\n]+)', result))))
        entry['machine'] = next((x.split(':', 1)[1].strip() for x in result.splitlines() if x.startswith('Arch:')), None)
    elif magic == b'!<arch>\n':
        nm = find_tool('llvm-nm') or shutil.which('nm')
        entry.update(format='archive', ndi_exports=None, evidence_kind='archive-defined-globals')
        if nm:
            result = run([nm, '-g', '--defined-only', str(path)])
            entry['defined_globals'] = sorted(set(re.findall(r'\b_?(NDIlib_\w+)(?:\s|$)', result.stdout)))
            entry['inspection_tool'] = tool_identity(nm)
        else:
            entry['inspection_error'] = 'No archive symbol reader installed'
    elif magic[:4] in {b'\xca\xfe\xba\xbe', b'\xbe\xba\xfe\xca', b'\xcf\xfa\xed\xfe',
                        b'\xfe\xed\xfa\xcf', b'\xce\xfa\xed\xfe', b'\xfe\xed\xfa\xce',
                        b'\xca\xfe\xba\xbf', b'\xbf\xba\xfe\xca'}:
        dump, nm = find_tool('llvm-objdump'), find_tool('llvm-nm')
        entry.update(format='Mach-O', ndi_exports=None, evidence_kind='macho-per-slice-defined-globals')
        if dump and nm:
            universal = run([dump, '--macho', '--universal-headers', str(path)]).stdout
            arches = re.findall(r'^architecture (\S+)', universal, re.M)
            entry['slices'] = {}
            for arch in arches or ['thin']:
                arch_args = [] if arch == 'thin' else ['--arch=' + arch]
                result = run([nm, *arch_args, '-g', '--defined-only', '--format=posix', str(path)]).stdout
                entry['slices'][arch] = {'defined_globals': sorted(set(re.findall(r'^_?(NDIlib_\w+)\s', result, re.M)))}
            entry['inspection_tool'] = tool_identity(nm)
            entry['inspection'] = 'Defined external symbols per slice; export trie/reexports require link/runtime validation'
        else:
            entry['inspection_error'] = 'LLVM Mach-O tools unavailable'
    else:
        entry['format'] = 'other'
        entry['ndi_exports'] = None
        entry['inspection'] = 'uninspected binary format; hash/size only'
        entry['evidence_kind'] = 'uninspected'
    return entry


def is_library_name(name):
    return 'ndi' in name.lower() and re.search(r'(?:\.so(?:\.\d+)*|\.dll|\.dylib|\.a|\.lib)$', name, re.I) is not None


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def read_snapshot(path):
    manifest = json.loads(path.read_text())
    if manifest.get('schema') != SCHEMA:
        raise RuntimeError('Unsupported manifest schema')
    expected = manifest.get('identity', {}).get('snapshot_sha256')
    unsigned = json.loads(json.dumps(manifest))
    unsigned.get('identity', {}).pop('snapshot_sha256', None)
    if expected != digest(unsigned):
        raise RuntimeError('Snapshot content fingerprint mismatch: ' + str(path))
    return manifest


def validate_profile(args):
    if not args.profile:
        return None
    with args.profiles.open() as file:
        rows = list(csv.DictReader(file, delimiter='\t'))
    matches = [row for row in rows if row['id'] == args.profile]
    if len(matches) != 1:
        raise RuntimeError('Unknown or ambiguous artifact profile: ' + args.profile)
    row = matches[0]
    if row['target'] != args.target or args.edition not in row['editions'].split(','):
        raise RuntimeError('Profile target/edition does not match explicit snapshot configuration')
    return row


def snapshot(args):
    if args.output.resolve().is_relative_to(REPOSITORY):
        raise RuntimeError('SDK snapshots may contain vendor ABI material; select an output path outside the repository')
    root = args.sdk_root.resolve(strict=True)
    include = next((root / name for name in ['include', 'Include'] if (root / name).is_dir()), None)
    if include is None:
        raise RuntimeError('SDK root must contain include/ or Include/')
    headers = sorted(p for p in include.rglob('*') if p.is_file() and p.suffix.lower() == '.h')
    for name in args.include_header:
        if not re.fullmatch(r'[A-Za-z0-9_./-]+', name) or '..' in pathlib.PurePath(name).parts:
            raise RuntimeError('Extra header must be a relative SDK include path')
    profile = validate_profile(args)
    manifest = {'schema': SCHEMA, 'label': args.label, 'headers': {}, 'version_files': {},
                'binaries': {}, 'symlinks': {}, 'limitations': [
                    'API/ABI inventory is not proof of runtime or behavioral compatibility.',
                    'Clang declarations/layouts cover one selected target and preprocessor configuration.',
                    'Umbrella-header AST may omit APIs in optional headers; all header hashes remain inventoried.',
                    'Exported names are not public API commitments; compare vendor headers and documentation.',
                    'SDK code is never executed; runtime version string and actual runtime capabilities are unverified.']}
    for path in headers:
        manifest['headers'][str(path.relative_to(root))] = {'sha256': sha256(path), 'size': path.stat().st_size}
    for path in sorted(root.rglob('*')):
        rel = str(path.relative_to(root))
        if path.is_file() and path.name.lower() == 'version.txt':
            manifest['version_files'][rel] = path.read_text(errors='replace').strip()
        if not is_library_name(path.name):
            continue
        if path.is_symlink():
            manifest['symlinks'][rel] = str(path.readlink())
        elif path.is_file():
            manifest['binaries'][rel] = binary_inventory(path, root)
    manifest['api'] = clang_inventory(root, include, headers, args.clang, args.target, args.clang_arg, args.include_header)
    manifest['identity'] = {'edition': args.edition, 'profile': profile,
                            'headers_sha256': digest(manifest['headers']),
                            'archive_sha256': sha256(args.archive) if args.archive else None,
                            'archive_size': args.archive.stat().st_size if args.archive else None,
                            'source_versions': manifest['version_files'],
                            'tool_sha256': sha256(pathlib.Path(__file__)),
                            'configuration_sha256': digest({k: v for k, v in manifest['api'].items()
                                                           if k not in {'declarations', 'record_layouts', 'macros', 'diagnostics'}})}
    manifest['identity']['snapshot_sha256'] = digest(manifest)
    write_json(args.output, manifest)
    print(f'{args.output}: {len(headers)} headers, {len(manifest["api"]["declarations"])} declarations, {len(manifest["api"]["record_layouts"])} named record layouts, {len(manifest["binaries"])} binaries')


def mapping_diff(old, new):
    return {'added': sorted(new.keys() - old.keys()), 'removed': sorted(old.keys() - new.keys()),
            'changed': {k: {'old': old[k], 'new': new[k]} for k in sorted(old.keys() & new.keys()) if old[k] != new[k]}}


def semantic_declarations(manifest):
    # Source location and parameter names are not ABI changes. Keep signatures,
    # field order/types, enum values, attributes, and record kind/completeness.
    out = {}
    for key, value in manifest['api']['declarations'].items():
        out[key] = semantic(value)
    return out


def compare(args):
    old = read_snapshot(args.old)
    new = read_snapshot(args.new)
    configuration = ['compiler_identity', 'target', 'language', 'extra_clang_args', 'extra_headers', 'include_environment']
    mismatches = [key for key in configuration if old['api'][key] != new['api'][key]]
    if old['identity']['tool_sha256'] != new['identity']['tool_sha256']:
        mismatches.append('audit_tool_source')
    old_external = {k: v for k, v in old['api']['transitive_headers'].items() if not k.startswith('<SDK>/')}
    new_external = {k: v for k, v in new['api']['transitive_headers'].items() if not k.startswith('<SDK>/')}
    external_delta = mapping_diff(old_external, new_external)
    if external_delta['changed']:
        mismatches.append('transitive_external_header_contents')
    result = {'schema': SCHEMA, 'old_label': old['label'], 'new_label': new['label'],
              'old_versions': old['version_files'], 'new_versions': new['version_files'],
              'configuration_mismatches': mismatches,
              'external_headers': external_delta,
              'warning': 'Static inventory diff, not compatibility proof. Review behavior, ownership, layout, platform/edition, and runtime combinations.',
              'headers': mapping_diff(old['headers'], new['headers']),
              'api': mapping_diff(semantic_declarations(old), semantic_declarations(new)),
              'record_layouts': mapping_diff(old['api']['record_layouts'], new['api']['record_layouts']),
              'macros': mapping_diff(old['api']['macros'], new['api']['macros']),
              'binary_inventory': mapping_diff(old['binaries'], new['binaries'])}
    # A version bump changes filenames; correlate runtimes by parent dir + SONAME.
    def exports(m):
        out = {}
        for path, data in m['binaries'].items():
            name = '/'.join([str(pathlib.PurePath(path).parent), '|'.join(data.get('soname') or [pathlib.PurePath(path).name])])
            if name in out:
                raise RuntimeError('Ambiguous runtime identity (same directory/SONAME): ' + name)
            out[name] = {'exports': data['ndi_exports'], 'needed': data.get('needed'),
                         'versioned_imports': data.get('versioned_imports'), 'machine': data.get('machine'),
                         'evidence_kind': data.get('evidence_kind'), 'slices': data.get('slices'),
                         'defined_globals': data.get('defined_globals')}
        return out
    result['runtime_symbols_and_dependencies'] = mapping_diff(exports(old), exports(new))
    write_json(args.output, result)
    for section in ['headers', 'api', 'record_layouts', 'macros', 'runtime_symbols_and_dependencies']:
        delta = result[section]
        print(f'{section}: {len(delta["added"])} added, {len(delta["removed"])} removed, {len(delta["changed"])} changed')
    if mismatches and not args.allow_configuration_change:
        raise RuntimeError('Incomparable compiler/target configuration: ' + ', '.join(mismatches))


DISPOSITIONS = {'implemented', 'planned', 'superseded', 'deprecated', 'equivalent_utility',
                'external_tool', 'not_applicable_target', 'vendor_blocked', 'reviewed_unavailable',
                'abi_type', 'inline_helper'}


def read_registry(path):
    with path.open('rb') as source:
        registry = tomllib.load(source)
    if registry.get('schema') != 1:
        raise RuntimeError('Unsupported registry schema')
    for section in ['declaration', 'header', 'macro', 'value', 'binary_export']:
        entries = registry.get(section, [])
        ids = [row['id'] for row in entries]
        if len(ids) != len(set(ids)):
            raise RuntimeError('Duplicate registry ' + section + ' IDs')
        for row in entries:
            if not row.get('evidence') or not row.get('sha256'):
                raise RuntimeError('Registry entry lacks reviewed hash/evidence: ' + row['id'])
            if any(not re.fullmatch('[0-9a-f]{64}', value) for value in row['sha256']):
                raise RuntimeError('Malformed registry fingerprint: ' + row['id'])
            if section == 'declaration':
                if row.get('disposition') not in DISPOSITIONS or not row.get('earliest_observed'):
                    raise RuntimeError('Declaration lacks explicit disposition/observation: ' + row['id'])
                if not row.get('wrapper') or not row.get('rationale'):
                    raise RuntimeError('Declaration lacks wrapper mapping or rationale: ' + row['id'])
                if row['disposition'] == 'reviewed_unavailable' and not row.get('scope_decision'):
                    raise RuntimeError('Unavailable declaration lacks explicit scope decision: ' + row['id'])
    known = {row['id'] for row in registry.get('declaration', [])}
    group_ids = [row['id'] for row in registry.get('capability', [])]
    if len(group_ids) != len(set(group_ids)):
        raise RuntimeError('Duplicate capability IDs')
    for group in registry.get('capability', []):
        if not group.get('evidence') or group.get('module') not in {'core', 'file'}:
            raise RuntimeError('Capability lacks module/evidence: ' + group['id'])
        for symbol in group.get('required_symbols', []):
            if 'FunctionDecl:' + symbol not in known:
                raise RuntimeError('Capability references unclassified symbol: ' + symbol)
        if 'minimum_runtime' in group and not re.fullmatch(r'\d+\.\d+\.\d+', group['minimum_runtime']):
            raise RuntimeError('Invalid capability minimum runtime')
    return registry


def emit_capabilities(args):
    registry = read_registry(args.registry)
    lines = ['// Generated by tools/sdk/audit.py emit-capabilities; edit reviewed TOML, not this file.',
             '// These requirements do not establish license, hardware or peer availability.',
             'pub const CAPABILITY_REQUIREMENTS: &[(&str, &str, &[&str], Option<(u16, u16, u16)>)] = &[']
    for group in sorted(registry.get('capability', []), key=lambda value: value['id']):
        minimum = 'Some((' + ', '.join(group['minimum_runtime'].split('.')) + '))' if 'minimum_runtime' in group else 'None'
        symbols = '&[' + ', '.join(json.dumps(name) for name in group.get('required_symbols', [])) + ']'
        lines.append(f'    ({json.dumps(group["id"])}, {json.dumps(group["module"])}, {symbols}, {minimum}),')
    text = '\n'.join(lines + ['];', ''])
    if args.check:
        if not args.output.is_file() or args.output.read_text() != text:
            raise RuntimeError('Generated capability metadata is stale: ' + str(args.output))
    else:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text)
    print(f'{len(registry.get("capability", []))} reviewed capability descriptors')


def classify(args):
    manifest = read_snapshot(args.manifest)
    registry = read_registry(args.registry)
    failures, pending, unavailable = [], [], []
    categories = {
        'declaration': {k: digest(semantic(v)) for k, v in manifest['api']['declarations'].items()},
        'header': {pathlib.PurePath(k).name: v['sha256'] for k, v in manifest['headers'].items()},
        'macro': {k: digest(v) for k, v in manifest['api']['macros'].items()},
        'value': {value['name']: digest({'enum': entry['name'], **value})
                  for entry in manifest['api']['declarations'].values() if entry['kind'] == 'EnumDecl'
                  for value in entry['constants']},
    }
    if len(categories['header']) != len(manifest['headers']):
        raise RuntimeError('Duplicate header basenames need explicit registry paths')
    for category, observations in categories.items():
        known = {entry['id']: entry for entry in registry.get(category, [])}
        for name, fingerprint in sorted(observations.items()):
            entry = known.get(name)
            if entry is None:
                failures.append({'category': category, 'id': name, 'reason': 'unclassified'})
            elif fingerprint not in entry['sha256']:
                failures.append({'category': category, 'id': name, 'reason': 'changed_requires_review'})
            elif category == 'declaration' and entry['disposition'] in {'planned', 'vendor_blocked'}:
                pending.append(name)
            elif category == 'declaration' and entry['disposition'] == 'reviewed_unavailable':
                unavailable.append({'id': name, 'wrapper': entry['wrapper'],
                                    'reason': entry['rationale'], 'scope_decision': entry['scope_decision']})
    declared_names = {entry['id'].split(':', 1)[1] for entry in registry.get('declaration', [])
                      if entry['id'].startswith(('FunctionDecl:', 'VarDecl:'))}
    reviewed_exports = {entry['id']: entry for entry in registry.get('binary_export', [])}
    native_names = set()
    for binary in manifest['binaries'].values():
        native_names.update(binary.get('ndi_exports') or [])
        native_names.update(binary.get('defined_globals') or [])
        for slice_info in binary.get('slices', {}).values():
            native_names.update(slice_info.get('defined_globals') or [])
    for raw_name in sorted(native_names):
        name = raw_name.removeprefix('_')
        if name in declared_names:
            continue
        entry = reviewed_exports.get(name)
        if entry is None or digest(name) not in entry['sha256']:
            failures.append({'category': 'binary_export', 'id': name, 'reason': 'unclassified_native_symbol'})
    if args.baseline:
        baseline = read_snapshot(args.baseline)
        if baseline['api']['target'] != manifest['api']['target']:
            raise RuntimeError('Classification baseline target differs from current target')
        removed = set(baseline['api']['declarations']) - set(manifest['api']['declarations'])
        for key in sorted(removed):
            failures.append({'category': 'declaration', 'id': key, 'reason': 'baseline_declaration_removed'})
    result = {'schema': 1, 'snapshot_identity': manifest.get('identity'),
              'registry_sha256': sha256(args.registry), 'classification_failures': failures,
              'implementation_pending': pending,
              'approved_unavailable': unavailable,
              'classification_passed': not failures,
              'implementation_coverage_passed': not failures and not pending,
              'complete_sdk_surface': not failures and not pending and not unavailable,
              'note': 'Implementation coverage excludes individually reviewed unavailable APIs. Neither classification nor implementation coverage establishes runtime/behavioral validation.'}
    write_json(args.output, result)
    print(f'{len(failures)} unclassified/changed observations; {len(pending)} implementation entries pending; {len(unavailable)} explicitly unavailable')
    if failures or (args.require_implemented and pending) or (args.require_complete_surface and (pending or unavailable)):
        raise RuntimeError('Registry review gate failed; see ' + str(args.output))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    snap = sub.add_parser('snapshot')
    snap.add_argument('--sdk-root', type=pathlib.Path, required=True)
    snap.add_argument('--output', type=pathlib.Path, required=True)
    snap.add_argument('--label', default='unlabelled')
    snap.add_argument('--clang', default='clang')
    snap.add_argument('--target', required=True, help='Explicit Clang/Rust target triple for ABI inspection')
    snap.add_argument('--edition', choices=['standard', 'advanced'], required=True)
    snap.add_argument('--archive', type=pathlib.Path, help='Optional original local archive for exact source identity')
    snap.add_argument('--profile', help='Optional exact artifact profile ID, cross-checked against target/edition')
    snap.add_argument('--profiles', type=pathlib.Path, default=REPOSITORY / 'build_profiles.tsv')
    snap.add_argument('--clang-arg', action='append', default=[], help='Repeatable extra Clang argument, e.g. --clang-arg=--sysroot=/path')
    snap.add_argument('--include-header', action='append', default=[], help='Extra header relative to the SDK include directory; repeat for optional/Advanced surfaces')
    snap.set_defaults(func=snapshot)
    diff = sub.add_parser('diff')
    diff.add_argument('--old', type=pathlib.Path, required=True)
    diff.add_argument('--new', type=pathlib.Path, required=True)
    diff.add_argument('--output', type=pathlib.Path, required=True)
    diff.add_argument('--allow-configuration-change', action='store_true', help='Produce an explicitly incomparable configuration diff')
    diff.set_defaults(func=compare)
    review = sub.add_parser('classify', help='Fail unknown/changed declarations, headers and macros until explicitly reviewed')
    review.add_argument('--manifest', type=pathlib.Path, required=True)
    review.add_argument('--registry', type=pathlib.Path, default=REPOSITORY / 'api/ndi-api.toml')
    review.add_argument('--output', type=pathlib.Path, required=True)
    review.add_argument('--require-implemented', action='store_true')
    review.add_argument('--require-complete-surface', action='store_true', help='Also reject explicitly reviewed unavailable APIs')
    review.add_argument('--baseline', type=pathlib.Path, help='Also reject declarations missing from the retained same-target baseline')
    review.set_defaults(func=classify)
    emit = sub.add_parser('emit-capabilities', help='Generate small typed runtime requirement metadata from reviewed registry')
    emit.add_argument('--registry', type=pathlib.Path, default=REPOSITORY / 'api/ndi-api.toml')
    emit.add_argument('--output', type=pathlib.Path, default=REPOSITORY / 'api/capabilities.rs')
    emit.add_argument('--check', action='store_true')
    emit.set_defaults(func=emit_capabilities)
    args = parser.parse_args()
    try:
        args.func(args)
    except (RuntimeError, OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        parser.exit(1, f'ERROR: {error}\n')


if __name__ == '__main__':
    main()
