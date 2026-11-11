"""Resolve target artifacts from Cargo's messages, independent of directory layout."""
from __future__ import annotations
import json
from pathlib import Path


def messages(output):
    result = []
    for line in output.splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue  # Compiler diagnostics may share the captured log.
        if isinstance(value, dict) and value.get('reason'):
            result.append(value)
    return result


def target_file(filename, target_root):
    path = Path(filename).resolve()
    if not path.is_relative_to(Path(target_root).resolve()) or not path.is_file():
        raise ValueError('Cargo target artifact is missing or outside the selected target directory')
    return path


def library(records, name, suffix, target_root):
    candidates = set()
    for record in records:
        if record.get('reason') == 'compiler-artifact' and record.get('target', {}).get('name') == name:
            for filename in record.get('filenames', []):
                if filename.endswith(suffix):
                    candidates.add(target_file(filename, target_root))
    if len(candidates) != 1:
        raise ValueError(f'expected exactly one Cargo target {name} {suffix} artifact')
    return candidates.pop()


def dependency_arguments(records, target_root, *, include_host_proc_macros=True):
    root = Path(target_root).resolve()
    directories = set()
    proc_macro_directories = set()
    for record in records:
        if record.get('reason') != 'compiler-artifact':
            continue
        for filename in record.get('filenames', []):
            path = Path(filename).resolve()
            # Ordinary host libraries cannot satisfy target dependencies.
            if path.is_relative_to(root) and path.suffix in ('.rlib', '.rmeta'):
                directories.add(target_file(path, root).parent)
            elif (include_host_proc_macros
                  and 'proc-macro' in record.get('target', {}).get('kind', [])
                  and path.suffix in ('.so', '.dylib', '.dll')):
                # Rust also loads the host procedural macros referenced by a
                # target crate's metadata (for example thiserror_impl). Cargo
                # records these outside <target-triple>, under its shared root.
                proc_macro_directories.add(target_file(path, root.parent).parent)
    if not directories:
        raise ValueError('Cargo emitted no target dependency artifacts')
    return ['-Ldependency=' + str(path) for path in sorted(directories | proc_macro_directories)]


def native_link_arguments(records, target_root):
    """Retain native search paths declared by this target's dependency builds."""
    root = Path(target_root).resolve()
    manifests = {}
    for record in records:
        if record.get('reason') != 'compiler-artifact':
            continue
        target_artifacts = [Path(name) for name in record.get('filenames', [])
                            if Path(name).resolve().is_relative_to(root)
                            and Path(name).suffix in ('.rlib', '.rmeta')]
        if not target_artifacts:
            continue
        for artifact in target_artifacts:
            target_file(artifact, root)
        manifest = record.get('manifest_path')
        if manifest:
            if not Path(manifest).is_absolute():
                raise ValueError('Cargo target dependency manifest must be absolute')
            package_id = record.get('package_id')
            if not isinstance(package_id, str) or not package_id:
                raise ValueError('Cargo target dependency package identity is missing')
            manifests.setdefault(package_id, set()).add(Path(manifest).resolve())
    arguments = []
    for record in records:
        if record.get('reason') != 'build-script-executed' or not record.get('linked_paths'):
            continue
        output = Path(record.get('out_dir', ''))
        if not output.is_absolute():
            raise ValueError('Cargo native build output directory must be absolute')
        output = output.resolve()
        if not output.is_relative_to(root):
            continue  # A host build script cannot supply target native libraries.
        if not output.is_dir():
            raise ValueError('Cargo native build output directory is missing')
        package_manifests = manifests.get(record.get('package_id'), set())
        if len(package_manifests) != 1:
            raise ValueError('Cargo native build requires one matching target dependency manifest')
        manifest = next(iter(package_manifests))
        if not manifest.is_file():
            raise ValueError('Cargo target dependency manifest is missing')
        for value in record['linked_paths']:
            if not isinstance(value, str) or any(ord(char) < 32 for char in value):
                raise ValueError('invalid Cargo native search path')
            if Path(value).is_absolute():
                kind, directory = 'all', value
            else:
                kind, separator, directory = value.partition('=')
                if not separator or kind not in ('native', 'framework', 'all'):
                    raise ValueError('unreviewed Cargo native search kind')
            path = Path(directory)
            if not path.is_absolute():
                raise ValueError('Cargo native search path must be absolute')
            path = path.resolve()
            if not path.is_dir() or not (path.is_relative_to(root)
                                        or path.is_relative_to(manifest.parent)):
                raise ValueError('Cargo native search path is missing or outside its target dependency')
            argument = '-L' + kind + '=' + str(path)
            if argument not in arguments:
                arguments.append(argument)  # Preserve Cargo's native search precedence.
    return arguments


def rebuilt_std_arguments(records, target_root):
    # Recent Cargo emits metadata separately from its rlib code archive. These
    # object-only ABI probes must consume full metadata, including the compiler's
    # implicitly required builtins, rather than the rlib's metadata stub.
    return [*dependency_arguments(records, target_root, include_host_proc_macros=False),
        *['--extern=' + name + '=' + str(library(records, name, '.rmeta', target_root))
          for name in ('std', 'compiler_builtins')]]


def generated_bindings(records, target_root):
    candidates = set()
    for record in records:
        if record.get('reason') == 'build-script-executed' and record.get('out_dir'):
            path = Path(record['out_dir']) / 'ndi_lib.rs'
            if path.is_file():
                candidates.add(target_file(path, target_root))
    if len(candidates) != 1:
        raise ValueError('expected one generated baseline binding file from Cargo build-script messages')
    return candidates.pop()
