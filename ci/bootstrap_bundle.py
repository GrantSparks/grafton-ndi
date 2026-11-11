#!/usr/bin/env python3
"""Build a deterministic private bootstrap tar; never upload or acquire SDKs."""
from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import sys
import tarfile
import tempfile

sys.dont_write_bytecode = True
import prepare_store
import provision
import sdk_store

HERE = Path(__file__).resolve().parent


def json_bytes(value):
    return (json.dumps(value, indent=2, sort_keys=True) + '\n').encode()


def create_bundle(sources, output, *, catalog=None):
    output = Path(output).expanduser().resolve()
    if output.is_relative_to(HERE.parent):
        raise ValueError('private bootstrap output must remain outside the source checkout')
    catalog = sdk_store.reviewed_catalog(catalog if catalog is not None else HERE/'sdk-packages.json')
    sources = prepare_store.normalized_sources(catalog, sources, provision)
    packages = {key: provision.resolved_package(key, catalog) for key in catalog}
    installs = sorted(key for key, p in packages.items() if p['platform'] in ('linux', 'android'))
    if not installs:
        raise ValueError('bootstrap catalog must contain Linux installations')
    files = {name: (HERE/name).read_bytes() for name in prepare_store.FILES
             if name not in ('sources.json', 'sdk-packages.json')}
    files['sdk-packages.json'] = json_bytes(packages)
    files['sources.json'] = json_bytes(sources)
    files['bootstrap.json'] = json_bytes({'schema': 1, 'packages': sorted(packages),
        'install_packages': installs,
        'files': {name: hashlib.sha256(content).hexdigest() for name, content in sorted(files.items())}})
    output.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with tempfile.NamedTemporaryFile(prefix='.ndi-bootstrap-', dir=output.parent, delete=False) as temporary:
        stage = Path(temporary.name)
    try:
        with stage.open('wb') as stream, gzip.GzipFile(filename='', mode='wb', fileobj=stream, mtime=0) as zipped:
            with tarfile.open(fileobj=zipped, mode='w', format=tarfile.USTAR_FORMAT) as archive:
                for name, content in sorted(files.items()):
                    member = tarfile.TarInfo(name)
                    member.size = len(content)
                    member.mode = 0o600
                    archive.addfile(member, io.BytesIO(content))
        stage.replace(output)
    finally:
        stage.unlink(missing_ok=True)
    return {'schema': 1, 'sha256': prepare_store.digest(output), 'bytes': output.stat().st_size,
            'packages': sorted(packages), 'install_packages': installs}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sources', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--catalog', type=Path)
    args = parser.parse_args(argv)
    try:
        result = create_bundle(args.sources, args.output, catalog=args.catalog)
    except (ValueError, OSError) as error:
        print('Bootstrap bundle creation failed: ' + str(error), file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
