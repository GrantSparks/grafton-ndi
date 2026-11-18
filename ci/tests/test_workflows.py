"""Guards on workflow configuration and the files CI uses.

`violations` holds the security rules. It is run on the real workflows, which must
comply, and on a list of plausible wrong edits, each of which it must report.
actionlint, in the same job, checks that the files are valid workflows at all.
"""
import copy
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tempfile
import tomllib
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import sdk  # noqa: E402
import workflows  # noqa: E402

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / '.github' / 'workflows'

READ_ONLY = {'contents': 'read'}
# Only deployment jobs and the reusable-workflow caller may request write permissions.
JOB_PERMISSIONS = {
    ('publish.yml', 'ci'): {'contents': 'read', 'pages': 'write', 'id-token': 'write'},
    ('publish.yml', 'publish'): {'contents': 'read', 'id-token': 'write'},
    ('rust.yml', 'deploy-docs'): {'contents': 'read', 'pages': 'write', 'id-token': 'write'},
}
PASSED_SECRETS = {name: f'${{{{ secrets.{name} }}}}' for name in ('NDI_SDK_SOURCES_JSON', 'NDI_SDK_ARCHIVE_TOKEN')}
# Every place where the word `secrets` may occur, as a key or inside any scalar.
SECRET_USES = {
    'rust.yml': {'on.workflow_call.secrets (key)', *(f'jobs.sdk.steps[Provision the SDK].env.{name}'
                                                    for name in PASSED_SECRETS)},
    'publish.yml': {'jobs.ci.secrets (key)', *(f'jobs.ci.secrets.{name}' for name in PASSED_SECRETS)},
}
# Where the secrets exist: not in pull requests from forks, and not in runs that Dependabot starts.
SECRETS_AVAILABLE = ("github.actor != 'dependabot[bot]' && (github.event_name != 'pull_request' || "
                     "github.event.pull_request.head.repo.full_name == github.repository)")
JOB_CONDITIONS = {
    'rust.yml': {'sdk': f"vars.NDI_CI_RELEASED == 'true' && {SECRETS_AVAILABLE}", 'required-ci': 'always()', 'deploy-docs': "${{ !cancelled() && needs.required-ci.result == 'success' && github.event_name == 'push' && github.repository == 'GrantSparks/grafton-ndi' && github.ref == 'refs/heads/main' }}"},
    'publish.yml': {'release': "github.repository == 'GrantSparks/grafton-ndi'"},
}
TRIGGERS = {
    'rust.yml': {'push': {'branches': ['main']}, 'pull_request': {'branches': ['main']}, 'workflow_dispatch': None,
                 'workflow_call': {'secrets': {name: {'required': 'false'} for name in PASSED_SECRETS}}},
    'publish.yml': {'push': {'tags': ['v*']}},
}
# The actions each job uses, in order. The sdk job has SDK files on disk, so it has no
# cache and only downloads the test pictures; the publish job holds the registry identity.
ACTIONS = {
    'rust.yml': {
        'lint': ['actions/checkout', 'actions/setup-python', 'dtolnay/rust-toolchain', 'Swatinem/rust-cache', 'actions/upload-pages-artifact'],
        'test': ['actions/checkout', 'actions/setup-python', 'dtolnay/rust-toolchain', 'Swatinem/rust-cache'],
        'cross': ['actions/checkout', 'dtolnay/rust-toolchain', 'Swatinem/rust-cache'],
        'miri': ['actions/checkout'],
        'hx-pictures': ['actions/checkout', 'actions/upload-artifact'],
        'sdk': ['actions/checkout', 'actions/setup-python', 'dtolnay/rust-toolchain', 'actions/download-artifact'],
        'required-ci': ['actions/checkout'],
        'deploy-docs': ['actions/deploy-pages'],
    },
    'publish.yml': {
        'release': ['actions/checkout'],
        'ci': './.github/workflows/rust.yml',
        'verify': ['actions/checkout', 'dtolnay/rust-toolchain', 'actions/upload-artifact'],
        'publish': ['actions/checkout', 'rust-lang/crates-io-auth-action'],
    },
}
PUBLISH_NEEDS = {'release': None, 'ci': 'release', 'verify': 'ci', 'publish': 'verify'}
OUTSIDE_THE_CHECKOUT = '${{ runner.temp }}'
MANIFEST = '--manifest-path "$GITHUB_WORKSPACE/Cargo.toml"'


def read_workflows():
    return {path.name: path.read_text(encoding='utf-8') for path in sorted(WORKFLOWS.glob('*.yml'))}


def differs(what, actual, expected):
    return [] if actual == expected else [f'{what} is {actual!r}, not {expected!r}']


def violations(texts):
    """Every way the workflows break a rule; empty when they comply."""
    if set(texts) != set(ACTIONS):
        return [f'the workflows are {sorted(texts)}, not {sorted(ACTIONS)}']
    try:
        parsed = {name: workflows.load(text) for name, text in texts.items()}
    except workflows.Unsupported as error:
        return [f'a workflow uses YAML the guards do not read: {error}']
    found = []
    for name, workflow in parsed.items():
        jobs = workflow['jobs']
        found += differs(f'{name}: triggers', workflow.get('on'), TRIGGERS[name])
        found += differs(f'{name}: default permissions', workflow.get('permissions'), READ_ONLY)
        found += differs(f'{name}: places that mention secrets',
                         {place for place, text in workflows.entries(workflow) if 'secrets' in text.lower()},
                         SECRET_USES[name])
        found += differs(f'{name}: job conditions', {job: body['if'] for job, body in jobs.items() if 'if' in body},
                         JOB_CONDITIONS[name])
        found += [f'{name}: {place} lets a failure pass' for place, text in workflows.entries(workflow)
                  if text == 'continue-on-error' and place.endswith(' (key)')]
        for job, body in jobs.items():
            found += differs(f'{name}: permissions of {job}', body.get('permissions'), JOB_PERMISSIONS.get((name, job)))
            used = body['uses'] if 'uses' in body else [step['uses'] for step in body['steps'] if 'uses' in step]
            pinned = [used] if isinstance(used, str) else [re.sub(r'@[0-9a-f]{40}$', '', action) for action in used]
            found += differs(f'{name}: actions of {job}, each pinned to a commit',
                             pinned[0] if isinstance(used, str) else pinned, ACTIONS[name].get(job))
            for step in body.get('steps', []):
                if step.get('uses', '').startswith('dtolnay/rust-toolchain@'):
                    pinned_release = tomllib.loads((ROOT / 'rust-toolchain.toml').read_text())['toolchain']['channel']
                    if not re.fullmatch(r'\d+\.\d+\.\d+', pinned_release):
                        found.append('rust-toolchain.toml: contributor toolchain is not an exact release')
                    found += differs(f'{name}: toolchain of {job}', step.get('with', {}).get('toolchain'), pinned_release)
            if 'runs-on' in body and not re.fullmatch(r'\d+', body.get('timeout-minutes', '')):
                found.append(f'{name}: {job} has no timeout')
    return (found + gate_violations(parsed['rust.yml']) + publish_violations(parsed['publish.yml'])
            + docs_violations(parsed['rust.yml']) + memory_checker_violations(parsed['rust.yml'])
            + repeated_feature_violations(parsed['rust.yml'])
            + sdk_input_violations(parsed['rust.yml']))


def sdk_input_violations(rust):
    """The SDK job gets fixture data and a verified LLVM installer."""
    jobs = rust['jobs']
    sdk_job = jobs['sdk']
    sdk_steps = {step.get('name'): step for step in sdk_job['steps']}
    fixture_steps = {step.get('name'): step for step in jobs['hx-pictures']['steps']}
    names = ('quadrants-420.h264', 'chroma-444.hevc')
    directory = '${{ runner.temp }}/ndi-hx-pictures'
    upload = fixture_steps.get('Upload test pictures', {})
    download = sdk_steps.get('Download test pictures', {})
    found = (differs('rust.yml: SDK picture dependency', sdk_job.get('needs'), 'hx-pictures')
             + differs('rust.yml: SDK picture upload', upload.get('with'), {
                 'name': 'ndi-hx-pictures', 'path': '\n'.join(f'{directory}/{name}' for name in names),
                 'if-no-files-found': 'error', 'retention-days': '1'})
             + differs('rust.yml: SDK picture download', download.get('with'), {
                 'name': 'ndi-hx-pictures', 'path': directory})
             + differs('rust.yml: SDK picture download condition', download.get('if'),
                       "contains(matrix.package, 'advanced')"))
    if download and sdk_job['steps'].index(download) > sdk_job['steps'].index(sdk_steps['Provision the SDK']):
        found.append('rust.yml: SDK pictures must be downloaded before provisioning')

    permitted = {
        'bubblewrap libavahi-client3',
        'libclang-19-dev',
        'gcc-multilib libc6-dev:i386 libavahi-client3:i386',
        'valgrind',
    }
    installs = re.compile(r'\b(apt-get|apt|brew|choco|winget|pip3?|npm|cargo|dnf|yum|pacman)\s+install\b([^\n]*)')
    for step in sdk_steps.values():
        for manager, arguments in installs.findall(step.get('run', '')):
            match = re.fullmatch(r' --yes --no-install-recommends (.+)', arguments)
            if manager != 'apt-get' or not match or match[1] not in permitted:
                found.append(f'rust.yml: SDK package install in {step.get("name")}: {manager} install{arguments}')

    pin = rust.get('env', {})
    if (not re.fullmatch(r'\d+\.\d+\.\d+', pin.get('WINDOWS_LLVM_VERSION', ''))
            or not re.fullmatch(r'[0-9a-f]{64}', pin.get('WINDOWS_LLVM_SHA256', ''))):
        found.append('rust.yml: Windows libclang needs a version and SHA-256 pin')
    script = sdk_steps.get('Windows libclang', {}).get('run', '').splitlines()
    hash_gate = [
        '$digest = (Get-FileHash -Algorithm SHA256 -LiteralPath $installer).Hash',
        'if ($digest -ne $env:WINDOWS_LLVM_SHA256) { throw "LLVM $version SHA-256 mismatch: $digest" }',
    ]
    checks = hash_gate + [
        '$process = Start-Process -Wait -PassThru -FilePath $installer -ArgumentList "/S /D=$directory"',
        'python ci/libclang_version.py',
    ]
    positions = [script.index(line) if line in script else -1 for line in checks]
    if -1 in positions or positions != sorted(positions):
        found.append('rust.yml: Windows libclang must verify the installer hash before running it')
    return found


def docs_violations(docs):
    """Only the checked HTML build reaches the Pages deployment identity."""
    jobs = docs['jobs']
    deploy = jobs['deploy-docs']
    upload = next(step for step in jobs['lint']['steps'] if step.get('name') == 'Upload Pages artifact')
    return (differs('rust.yml: job order', deploy.get('needs'), 'required-ci')
            + differs('rust.yml: artifact condition', upload.get('if'), "github.event_name == 'push' && github.repository == 'GrantSparks/grafton-ndi' && github.ref == 'refs/heads/main'")
            + differs('rust.yml: artifact source', upload.get('with'), {'path': 'target/doc'})
            + differs('rust.yml: deployment environment', deploy.get('environment'),
                      {'name': 'github-pages', 'url': '${{ steps.deployment.outputs.page_url }}'})
            + differs('rust.yml: deployment scripts', [step for step in deploy['steps'] if 'run' in step], [])
            + rustdoc_feature_violations(jobs['lint']['steps']) + implementation_coverage_violations(docs))


def rustdoc_feature_violations(steps):
    """Every linted feature set is documented, before the published full build."""
    named = {step.get('name'): step for step in steps}
    clippy = named.get('Clippy', {})
    rustdoc = named.get('Rustdoc feature combinations', {})

    def features(step):
        loops = re.findall(r'for features in ([^;]+); do', step.get('run', ''))
        return [shlex.split(loop) for loop in loops]

    found = differs('rust.yml: rustdoc feature combinations', features(rustdoc), features(clippy))
    found += differs('rust.yml: rustdoc denies warnings', rustdoc.get('env'), {'RUSTDOCFLAGS': '-D warnings'})
    if 'cargo doc --locked --no-deps --no-default-features ${features:+--features "$features"}' not in rustdoc.get('run', ''):
        found.append('rust.yml: rustdoc feature combinations do not build their selected features')
    names = [step.get('name') for step in steps]
    if 'Rustdoc feature combinations' not in names or names.index('Rustdoc feature combinations') >= names.index('Rustdoc'):
        found.append('rust.yml: all-features rustdoc must be built last for checking and publication')
    return found


REPEATED_FEATURE_TESTS = (
    ('Tests, default features', 'cargo test --locked', 'default'),
    ('Tests, File API without Advanced wrappers',
     'cargo test --locked --no-default-features --features file-io', 'file'),
)


def scoped_commands(command, stem):
    return (
        f'{command} -- --list > "$scope_dir/{stem}-tests.txt"',
        f'python ci/sdk_test_scope.py --scope fixed-graph --inventory "$scope_dir/{stem}-tests.txt" --output "$scope_dir/{stem}-skips.txt"',
        f'{command} -- "${{scope_args[@]}}"',
    )


def repeated_feature_violations(rust):
    """Fixed nested compiler graphs retain one earlier owner on each host."""
    steps = rust['jobs']['test']['steps']
    names = [step.get('name') for step in steps]
    owner = next((step for step in steps if step.get('name') == 'Tests, all features'), {})
    found = differs('rust.yml: fixed compiler owner', owner.get('run'), 'cargo test --locked --all-features')
    if 'if' in owner:
        found.append('rust.yml: fixed compiler owner must run on every test host')
    for name, command, stem in REPEATED_FEATURE_TESTS:
        step = next((step for step in steps if step.get('name') == name), {})
        if 'if' in step:
            found.append(f'rust.yml: repeated feature step {name} must run on every test host')
        for setting in ('env', 'working-directory'):
            if step.get(setting) != owner.get(setting):
                found.append(f'rust.yml: repeated feature step {name} changes fixed compiler inputs: {setting}')
        lines = [line.strip() for line in step.get('run', '').splitlines()]
        required = (*scoped_commands(command, stem),
                    'scope_dir="${RUNNER_TEMP:-target/ci}"', 'mkdir -p "$scope_dir"',
                    'scope_args=()',
                    f'while IFS= read -r argument; do scope_args+=("$argument"); done < "$scope_dir/{stem}-skips.txt"')
        for line in required:
            if lines.count(line) != 1:
                found.append(f'rust.yml: repeated feature step {name} lacks one guarded scope operation: {line}')
        if name not in names or 'Tests, all features' not in names or names.index(name) <= names.index('Tests, all features'):
            found.append(f'rust.yml: repeated feature step {name} has no earlier fixed compiler owner')
    return found


def local_recipe_violations(rust, readme):
    """The local recipe runs each documented lint/test step on the same features."""
    section = readme.split('## Running the checks locally', 1)[-1]
    recipe = section.split('```sh\n', 1)[-1].split('```', 1)[0]
    lines = [re.sub(r'^RUSTDOCFLAGS="-D warnings" ', '', line.strip())
             for line in recipe.splitlines()]
    found = []
    checks = (
        ('lint', 'CI scripts and workflows', 'python -m unittest discover -s ci/tests'),
        ('lint', 'Format', 'cargo fmt --all -- --check'),
        ('lint', 'Clippy', 'cargo clippy --locked --workspace --all-targets --all-features -- -D warnings'),
        ('lint', 'Clippy', 'cargo clippy --locked --all-targets --no-default-features ${features:+--features "$features"} -- -D warnings'),
        ('lint', 'Bindings generator tests', 'cargo test --locked --package xtask'),
        ('lint', 'Rustdoc feature combinations', 'cargo doc --locked --no-deps --no-default-features ${features:+--features "$features"}'),
        ('lint', 'Rustdoc', 'cargo doc --locked --no-deps --all-features'),
        ('lint', 'Documentation links and examples', 'python ci/docs.py --compile --rustdoc target/doc'),
        ('test', 'Tests, all features', 'cargo test --locked --all-features'),
    ) + tuple(('test', name, scoped)
              for name, command, stem in REPEATED_FEATURE_TESTS
              for scoped in scoped_commands(command, stem))
    positions = []
    for job, name, command in checks:
        step = next((step for step in rust['jobs'][job]['steps'] if step.get('name') == name), {})
        if command not in [line.strip() for line in step.get('run', '').splitlines()]:
            found.append(f'rust.yml: local recipe step {name} no longer runs {command}')
        if command.startswith('cargo doc '):
            if f'RUSTDOCFLAGS="-D warnings" {command}' not in [line.strip() for line in recipe.splitlines()]:
                found.append(f'ci/README.md: local recipe rustdoc does not deny warnings: {command}')
            if step.get('env') != {'RUSTDOCFLAGS': '-D warnings'}:
                found.append(f'rust.yml: local recipe step {name} does not deny warnings')
        if command not in lines:
            found.append(f'ci/README.md: local recipe omits {command}')
        else:
            positions.append(lines.index(command))
    if positions != sorted(positions):
        found.append('ci/README.md: local recipe steps differ from lint and test order')
    expected = []
    for name in ('Clippy', 'Rustdoc feature combinations'):
        step = next((step for step in rust['jobs']['lint']['steps'] if step.get('name') == name), {})
        expected.extend(shlex.split(loop) for loop in re.findall(r'for features in ([^;]+); do', step.get('run', '')))
    actual = [shlex.split(loop) for loop in re.findall(r'for features in ([^;]+); do', recipe)]
    found += differs('ci/README.md: local recipe feature combinations', actual, expected)
    return found


# The native obligations are independent of their scheduling roles. Keep the
# original configurations explicit so deleting a row cannot shrink coverage.
PRIMARY_SDK_CONFIGURATIONS = {
    ('linux-x64 standard-6.1.1', 'x86_64-unknown-linux-gnu', '6.1.1-standard-linux'),
    ('linux-x64 standard-6.3.2', 'x86_64-unknown-linux-gnu', '6.3.2-standard-linux'),
    ('linux-x64 advanced-6.3.2', 'x86_64-unknown-linux-gnu', '6.3.2-advanced-linux'),
    ('linux-x64 standard-6.4', 'x86_64-unknown-linux-gnu', 'current-standard-linux'),
    ('linux-x64 advanced-6.4', 'x86_64-unknown-linux-gnu', 'current-advanced-linux'),
    ('linux-x86 standard-6.4', 'i686-unknown-linux-gnu', 'current-standard-linux'),
    ('linux-arm64 standard-6.4', 'aarch64-unknown-linux-gnu', 'current-standard-linux'),
    ('windows-x64 standard-6.3.2', 'x86_64-pc-windows-msvc', '6.3.2-standard-windows'),
    ('windows-x64 advanced-6.3.2', 'x86_64-pc-windows-msvc', '6.3.2-advanced-windows'),
    ('windows-x64 standard-6.4', 'x86_64-pc-windows-msvc', 'current-standard-windows'),
    ('windows-x64 advanced-6.4', 'x86_64-pc-windows-msvc', 'current-advanced-windows'),
    ('windows-x86 advanced-6.4', 'i686-pc-windows-msvc', 'current-advanced-windows'),
    ('macos-arm64 standard-6.3.2', 'aarch64-apple-darwin', '6.3.2-standard-apple'),
    ('macos-arm64 advanced-6.3.2', 'aarch64-apple-darwin', '6.3.2-advanced-apple'),
    ('macos-arm64 standard-6.4', 'aarch64-apple-darwin', 'current-standard-apple'),
    ('macos-arm64 advanced-6.4', 'aarch64-apple-darwin', 'current-advanced-apple'),
    ('macos-x64 standard-6.4', 'x86_64-apple-darwin', 'current-standard-apple'),
}
COMPATIBILITY_PACKAGES = {
    'x86_64-unknown-linux-gnu': ('current-advanced-linux', '6.1.1-standard-linux', '6.3.2-advanced-linux'),
    'x86_64-pc-windows-msvc': ('current-advanced-windows', '6.3.2-standard-windows', '6.3.2-advanced-windows'),
    'aarch64-apple-darwin': ('current-advanced-apple', '6.3.2-standard-apple', '6.3.2-advanced-apple'),
}
COMPATIBILITY_ROLES = {'current-headers', 'baseline-headers'}
# advanced_headers invokes nested Cargo for the compiler host, not the row's
# runtime target. Windows x86 therefore shares the x64 host's synthetic fixture.
SYNTHETIC_HEADER_OWNERS = {
    'linux-x64 advanced-6.3.2': 'linux-x64 advanced-6.4',
    'windows-x64 advanced-6.3.2': 'windows-x64 advanced-6.4',
    'windows-x86 advanced-6.4': 'windows-x64 advanced-6.4',
    'macos-arm64 advanced-6.3.2': 'macos-arm64 advanced-6.4',
}


def sdk_matrix_violations(rust):
    """Every crossed build has exactly one primary compiler/configuration owner."""
    rows = rust['jobs']['sdk']['strategy']['matrix']['include']
    primary = [row for row in rows if row.get('role') == 'primary']
    actual = {(row.get('name'), row.get('target'), row.get('package')) for row in primary}
    found = differs('rust.yml: primary SDK configurations', actual, PRIMARY_SDK_CONFIGURATIONS)
    if len(primary) != len(actual) or len({row.get('name') for row in rows}) != len(rows):
        found.append('rust.yml: SDK row names and primary configurations must be unique')
    if any(row.get('role') not in {'primary', *COMPATIBILITY_ROLES} for row in rows):
        found.append('rust.yml: an SDK row has an unknown scheduling role')
    crossed = [row for row in rows if row.get('role') in COMPATIBILITY_ROLES]
    actual_crossed = {(row['role'], row.get('target'), row.get('package'),
                       row.get('compatibility'), row.get('compatibility-advanced')) for row in crossed}
    expected_crossed = {(role, target, *packages)
                        for target, packages in COMPATIBILITY_PACKAGES.items() for role in COMPATIBILITY_ROLES}
    found += differs('rust.yml: independent SDK compatibility directions', actual_crossed, expected_crossed)
    if len(crossed) != len(expected_crossed):
        found.append('rust.yml: SDK compatibility directions must appear exactly once')
    for row in crossed:
        headers = ([row.get('package')] if row['role'] == 'current-headers'
                   else [row.get('compatibility'), row.get('compatibility-advanced')])
        for package in headers:
            owners = [owner for owner in primary
                      if (owner.get('target'), owner.get('package')) == (row.get('target'), package)]
            if len(owners) != 1:
                found.append(f'rust.yml: {row.get("name")} header package {package} has no unique primary build owner')
    for field, owners in (
        ('bindings', {'linux-x64 standard-6.4'}),
        ('apple-mobile', {'macos-arm64 standard-6.4', 'macos-arm64 advanced-6.4'}),
    ):
        found += differs(f'rust.yml: {field} configuration owners',
                         {row.get('name') for row in rows if row.get(field) == 'true'}, owners)
    found += differs('rust.yml: synthetic-header deduplication owners',
                     {row.get('name') for row in rows if row.get('test-scope') == 'synthetic-headers'},
                     set(SYNTHETIC_HEADER_OWNERS))
    for row in primary:
        if row.get('test-scope') == 'synthetic-headers':
            owners = [owner for owner in primary
                      if owner.get('name') == SYNTHETIC_HEADER_OWNERS.get(row['name'])
                      and owner.get('runner') == row.get('runner')
                      and owner.get('package', '').startswith('current-advanced-')
                      and not owner.get('test-scope')]
            if len(owners) != 1:
                found.append(f'rust.yml: {row["name"]} has no unique full synthetic-header owner')
        elif row.get('test-scope'):
            found.append('rust.yml: a primary SDK row has an unaudited test scope')
    if any('compatibility' in row or 'compatibility-advanced' in row for row in primary):
        found.append('rust.yml: primary SDK rows still provision compatibility packages')
    return found


def implementation_coverage_violations(rust):
    """Real Advanced code and both compatibility editions must be checked."""
    named = {step.get('name'): step for step in rust['jobs']['sdk']['steps']}
    advanced = named.get('Lint and document the Advanced implementation', {})
    found = sdk_matrix_violations(rust)
    found += differs('rust.yml: Advanced implementation owner', advanced.get('if'),
                     when("matrix.role == 'primary' && contains(matrix.package, 'advanced')"))
    for command in ('rustup toolchain install 1.88.0 --profile minimal --target "$TARGET"',
                    'cargo +1.88.0 check --locked --all-targets --target "$TARGET" --all-features'):
        if command not in advanced.get('run', ''):
            found.append('rust.yml: Advanced implementation has no MSRV check with installed headers')
    for role, name in (
        ('current-headers', 'Current Advanced headers with baseline runtimes'),
        ('baseline-headers', 'Baseline SDK builds with the current Advanced runtime'),
    ):
        step = named.get(name, {})
        script = step.get('run', '')
        found += differs(f'rust.yml: {name} scheduling role', step.get('if'), when(f"matrix.role == '{role}'"))
        for required in ('failed=0', '--all-features -- "${skip_args[@]}" || failed=1', 'exit "$failed"'):
            if required not in script:
                found.append(f'rust.yml: {name} does not accumulate compatibility failures')
        headers = 'advanced' if role == 'current-headers' else '"$edition"'
        required = ('--all-features -- --list > "$inventory"',
                    'python ci/sdk_test_scope.py --inventory "$inventory" --target "$TARGET" '
                    f'--headers {headers} --output "$skip_file"',
                    'while IFS= read -r argument; do skip_args+=("$argument"); done < "$skip_file"')
        if any(command not in script for command in required):
            found.append(f'rust.yml: {name} lacks validated runtime-independent selection')
    for name in ('Tests against the real runtime, all features',
                 'Tests against the real runtime, no default features'):
        step = named.get(name, {})
        found += differs(f'rust.yml: {name} primary owner', step.get('if'), when("matrix.role == 'primary'"))
        script = step.get('run', '')
        if '--skip' in script:
            found.append(f'rust.yml: {name} omits the primary configuration checks')
        if name.endswith('all features'):
            required = ('if [ "${{ matrix.test-scope }}" = synthetic-headers ]; then',
                        '--all-features -- --list > "$inventory"',
                        'python ci/sdk_test_scope.py --scope synthetic-headers --inventory "$inventory" --output "$skip_file"',
                        'cargo test --locked --config ci/offline.toml --target "$TARGET" --no-fail-fast --all-features -- "${skip_args[@]}"')
            if any(command not in script for command in required):
                found.append('rust.yml: primary SDK synthetic-header scope lacks its validated owner selection')
        elif 'sdk_test_scope' in script or 'skip_args' in script:
            found.append('rust.yml: default SDK configuration omits full primary checks')
    return found


# Keep this contract independent of the workflow parser: batching cannot drop a selector or add a skip.
MIRI_BATCHES = {
    'reads': [
        'sdk_copy::tests::',
        'source_retention::tests::',
        'connection_metadata::tests::',
        'finder::budget::tests::',
        'file::out_param::tests::',
    ],
    'media': [
        'frames::descriptor_tests::',
        'pixels::tests::',
        'frames::tests::video_padding_all_formats',
        'frames::tests::video_padding_single_rows',
        'frames::tests::video_padding_encoders',
        'frames::metadata_tests::metadata_constructors',
        'frames::metadata_tests::owned_metadata_states',
        'frames::metadata_tests::audio_timestamp',
        'receiver::mixed_tests',
        'receiver::latest_report_tests::',
        'receiver::pending::tests::',
        'asynchronous::receiver_tests::receiver_delivery_replays_only_while_armed',
        'asynchronous::scoped_tests',
        'framesync::tests::test_framesync_video_ref_uses',
        'framesync::tests::test_framesync_audio_ref_supports',
        'advanced::sync::tests::returned_audio_is_released',
        'advanced::sync::tests::borrowed_avsync_audio',
    ],
    'ownership': [
        'finder::cache_retirement_tests::retirement_',
        'finder::cache_retirement_tests::snapshot_remains_live_',
        'finder::tests::invalidation_prevents_pending_lookup_',
        'advanced::kvm::ownership_tests::',
        'advanced::reserved_map::tests::',
        'advanced::send_slots::tests::',
        'advanced::aliasable::tests::',
        'advanced::packets::payload::tests::',
        'advanced::packets::received::tests::',
        'advanced::queued::boundary_tests::',
        'sender::in_flight::tests::',
        'advanced::send::tests::',
        'advanced::queued::tests::',
        'sender::tests::',
    ],
}
MIRI_SKIPS = [
    'descriptor_admission_and_collection_stay_allocation_free_under_churn',
    'exact_copies_match_standard_utf8_replacement',
    'sdk_copy::tests::expanded_text_cannot_allocate_past_the_snapshot_limit',
    'sdk_copy::tests::omitting_malformed_entries_keeps_their_scan_cost_and_fatal_errors',
    'sdk_copy::tests::rejected_scans_stay_charged_and_short_budgets_are_fatal',
    'finder::budget::tests::aggregate_bytes_are_checked_before_string_allocation',
    'finder::budget::tests::count_and_span_are_checked_before_dereferencing_or_reserving',
    'finder::budget::tests::lossy_expansion_is_charged_before_allocating_the_output',
    'finder::budget::tests::rejected_entries_still_consume_the_scan_budget',
]


def miri_batches(script):
    return [(name, shlex.split(body)) for name, body in
            re.findall(r'^\s*(\w+)\) filters=\((.*?)\);;', script, re.DOTALL | re.MULTILINE)]


def memory_checker_violations(rust):
    """Both interpreters and live padding checks must fail required jobs."""
    jobs = rust['jobs']
    miri = jobs.get('miri', {})
    steps = {step.get('name'): step for step in miri.get('steps', [])}
    setup = steps.get('Pinned Miri toolchain', {}).get('run', '')
    run = steps.get('Initialized rows, metadata and synthetic native guards', {}).get('run', '')
    found = differs('rust.yml: Miri aliasing models',
                    miri.get('strategy', {}).get('matrix', {}).get('model'), ['stacked', 'tree'])
    found += differs('rust.yml: Miri model selection', miri.get('env', {}).get('MIRIFLAGS'),
                     "${{ matrix.model == 'tree' && '-Zmiri-tree-borrows' || '' }}")
    for command in ('rustup toolchain install nightly-2026-11-10 --profile minimal --component miri,rust-src',
                    'cargo +nightly-2026-11-10 miri setup'):
        if command not in setup:
            found.append('rust.yml: Miri setup does not pin the checked nightly and components')
    command = 'cargo +nightly-2026-11-10 miri test --locked --lib --features advanced,file-io,image-encoding,tokio'
    for selection in ('-- --exact "${batch_tests[@]}"', '-- --list'):
        if f'{command} {selection}' not in run:
            found.append('rust.yml: Miri does not build and execute the selected synthetic fixtures with file-io')
    if run.count('-- --list') != 1 or f'inventory=$({command} -- --list)' not in run:
        found.append('rust.yml: Miri must read one interpreted test inventory')
    skips = re.search(r'skips=\((.*?)\)', run, re.DOTALL)
    found += differs('rust.yml: Miri skip policy', shlex.split(skips.group(1)) if skips else [], MIRI_SKIPS)
    batches = miri_batches(run)
    found += differs('rust.yml: Miri audited batches', batches, list(MIRI_BATCHES.items()))
    order = re.search(r'batches=\((.*?)\)', run, re.DOTALL)
    found += differs('rust.yml: Miri batch order', shlex.split(order.group(1)) if order else [], list(MIRI_BATCHES))
    filters = [fixture for _, batch in batches for fixture in batch]
    for fixture in [fixture for batch in MIRI_BATCHES.values() for fixture in batch]:
        if fixture not in filters:
            found.append(f'rust.yml: Miri omits {fixture}')
    for required in ('batch_tests+=("${selected[@]}")', '-- --exact "${batch_tests[@]}"'):
        if required not in run:
            found.append('rust.yml: Miri does not run exact inventory selections')
    if '--test-threads=1' not in run:
        found.append('rust.yml: Miri batching increases inter-test concurrency')
    for required in ('selected_names=()', '"$test" = "$previous"', 'Ambiguous test selection:'):
        if required not in run:
            found.append('rust.yml: Miri lacks the overlapping-selection guard')

    test = next((step for step in jobs['test']['steps']
                 if step.get('name') == 'Memcheck over scripted capture and encoder padding'), {})
    sdk = next((step for step in jobs['sdk']['steps']
                if step.get('name') == 'Memcheck over SDK padding'), {})
    found += differs('rust.yml: scripted Memcheck condition', test.get('if'), "runner.os == 'Linux'")
    found += differs('rust.yml: SDK Memcheck condition', sdk.get('if'),
                     when("matrix.name == 'linux-x64 standard-6.3.2'"))
    for owner, step in (('test', test), ('sdk', sdk)):
        command = step.get('run', '')
        for required in ('valgrind', '--tool=memcheck', '--error-exitcode=99'):
            if required not in command:
                found.append(f'rust.yml: {owner} Memcheck does not fail on detector errors')
        if owner not in jobs['required-ci'].get('needs', []):
            found.append(f'rust.yml: {owner} Memcheck is not required')
    for required in ('--trace-children=yes', '--trace-children-skip=', '--test mixed_capture_boundary',
                     '--test frame_metadata_boundary', 'filters=(frames::tests::video_padding_encoders)'):
        if required not in test.get('run', ''):
            found.append('rust.yml: scripted Memcheck omits active SDK-free fixtures or their child processes')
    for required in ('-- --list "$filter"', "if ! grep -q ': test$'", 'No tests selected for filter: $filter', 'exit 1'):
        if required not in test.get('run', ''):
            found.append('rust.yml: scripted Memcheck lacks the empty-selection guard')
    for required in ('select_tests "$filter" true', 'No tests selected for filter: $filter', 'exit 1'):
        if required not in run:
            found.append('rust.yml: Miri lacks the empty-selection guard')
    for required in ('select_tests "$skip" false', 'No tests selected for skip: $skip',
                     'Ambiguous skip selection:', 'skip_args+=(--skip "$skip")'):
        if required not in run:
            found.append('rust.yml: Miri lacks the skip-selection guard')
    for required in ('ci/offline/linux.sh', '--test video_padding_sdk', '--features image-encoding'):
        if required not in sdk.get('run', ''):
            found.append('rust.yml: live Memcheck omits SDK padding or loopback isolation')
    if 'miri' not in jobs['required-ci'].get('needs', []):
        found.append('rust.yml: Miri is not required')
    return found


def gate_violations(rust):
    """required-ci waits for every check; deployment follows the gate."""
    jobs = rust['jobs']
    gate = jobs['required-ci']
    step = gate['steps'][-1]
    return (differs('rust.yml: jobs required-ci waits for', sorted(gate.get('needs') or []),
                    sorted(set(jobs) - {'required-ci', 'deploy-docs'}))
            + differs('rust.yml: the gate step', (step.get('run'), step.get('env')), ('python3 ci/gate.py', {
                'RESULTS': '${{ toJSON(needs) }}', 'SDK_EXPECTED': f'${{{{ {SECRETS_AVAILABLE} }}}}'}))
            + differs('rust.yml: the step given the secrets',
                      [(item.get('name'), item['env']) for item in jobs['sdk']['steps']
                       if any('secrets' in value.lower() for value in (item.get('env') or {}).values())],
                      [('Provision the SDK', PASSED_SECRETS)]))


def publish_violations(publish):
    """The job that holds the publishing identity runs nothing from the repository."""
    jobs = publish['jobs']
    job = jobs['publish']
    scripts = [step for step in job['steps'] if 'run' in step]
    text = '\n'.join(step['run'] for step in scripts)
    cargo = re.findall(r'^\s*cargo (\w+)(.*)$', text, re.MULTILINE)
    found = (differs('publish.yml: job order', {name: body.get('needs') for name, body in jobs.items()}, PUBLISH_NEEDS)
             + differs('publish.yml: secrets passed to CI', jobs['ci'].get('secrets'), PASSED_SECRETS)
             + differs('publish.yml: environment of publish', job.get('environment'), 'crates-io')
             + differs('publish.yml: values publish takes from verify', job.get('env'), {
                 'VERIFIED_SHA256': '${{ needs.verify.outputs.sha256 }}',
                 'VERIFIED_TOOLCHAIN': '${{ needs.verify.outputs.toolchain }}'})
             + differs('publish.yml: cargo commands in publish', [command for command, _ in cargo], ['package', 'publish'])
             + differs('publish.yml: where the registry token goes',
                       {place: value for place, value in workflows.entries(publish) if 'steps.auth' in value.lower()},
                       {'jobs.publish.steps[Publish].env.CARGO_REGISTRY_TOKEN': '${{ steps.auth.outputs.token }}'}))
    # Cargo reads configuration from the working directory upwards, so it must not start in the checkout.
    found += [f'publish.yml: step {step["name"]!r} of publish runs in the checkout'
              for step in scripts if step.get('working-directory') != OUTSIDE_THE_CHECKOUT]
    found += [f'publish.yml: cargo {command} in publish lacks {option}' for command, arguments in cargo
              for option in ('--locked', '--no-verify', MANIFEST) if option not in arguments]
    if text.count('GITHUB_WORKSPACE') != len(cargo) or re.search(r'python|(^|[\s"\'=])(ci|scripts|xtask)/', text):
        found.append('publish.yml: publish reads or runs something from the checkout besides the manifest')
    # The verified digest is compared with the digest of the one named file, not with a file of digests.
    for needed in ('[[ "$VERIFIED_SHA256" =~ ^[0-9a-f]{64}$ ', '[ "$actual" != "$VERIFIED_SHA256" ]',
                   'actual=$(sha256sum "$CARGO_TARGET_DIR/package/grafton-ndi-${GITHUB_REF_NAME#v}.crate" | cut'):
        if needed not in text:
            found.append(f'publish.yml: publish no longer contains {needed!r}')
    return found


# Plausible wrong edits: (name, workflow, text to replace, replacement, words the report must contain).
WRONG_EDITS = (
    ('Advanced MSRV check removed', 'rust.yml',
     '          cargo +1.88.0 check --locked --all-targets --target "$TARGET" --all-features\n', '',
     'MSRV check with installed headers'),
    ('compatibility failure stops later editions', 'rust.yml',
     '            cargo test --locked --config ci/offline.toml --target "$TARGET" --no-fail-fast --all-features -- "${skip_args[@]}" || failed=1\n          done\n          exit "$failed"\n      # A Standard',
     '            cargo test --locked --config ci/offline.toml --target "$TARGET" --no-fail-fast --all-features -- "${skip_args[@]}"\n          done\n          exit "$failed"\n      # A Standard',
     'accumulate compatibility failures'),
    ('toolchain differs from contributor pin', 'publish.yml', '          toolchain: 1.100.0\n',
     '          toolchain: 1.99.1\n', 'toolchain of verify'),
    ('Pages deploy without CI', 'rust.yml', '    needs: required-ci\n', '', 'job order'),
    ('Pages deploy runs a repository script', 'rust.yml', '      - name: Deploy to GitHub Pages\n',
     '      - run: python ci/docs.py\n      - name: Deploy to GitHub Pages\n', 'deployment scripts'),
    ('Pages deploy allows failed CI', 'rust.yml', "!cancelled() && needs.required-ci.result == 'success' &&", "!cancelled() &&", 'job conditions'),
    ('Pages uploads unvalidated files', 'rust.yml', '          path: target/doc\n',
     '          path: target\n', 'artifact source'),
    ('workflow-level id-token', 'publish.yml', 'permissions:\n  contents: read\n\n',
     'permissions:\n  contents: read\n  id-token: write\n\n', 'default permissions'),
    ('write-all on verify', 'publish.yml', '    name: verify package\n',
     '    name: verify package\n    permissions: write-all\n', 'permissions of verify'),
    ('id-token on lint', 'rust.yml', '    name: lint\n', '    name: lint\n    permissions:\n      id-token: write\n',
     'permissions of lint'),
    ('permissions as a flow mapping', 'rust.yml', '    name: lint\n',
     '    name: lint\n    permissions: {id-token: write}\n', 'YAML the guards do not read'),
    ('every secret to a lint step', 'rust.yml', '      - name: Format\n        run:',
     '      - name: Format\n        env:\n          ALL: ${{ toJSON(secrets) }}\n        run:', 'mention secrets'),
    ('an indexed secret to a lint step', 'rust.yml', '      - name: Format\n        run:',
     "      - name: Format\n        env:\n          T: ${{ secrets['NDI_SDK_ARCHIVE_TOKEN'] }}\n        run:",
     'mention secrets'),
    ('a secret in other letter case', 'rust.yml', '      - name: Format\n        run:',
     '      - name: Format\n        env:\n          T: ${{ SECRETS.NDI_SDK_ARCHIVE_TOKEN }}\n        run:',
     'mention secrets'),
    ('a secret inside a script', 'rust.yml', '        run: cargo fmt --all -- --check\n',
     '        run: cargo fmt --all -- --check "${{ secrets.NDI_SDK_ARCHIVE_TOKEN }}"\n', 'mention secrets'),
    ('a secret to a second sdk step', 'rust.yml', '      - name: Vendored bindings are current\n',
     '      - name: Vendored bindings are current\n        env:\n'
     '          NDI_SDK_ARCHIVE_TOKEN: ${{ secrets.NDI_SDK_ARCHIVE_TOKEN }}\n', 'mention secrets'),
    ('all secrets inherited by CI', 'publish.yml', '    secrets:\n      NDI_SDK_SOURCES_JSON: ${{ secrets.NDI_SDK_SOURCES_JSON }}\n'
     '      NDI_SDK_ARCHIVE_TOKEN: ${{ secrets.NDI_SDK_ARCHIVE_TOKEN }}\n', '    secrets: inherit\n', 'mention secrets'),
    ('a job the gate does not wait for', 'rust.yml', '  required-ci:\n',
     '  docs:\n    runs-on: ubuntu-24.04\n    timeout-minutes: 5\n    steps:\n      - run: exit 1\n\n  required-ci:\n',
     'required-ci waits for'),
    ('the gate no longer waits for sdk', 'rust.yml', '    needs: [lint, test, cross, hx-pictures, sdk, miri]\n',
     '    needs: [lint, test, cross]\n', 'required-ci waits for'),
    ('the gate skipped when a job fails', 'rust.yml', '    name: required-ci\n    if: always()\n',
     '    name: required-ci\n', 'job conditions'),
    ('continue-on-error on sdk', 'rust.yml', '    runs-on: ${{ matrix.runner }}\n',
     '    runs-on: ${{ matrix.runner }}\n    continue-on-error: true\n', 'lets a failure pass'),
    ('continue-on-error on a test step', 'rust.yml', '      - name: Tests, all features\n',
     '      - name: Tests, all features\n        continue-on-error: true\n', 'lets a failure pass'),
    ('a condition on lint', 'rust.yml', '    name: lint\n', "    name: lint\n    if: github.event_name == 'push'\n",
     'job conditions'),
    ('Dependabot expected to have secrets', 'rust.yml', "${{ github.actor != 'dependabot[bot]' &&\n            (", '${{ (',
     'the gate step'),
    ('sdk runs for Dependabot', 'rust.yml', " && github.actor != 'dependabot[bot]' &&\n      (", ' &&\n      (',
     'job conditions'),
    ('a cache in the sdk job', 'rust.yml', '      - name: Linux prerequisites\n',
     '      - uses: Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0 # v2.9.3\n'
     '      - name: Linux prerequisites\n', 'actions of sdk'),
    ('an upload from the sdk job', 'rust.yml', '      - name: Vendored bindings are current\n',
     '      - uses: actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9 # v7\n'
     '        with:\n          path: target\n      - name: Vendored bindings are current\n', 'actions of sdk'),
    ('an unreviewed SDK package install', 'rust.yml', '          sudo apt-get install --yes --no-install-recommends libclang-19-dev\n',
     '          sudo apt-get install --yes --no-install-recommends libclang-19-dev ffmpeg\n', 'SDK package install'),
    ('SDK no longer waits for pictures', 'rust.yml', '    needs: hx-pictures\n', '', 'SDK picture dependency'),
    ('LLVM installer skips digest comparison', 'rust.yml',
     '          if ($digest -ne $env:WINDOWS_LLVM_SHA256) { throw "LLVM $version SHA-256 mismatch: $digest" }\n',
     '', 'verify the installer hash'),
    ('an action pinned by tag', 'publish.yml', 'rust-lang/crates-io-auth-action@c6f97d42243bad5fab37ca0427f495c86d5b1a18',
     'rust-lang/crates-io-auth-action@v1', 'pinned to a commit'),
    ('a job without a timeout', 'rust.yml', '    name: cross\n    runs-on: ubuntu-24.04\n    timeout-minutes: 45\n',
     '    name: cross\n    runs-on: ubuntu-24.04\n', 'no timeout'),
    ('pull_request_target', 'rust.yml', '  pull_request:\n    branches: [main]\n',
     '  pull_request_target:\n    branches: [main]\n', 'triggers'),
    ('publishing from a branch push', 'publish.yml', "  push:\n    tags: ['v*']\n",
     "  push:\n    tags: ['v*']\n    branches: [main]\n", 'triggers'),
    ('publish without the environment', 'publish.yml', '    environment: crates-io\n', '', 'environment of publish'),
    ('publish without waiting for verify', 'publish.yml', '    name: publish to crates.io\n    needs: verify\n',
     '    name: publish to crates.io\n    needs: release\n', 'job order'),
    ('cargo package started in the checkout', 'publish.yml',
     '      - name: Re-create the verified package\n        working-directory: ${{ runner.temp }}\n',
     '      - name: Re-create the verified package\n', 'runs in the checkout'),
    ('cargo publish started in the checkout', 'publish.yml',
     '      - name: Publish\n        working-directory: ${{ runner.temp }}\n', '      - name: Publish\n',
     'runs in the checkout'),
    ('cargo publish without the manifest path', 'publish.yml',
     'cargo publish --locked --no-verify --manifest-path "$GITHUB_WORKSPACE/Cargo.toml"',
     'cd "$GITHUB_WORKSPACE" && cargo publish --locked --no-verify', 'cargo commands in publish'),
    ('publish builds the crate', 'publish.yml', 'cargo publish --locked --no-verify', 'cargo publish --locked',
     'lacks --no-verify'),
    ('publish runs a repository script', 'publish.yml', '          rustup toolchain install "$VERIFIED_TOOLCHAIN"',
     '          python3 "$GITHUB_WORKSPACE/ci/release.py" "$GITHUB_REF_NAME"\n'
     '          rustup toolchain install "$VERIFIED_TOOLCHAIN"', 'from the checkout'),
    ('the digest checked against a file of digests', 'publish.yml', 'if [ "$actual" != "$VERIFIED_SHA256" ]; then',
     'if ! sha256sum --check SHA256SUMS; then', 'no longer contains'),
    ('the digest taken unchecked', 'publish.yml', '[[ "$VERIFIED_SHA256" =~ ^[0-9a-f]{64}$ && ', '[[ ',
     'no longer contains'),
    ('the registry token to another step', 'publish.yml',
     '      - name: Re-create the verified package\n        working-directory: ${{ runner.temp }}\n',
     '      - name: Re-create the verified package\n        working-directory: ${{ runner.temp }}\n'
     '        env:\n          T: ${{ steps.auth.outputs.token }}\n', 'registry token'),
    ('a download into the publish job', 'publish.yml', '      - name: Re-create the verified package\n',
     '      - uses: actions/download-artifact@9000827ccba6bdab643e8b6fd33ac0654aef8333 # v8\n'
     '      - name: Re-create the verified package\n', 'actions of publish'),
)


class Rules(unittest.TestCase):
    def test_workflows_break_no_rule(self):
        self.assertEqual(violations(read_workflows()), [])

    def test_every_wrong_edit_is_reported(self):
        texts = read_workflows()
        self.assertEqual(len({name for name, *_ in WRONG_EDITS}), len(WRONG_EDITS))
        for name, workflow, old, new, expected in WRONG_EDITS:
            with self.subTest(name):
                self.assertEqual(texts[workflow].count(old), 1, 'the edit no longer applies to the workflow')
                report = violations({**texts, workflow: texts[workflow].replace(old, new)})
                self.assertTrue(any(expected in line for line in report), report)

    def test_reader_refuses_yaml_it_cannot_see_through(self):
        self.assertEqual(workflows.load('a:\n  - b: c\n    d: [e, \'f\']  # g\n  - h\ni: >-\n  j\n  k\nl: |\n  m\n  # n\n'),
                         {'a': [{'b': 'c', 'd': ['e', 'f']}, 'h'], 'i': 'j k', 'l': 'm\n# n'})
        for text in ('a: &x 1\n', 'a: *x\n', 'a: {b: c}\n', 'a: !!str 1\n', 'a:\n\tb: 1\n', 'a: 1\na: 2\n',
                     'a: b\n  c\n', '---\na: 1\n', 'a:\n- b\n', 'a: "x\\ny"\n', 'a: [b, [c]]\n', 'a: b: c\n'):
            with self.subTest(text=text), self.assertRaises(workflows.Unsupported):
                workflows.load(text)


def checkable(target, pin, *, all_features, headers=None, driver=None, decoders=True):
    """What tests can check in a build for `target` running against the package `pin`.

    `headers` is the package the build reads its headers from, `pin` itself
    unless given, and `driver` the driver package installed beside the runtime.
    This restates the crate's test policy: the Discovery Server tests are Linux
    only and need a 6.4 runtime; the Advanced tests need the `advanced` feature,
    an Advanced runtime and Advanced headers; the File tests need the `file-io`
    feature and a File library the runtime can drive, which is an Advanced
    runtime everywhere except Windows, where the library is edition-neutral and
    ships for x64. The H.264 and HEVC 4:4:4 decode tests are Advanced tests that
    need an NDI|HX decoder, and the HEVC one a 6.4 runtime for its Best444
    receiver. The macOS runtime decodes both itself; on Linux the decoder is the
    NDI|HX driver of the runtime's release. The Windows runtime decodes H.264
    itself, but on the hosted Windows runners it delivers its black placeholder
    frames instead of HEVC 4:4:4, so the HEVC test runs there without being
    counted.
    `decoders=False` leaves both out, as the compatibility runs do.
    """
    platform, advanced = pin['platform'], pin['edition'] == 'advanced'
    declared = (headers or pin)['edition'] == 'advanced'
    current = sdk.version_tuple(pin['version']) >= (6, 4, 0, 0)
    driven = (driver is not None and driver['driver'] == 'hx' and driver['platform'] == platform
              and driver['version'] == pin['version'])
    decoder = decoders and (platform == 'apple' or driven)
    h264 = decoder or (decoders and platform == 'windows')
    found = {'1'}
    if platform == 'linux' and current:
        found.add('discovery-server')
    if all_features and advanced and declared:
        found.add('advanced')
        if h264:
            found.add('h264-decoder')
        if decoder and current:
            found.add('hevc-444-decoder')
    if all_features and current and (target == 'x86_64-pc-windows-msvc' if platform == 'windows' else advanced):
        found.add('file')
    return found


def exported_promises(step, require):
    """The GRAFTON_NDI_REQUIRE_RUNTIME that each edition's pass of a compatibility step exports.

    Bash runs the step's loop with only its lines that set the promise, `matrix.require` replaced by `require`.
    """
    lines = [line for line in step['run'].splitlines()
             if line.lstrip().startswith('for edition in') or 'require' in line.lower()]
    script = '\n'.join(lines + ['  echo "$edition $GRAFTON_NDI_REQUIRE_RUNTIME"', 'done'])
    script = script.replace('${{ matrix.require }}', require)
    if '${{' in script:
        raise AssertionError(f'a promise line reads another expression:\n{script}')
    output = subprocess.run(['bash', '-eo', 'pipefail', '-c', script], capture_output=True, text=True, check=True)
    return {edition: set(promise.split(',')) | {'1'}
            for edition, promise in (line.split(' ', 1) for line in output.stdout.splitlines())}


# What every step of the sdk job after the isolation check runs on: the job was not cancelled and the runner
# was shown to isolate, whatever the steps in between did.
ROW_READY = "!cancelled() && steps.isolation.outcome == 'success'"


def when(condition=None):
    """The `if` of a step of the sdk job that runs on ROW_READY, and on `condition` when one is given."""
    return '${{ ' + ROW_READY + (f' && {condition}' if condition else '') + ' }}'


class SdkJob(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.texts = read_workflows()
        cls.job = workflows.load(cls.texts['rust.yml'])['jobs']['sdk']
        cls.rows = cls.job['strategy']['matrix']['include']
        cls.pins = sdk.load_pins()
        cls.primary = [row for row in cls.rows if row['role'] == 'primary']

    def test_matrix_rows_use_pinned_packages_for_their_target(self):
        self.assertEqual(len(self.rows), 23)
        self.assertEqual(sdk_matrix_violations(workflows.load(self.texts['rust.yml'])), [])
        for row in self.rows:
            with self.subTest(row=row['name']):
                self.assertEqual(sdk.RUNTIMES[row['target']][0], self.pins[row['package']]['platform'])
        self.assertEqual({row['package'].rsplit('-', 1)[0] for row in self.rows},
                         {'6.1.1-standard', '6.3.2-standard', '6.3.2-advanced', 'current-standard', 'current-advanced'})

    def test_a_row_installs_only_a_driver_built_for_its_runtime(self):
        rows = [row for row in self.rows if 'hx-driver' in row]
        self.assertEqual({row['name'] for row in rows},
                         {'linux-x64 advanced-6.4', 'linux-x64 current-headers', 'linux-x64 baseline-headers'})
        for row in rows:
            driver, package = self.pins[row['hx-driver']], self.pins[row['package']]
            self.assertEqual((driver['platform'], driver['version']), (package['platform'], package['version']))
            self.assertIn(row['target'], sdk.DRIVERS[driver['driver']].directories)
        provision = next(step for step in self.job['steps'] if step.get('name') == 'Provision the SDK')['run']
        self.assertIn('if [ -n "${{ matrix.hx-driver }}" ]; then\n  python ci/sdk.py install-driver '
                      '"${{ matrix.hx-driver }}" --sdk "$RUNNER_TEMP/ndi-sdk" --target "$TARGET"\nfi', provision)

    def test_each_step_promises_exactly_what_its_build_can_check(self):
        """A promise no test in the build can check fails that build; a missing one lets tests skip."""
        for row in self.primary:
            pin, target, driver = self.pins[row['package']], row['target'], self.pins.get(row.get('hx-driver'))
            for step, promise, all_features in (('all features', row['require'], True),
                                                ('no default features', row.get('require-no-default', '1'), False)):
                with self.subTest(row=row['name'], step=step):
                    # `1` is implied by every other entry, so it is spelt out only when it stands alone.
                    self.assertEqual(set(promise.split(',')) | {'1'},
                                     checkable(target, pin, all_features=all_features, driver=driver))
        command = 'cargo test --locked --config ci/offline.toml --target "$TARGET" --no-fail-fast '
        for name, flags, environment in (
            ('Tests against the real runtime, all features', '--all-features',
             {'GRAFTON_NDI_REQUIRE_RUNTIME': '${{ matrix.require }}'}),
            ('Tests against the real runtime, no default features', '--no-default-features',
             {'GRAFTON_NDI_REQUIRE_RUNTIME': "${{ matrix.require-no-default || '1' }}"}),
        ):
            step = next(step for step in self.job['steps'] if step.get('name') == name)
            self.assertEqual(step['env'], environment)
            self.assertIn(command + flags, step['run'])

    def test_a_failing_step_does_not_hide_the_steps_after_it(self):
        """Every step after the isolation check runs whenever that check passed, so one run reports everything
        that is wrong with a row; none runs when the runner was not shown to isolate."""
        names = [step.get('name') for step in self.job['steps']]
        check = names.index('Test runner leaves loopback networking only')
        self.assertEqual(self.job['steps'][check].get('id'), 'isolation')
        self.assertNotIn('if', self.job['steps'][check])
        after = self.job['steps'][check + 1:]
        self.assertGreaterEqual(len(after), 7)
        for step in after:
            with self.subTest(step=step['name']):
                self.assertTrue(step.get('if', '').startswith('${{ ' + ROW_READY),
                                f"{step['name']} runs on {step.get('if')!r}")
        self.assertEqual({step['name']: step['if'] for step in after if 'matrix.' in step['if']}, {
            'Tests against the real runtime, all features': when("matrix.role == 'primary'"),
            'Tests against the real runtime, no default features': when("matrix.role == 'primary'"),
            'Memcheck over SDK padding': when("matrix.name == 'linux-x64 standard-6.3.2'"),
            'Lint and document the Advanced implementation': when("matrix.role == 'primary' && contains(matrix.package, 'advanced')"),
            'Vendored bindings are current': when('matrix.bindings'),
            'Build for the statically linked Apple mobile targets': when('matrix.apple-mobile'),
            'Current Advanced headers with baseline runtimes': when("matrix.role == 'current-headers'"),
            'Baseline SDK builds with the current Advanced runtime': when("matrix.role == 'baseline-headers'")})

    def test_every_test_step_is_bounded_inside_the_job(self):
        """A test that never returns fails its own step, well before the job's limit, and a failing test binary
        does not keep the ones after it from running."""
        job = int(self.job['timeout-minutes'])
        steps = [step for step in self.job['steps'] if 'cargo test' in step.get('run', '')]
        self.assertGreaterEqual(len(steps), 4)
        for step in steps:
            with self.subTest(step=step['name']):
                self.assertRegex(step.get('timeout-minutes', ''), r'^\d+$')
                self.assertLess(int(step['timeout-minutes']), job)
                if 'continue-on-error' not in step:
                    self.assertIn('--no-fail-fast', step['run'])

    def test_compatibility_runs_use_independent_headers_and_runtime_identity(self):
        rows = [row for row in self.rows if 'compatibility' in row]
        self.assertEqual({self.pins[row['compatibility']]['platform'] for row in rows},
                         {'linux', 'windows', 'apple'})
        for row in rows:
            self.assertEqual(self.pins[row['compatibility']]['edition'], 'standard')
            self.assertEqual(self.pins[row['compatibility-advanced']]['edition'], 'advanced')
            self.assertEqual(self.pins[row['compatibility-advanced']]['platform'], self.pins[row['compatibility']]['platform'])
            self.assertLess(sdk.version_tuple(self.pins[row['compatibility-advanced']]['version']),
                            sdk.version_tuple(self.pins[row['package']]['version']))
            self.assertLess(sdk.version_tuple(self.pins[row['compatibility']]['version']),
                            sdk.version_tuple(self.pins[row['package']]['version']))
            self.assertEqual(self.pins[row['compatibility']]['platform'],
                             self.pins[row['package']]['platform'])
        steps = [step for step in self.job['steps'] if step.get('if') in {when("matrix.role == 'current-headers'"),
                                                 when("matrix.role == 'baseline-headers'")}]
        self.assertEqual(len(steps), 2)
        for step in steps:
            self.assertNotIn('env', step)
            self.assertIn('export GRAFTON_NDI_REQUIRE_RUNTIME=', step['run'])
            self.assertIn('cargo test --locked --config ci/offline.toml --target "$TARGET" --no-fail-fast --all-features',
                          step['run'])
            self.assertIn('ndi-compat-$edition.env', step['run'])
            self.assertIn('for edition in standard advanced; do', step['run'])
        self.assertIn('export NDI_SDK_DIR="${{ steps.sdk.outputs.NDI_SDK_DIR }}"', steps[0]['run'])
        for name, source in [('GRAFTON_NDI_PEER_RUNTIME', 'GRAFTON_NDI_EXPECTED_RUNTIME'),
                             ('GRAFTON_NDI_PEER_VERSION', 'GRAFTON_NDI_EXPECTED_VERSION'),
                             ('GRAFTON_NDI_PEER_EDITION', 'GRAFTON_NDI_EXPECTED_EDITION')]:
            self.assertIn(f'export {name}="${{{{ steps.sdk.outputs.{source} }}}}"', steps[0]['run'])
            self.assertIn(f'export {name}="${source}"', steps[1]['run'])
        for name in ('NDI_RUNTIME_DIR_V6', 'GRAFTON_NDI_EXPECTED_VERSION',
                     'GRAFTON_NDI_EXPECTED_EDITION', 'GRAFTON_NDI_EXPECTED_RUNTIME'):
            self.assertIn(f'export {name}="${{{{ steps.sdk.outputs.{name} }}}}"', steps[1]['run'])

    def test_compatibility_loops_run_the_second_edition_after_a_failure(self):
        steps = [step for step in self.job['steps'] if step.get('if') in {
            when("matrix.role == 'current-headers'"), when("matrix.role == 'baseline-headers'")}]
        for step in steps:
            # Retain the actual loop and its failure policy. Native execution,
            # or one baseline build's preparation, deterministically fails first.
            cases = ['runtime']
            if step['if'] == when("matrix.role == 'baseline-headers'"):
                cases += ['inventory', 'scope']
            for failure in cases:
                script = step['run'][step['run'].index('failed=0'):]
                lines = []
                for line in script.splitlines():
                    if not line.startswith('  '):
                        lines.append(line)
                    elif '-- "${skip_args[@]}" || failed=1' in line:
                        check = 'test "$edition" = advanced' if failure == 'runtime' else 'true'
                        lines.append(f'  echo "$edition"; {check} || failed=1')
                    elif failure == 'inventory' and '-- --list' in line:
                        lines.append(re.sub(r'cargo test .*? > "\$inventory"',
                                            'test "$edition" = advanced', line))
                    elif failure == 'scope' and 'python ci/sdk_test_scope.py' in line:
                        lines.append(re.sub(r'python ci/sdk_test_scope.py .*?(?= \|\|)',
                                            'test "$edition" = advanced', line))
                with self.subTest(step=step['name'], failure=failure):
                    result = subprocess.run(['bash', '-eo', 'pipefail', '-c', '\n'.join(lines)],
                                            text=True, capture_output=True)
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    expected = ['standard', 'advanced'] if failure == 'runtime' else ['advanced']
                    self.assertEqual(result.stdout.splitlines(), expected)

    def test_matrix_guards_reject_removed_or_ambiguous_coverage(self):
        rust = workflows.load(self.texts['rust.yml'])
        rows = rust['jobs']['sdk']['strategy']['matrix']['include']
        self.assertEqual(sdk_matrix_violations(rust), [])
        for index, row in enumerate(rows):
            with self.subTest(removed=row['name']):
                missing = copy.deepcopy(rust)
                del missing['jobs']['sdk']['strategy']['matrix']['include'][index]
                self.assertTrue(sdk_matrix_violations(missing))
        for mutation, expected in (
            ('duplicate owner', 'unique primary build owner'),
            ('missing owner', 'unique primary build owner'),
            ('unknown role', 'unknown scheduling role'),
            ('different baseline', 'independent SDK compatibility directions'),
            ('mobile check removed', 'apple-mobile configuration owners'),
            ('bindings check removed', 'bindings configuration owners'),
            ('synthetic owner omitted', 'full synthetic-header owner'),
            ('unknown primary scope', 'unaudited test scope'),
        ):
            changed = copy.deepcopy(rust)
            altered = changed['jobs']['sdk']['strategy']['matrix']['include']
            if mutation == 'duplicate owner':
                duplicate = copy.deepcopy(next(row for row in altered
                                               if row['name'] == 'linux-x64 advanced-6.4'))
                duplicate['name'] += ' duplicate'
                altered.append(duplicate)
            elif mutation == 'missing owner':
                altered.remove(next(row for row in altered if row['name'] == 'linux-x64 standard-6.1.1'))
            elif mutation == 'unknown role':
                altered[-1]['role'] = 'unknown'
            elif mutation == 'different baseline':
                altered[-1]['compatibility'] = '6.1.1-standard-linux'
            elif mutation == 'mobile check removed':
                del next(row for row in altered if row['name'] == 'macos-arm64 advanced-6.4')['apple-mobile']
            elif mutation == 'bindings check removed':
                del next(row for row in altered if row['name'] == 'linux-x64 standard-6.4')['bindings']
            elif mutation == 'synthetic owner omitted':
                next(row for row in altered if row['name'] == 'linux-x64 advanced-6.4')['test-scope'] = 'synthetic-headers'
            else:
                next(row for row in altered if row['name'] == 'linux-x64 standard-6.4')['test-scope'] = 'runtime'
            with self.subTest(mutation=mutation):
                self.assertTrue(any(expected in line for line in sdk_matrix_violations(changed)))

    def test_compatibility_selection_guards_require_inventory_and_scoped_arguments(self):
        rust = workflows.load(self.texts['rust.yml'])
        self.assertEqual(implementation_coverage_violations(rust), [])
        names = ('Current Advanced headers with baseline runtimes',
                 'Baseline SDK builds with the current Advanced runtime')
        for name in names:
            for old, new in (
                ('-- --list > "$inventory"', '> "$inventory"'),
                ('python ci/sdk_test_scope.py', 'python ci/misspelled.py'),
                ('--target "$TARGET" --headers', '--headers'),
                ('done < "$skip_file"', 'done < /dev/null'),
                ('--all-features -- "${skip_args[@]}" || failed=1', '--all-features || failed=1'),
            ):
                changed = copy.deepcopy(rust)
                step = next(step for step in changed['jobs']['sdk']['steps'] if step.get('name') == name)
                self.assertIn(old, step['run'])
                step['run'] = step['run'].replace(old, new)
                with self.subTest(step=name, removed=old):
                    self.assertTrue(implementation_coverage_violations(changed))
        changed = copy.deepcopy(rust)
        step = next(step for step in changed['jobs']['sdk']['steps']
                    if step.get('name') == 'Tests against the real runtime, all features')
        step['run'] += ' --skip compile_contracts'
        self.assertTrue(any('omits the primary' in line for line in implementation_coverage_violations(changed)))
        for old, new in (
            ('if [ "${{ matrix.test-scope }}" = synthetic-headers ]; then', 'if true; then'),
            ('--scope synthetic-headers --inventory', '--scope runtime --inventory'),
        ):
            changed = copy.deepcopy(rust)
            step = next(step for step in changed['jobs']['sdk']['steps']
                        if step.get('name') == 'Tests against the real runtime, all features')
            step['run'] = step['run'].replace(old, new)
            self.assertTrue(any('validated owner selection' in line
                                for line in implementation_coverage_violations(changed)))

    def test_each_compatibility_pass_promises_exactly_what_it_can_check(self):
        """The first step builds with the row's headers against each baseline runtime, the second with each
        baseline's headers against the row's runtime; a Standard baseline has no headers a build reads. Only
        the row's own runtime has the row's driver beside it. Neither step promises an NDI|HX decoder: the
        row's own runs do, and the decode tests still run, and pass or skip visibly, in the crossed builds."""
        baseline_runtimes, baseline_headers = [step for step in self.job['steps']
                                               if step.get('if') in {when("matrix.role == 'current-headers'"),
                                                 when("matrix.role == 'baseline-headers'")}]
        for row in (row for row in self.rows if 'compatibility' in row):
            current, target, driver = self.pins[row['package']], row['target'], self.pins.get(row.get('hx-driver'))
            baselines = {'standard': self.pins[row['compatibility']], 'advanced': self.pins[row['compatibility-advanced']]}
            with self.subTest(row=row['name'], step=baseline_runtimes['name']):
                self.assertEqual(exported_promises(baseline_runtimes, row['require']),
                                 {edition: checkable(target, pin, all_features=True, headers=current, decoders=False)
                                  for edition, pin in baselines.items()})
            with self.subTest(row=row['name'], step=baseline_headers['name']):
                self.assertEqual(exported_promises(baseline_headers, row['require']),
                                 {edition: checkable(target, current, all_features=True, headers=pin, driver=driver,
                                                     decoders=False)
                                  for edition, pin in baselines.items()})

class MemoryCheckers(unittest.TestCase):
    def test_suppressions_match_only_the_observed_glibc_loader_stack(self):
        text = (ROOT / 'ci/valgrind.supp').read_text(encoding='utf-8')
        blocks = re.findall(r'\{(.*?)\}', text, re.DOTALL)
        self.assertEqual(len(blocks), 2)
        for block, operation in zip(blocks, ('_dl_dst_count', '_dl_dst_substitute')):
            frames = re.findall(r'^\s+fun:(.+)$', block, re.MULTILINE)
            expected = ['strncmp', 'is_dst', operation]
            if operation == '_dl_dst_count':
                expected.append('expand_dynamic_string_token')
            expected += ['fillin_rpath*', 'decompose_rpath', '_dl_map_object']
            self.assertEqual(frames, expected)
            self.assertNotIn('...', block)
            self.assertNotIn('obj:', block)
            self.assertIn('Memcheck:Addr8', block)

    def test_empty_filter_and_skip_names_fail_the_checker_scripts(self):
        rust = workflows.load(read_workflows()['rust.yml'])
        cases = [
            ('miri', 'Initialized rows, metadata and synthetic native guards', 'frames::tests::video_padding_all_formats'),
            ('miri', 'Initialized rows, metadata and synthetic native guards',
             'descriptor_admission_and_collection_stay_allocation_free_under_churn'),
            ('miri', 'Initialized rows, metadata and synthetic native guards', 'sdk_copy::tests::fixture'),
            ('test', 'Memcheck over scripted capture and encoder padding', 'frames::tests::video_padding_encoders'),
            ('test', 'Manual measurement fixtures', '--ignored'),
        ]
        miri_script = rust['jobs']['miri']['steps'][2]['run']
        skips = shlex.split(re.search(r'skips=\((.*?)\)', miri_script, re.DOTALL).group(1))
        batches = miri_batches(miri_script)
        filters = [fixture for _, batch in batches for fixture in batch]
        inventory = '\n'.join(f'{name}: test' for name in [*skips, *(name + 'fixture' for name in filters)])
        with tempfile.TemporaryDirectory() as directory:
            cargo = Path(directory) / 'cargo'
            cargo.write_text('''#!/bin/bash
empty=0
for argument in "$@"; do
  if [ "$argument" = "$EMPTY_SELECTION" ]; then empty=1; fi
done
printf '%s\n' "$*" >> "$CARGO_CALLS"
case " $* " in
  *" --list "*)
    if [ -n "$MIRI_INVENTORY" ]; then
      while IFS= read -r line; do
        if [ -z "$EMPTY_SELECTION" ] || [[ "$line" != *"$EMPTY_SELECTION"* ]]; then
          printf '%s\n' "$line"
        fi
      done <<< "$MIRI_INVENTORY"
    elif [ "$empty" = 0 ]; then
      echo 'fixture: test'
    fi ;;
esac
''')
            cargo.chmod(0o755)
            for job, name, missing in cases:
                script = next(step['run'] for step in rust['jobs'][job]['steps'] if step.get('name') == name)
                script = '\n'.join(line for line in script.splitlines() if not line.startswith('sudo '))
                for selection, expected in (('', 0), (missing, 1)):
                    with self.subTest(job=job, missing=selection):
                        calls_file = Path(directory) / 'calls'
                        calls_file.write_text('')
                        result = subprocess.run(['bash', '-eo', 'pipefail', '-c', script],
                                                env={**os.environ, 'PATH': directory + os.pathsep + os.environ['PATH'],
                                                     'EMPTY_SELECTION': selection, 'CARGO_CALLS': str(calls_file),
                                                     'MIRI_INVENTORY': inventory if job == 'miri' else ''}, text=True, capture_output=True)
                        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                        if selection:
                            self.assertIn(selection.removesuffix('fixture'), result.stdout)
                            self.assertIn('No tests selected', result.stdout)
                        if job == 'miri':
                            calls = calls_file.read_text().splitlines()
                            self.assertEqual(sum(' --list' in call for call in calls), 1)
                            if not selection:
                                self.assertEqual(sum(' --list' not in call for call in calls), len(batches))
                                self.assertTrue(all('--test-threads=1' in call and ' --exact ' in call
                                                    for call in calls if ' --list' not in call))

    def test_miri_inventory_rejects_ambiguous_filters_and_skips(self):
        rust = workflows.load(read_workflows()['rust.yml'])
        script = rust['jobs']['miri']['steps'][2]['run']
        filters = [fixture for batch in MIRI_BATCHES.values() for fixture in batch]
        inventory = '\n'.join(f'{name}: test' for name in [*MIRI_SKIPS, *(name + 'fixture' for name in filters)])
        cases = (
            (inventory + '\n' + MIRI_SKIPS[0] + '_duplicate: test', 'Ambiguous skip selection:'),
            (inventory + '\n' + filters[0] + filters[1] + 'overlap: test', 'Ambiguous test selection:'),
        )
        with tempfile.TemporaryDirectory() as directory:
            cargo = Path(directory) / 'cargo'
            cargo.write_text("""#!/bin/bash
case " $* " in
  *" --list "*) printf '%s\\n' "$MIRI_INVENTORY" ;;
esac
""")
            cargo.chmod(0o755)
            for changed, expected in cases:
                with self.subTest(expected=expected):
                    result = subprocess.run(['bash', '-eo', 'pipefail', '-c', script],
                                            env={**os.environ, 'PATH': directory + os.pathsep + os.environ['PATH'],
                                                 'MIRI_INVENTORY': changed}, text=True, capture_output=True)
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertIn(expected, result.stdout)

    def test_missing_detector_configuration_is_reported(self):
        rust = workflows.load(read_workflows()['rust.yml'])
        self.assertEqual(memory_checker_violations(rust), [])
        variants = []
        missing_model = copy.deepcopy(rust)
        missing_model['jobs']['miri']['strategy']['matrix']['model'].remove('tree')
        variants.append((missing_model, 'aliasing models'))
        unpinned = copy.deepcopy(rust)
        unpinned['jobs']['miri']['steps'][1]['run'] = 'cargo +nightly miri setup'
        variants.append((unpinned, 'pin the checked nightly'))
        for job in ('test', 'sdk'):
            ineffective = copy.deepcopy(rust)
            step = next(step for step in ineffective['jobs'][job]['steps']
                        if step.get('name', '').startswith('Memcheck over '))
            step['run'] = step['run'].replace('--error-exitcode=99', '')
            variants.append((ineffective, 'fail on detector errors'))
        for job, name in (('miri', 'Initialized rows, metadata and synthetic native guards'),
                          ('test', 'Memcheck over scripted capture and encoder padding')):
            missing_guard = copy.deepcopy(rust)
            step = next(step for step in missing_guard['jobs'][job]['steps'] if step.get('name') == name)
            guard = 'select_tests "$filter" true' if job == 'miri' else '-- --list "$filter"'
            step['run'] = step['run'].replace(guard, '')
            variants.append((missing_guard, 'empty-selection guard'))
        missing_skip_guard = copy.deepcopy(rust)
        missing_skip_guard['jobs']['miri']['steps'][2]['run'] = rust['jobs']['miri']['steps'][2]['run'].replace('select_tests "$skip" false', '')
        variants.append((missing_skip_guard, 'skip-selection guard'))
        for change in ('remove', 'repeat'):
            broken_inventory = copy.deepcopy(rust)
            step = broken_inventory['jobs']['miri']['steps'][2]
            inventory = next(line for line in step['run'].splitlines() if line.startswith('inventory='))
            step['run'] = step['run'].replace(inventory, '' if change == 'remove' else inventory + '\n' + inventory)
            variants.append((broken_inventory, 'one interpreted test inventory'))
        missing_send_module = copy.deepcopy(rust)
        missing_send_module['jobs']['miri']['steps'][2]['run'] = rust['jobs']['miri']['steps'][2]['run'].replace('advanced::aliasable::tests::', '')
        variants.append((missing_send_module, 'Miri omits advanced::aliasable'))
        for fixture in ('sdk_copy::tests::', 'source_retention::tests::',
                        'finder::budget::tests::', 'file::out_param::tests::',
                        'receiver::latest_report_tests::', 'advanced::kvm::ownership_tests::',
                        'advanced::reserved_map::tests::'):
            missing_reads = copy.deepcopy(rust)
            step = missing_reads['jobs']['miri']['steps'][2]
            step['run'] = step['run'].replace(fixture + '\n', '')
            variants.append((missing_reads, f'Miri omits {fixture}'))
        for selection in ('-- --exact "${batch_tests[@]}"', '-- --list'):
            missing_file = copy.deepcopy(rust)
            step = missing_file['jobs']['miri']['steps'][2]
            step['run'] = step['run'].replace(
                f'--features advanced,file-io,image-encoding,tokio {selection}',
                f'--features advanced,image-encoding,tokio {selection}')
            variants.append((missing_file, 'synthetic fixtures with file-io'))
        for old, new, expected in (
            ('--test-threads=1', '', 'inter-test concurrency'),
            ('--exact', '', 'exact inventory selections'),
            ('skips=(', 'skips=(new_test ', 'Miri skip policy'),
            ('"$test" = "$previous"', '', 'overlapping-selection guard'),
            ('batches=(reads media ownership)', 'batches=(reads ownership)', 'Miri batch order'),
            ('              reads) filters=(', '              unknown) filters=(', 'Miri audited batches'),
        ):
            broken_batch = copy.deepcopy(rust)
            step = broken_batch['jobs']['miri']['steps'][2]
            # Parsed YAML run bodies have no workflow-level indentation.
            step['run'] = step['run'].replace(old.lstrip(), new.lstrip())
            variants.append((broken_batch, expected))
        ungated = copy.deepcopy(rust)
        ungated['jobs']['required-ci']['needs'].remove('miri')
        variants.append((ungated, 'Miri is not required'))
        for broken, expected in variants:
            with self.subTest(expected=expected):
                self.assertTrue(any(expected in error for error in memory_checker_violations(broken)))


class Repository(unittest.TestCase):
    def test_manual_measurements_have_a_bounded_ci_invocation(self):
        rust = workflows.load(read_workflows()['rust.yml'])
        step = next(step for step in rust['jobs']['test']['steps']
                    if step.get('name') == 'Manual measurement fixtures')
        self.assertEqual(step.get('if'), "runner.os == 'Linux'")
        self.assertEqual(step.get('timeout-minutes'), '5')
        self.assertEqual(step.get('env'), {
            'GRAFTON_NDI_SIZES': '64x64', 'GRAFTON_NDI_FORMATS': 'Bgra',
            'GRAFTON_NDI_CONTENTS': 'photographic', 'GRAFTON_NDI_OUTPUTS': 'jpeg85',
        })
        self.assertIn('cargo test --locked --release --all-features --lib -- --ignored --list', step['run'])
        self.assertIn("if ! grep -q ': test$'", step['run'])
        self.assertIn('exit 1', step['run'])
        self.assertIn('cargo test --locked --release --all-features --lib -- --ignored --test-threads=1', step['run'])

    def test_vendored_bindings_cover_exactly_the_supported_platform_targets(self):
        setup = (ROOT / 'docs/setup.md').read_text(encoding='utf-8')
        table = setup.split('| Platform | Rust targets | Configured coverage |', 1)[1].split('\n\n', 1)[0]
        supported = {target for row in table.splitlines() if row.startswith('|')
                     for target in re.findall(r'`([^`]+)`', row.split('|')[2])}
        xtask = (ROOT / 'xtask/src/main.rs').read_text(encoding='utf-8')
        primary = re.search(r'const PRIMARY_TARGET: &str = "([^"]+)";', xtask).group(1)
        equivalents = re.search(r'const EQUIVALENCE_TARGETS: \[&str; \d+\] = \[(.*?)\];',
                                xtask, re.DOTALL).group(1)
        checked = [primary, *re.findall(r'"([^"]+)"', equivalents)]
        self.assertEqual(len(checked), len(set(checked)), 'ABI targets must not repeat')
        self.assertEqual(set(checked), supported)

    def test_bindings_check_selects_the_visionos_capable_linux_libclang(self):
        sdk_job = workflows.load(read_workflows()['rust.yml'])['jobs']['sdk']
        install = next(step for step in sdk_job['steps'] if step.get('name') == 'Linux libclang')
        self.assertIn('libclang-19-dev', install['run'])
        self.assertIn("echo 'LIBCLANG_PATH=/usr/lib/llvm-19/lib' >> \"$GITHUB_ENV\"", install['run'])

    def test_semver_check_runs_where_no_secret_is(self):
        texts = read_workflows()
        lint = workflows.load(texts['rust.yml'])['jobs']['lint']
        self.assertIn('python ci/semver.py', [step.get('run') for step in lint['steps']])
        self.assertNotIn('semver-checks', ''.join(texts.values()))

    def test_workflows_name_only_features_the_crate_has(self):
        features = set(tomllib.loads((ROOT / 'Cargo.toml').read_text(encoding='utf-8'))['features']) - {'default'}
        named = set()
        for text in read_workflows().values():
            for match in re.findall(r'--features[ =]"?([\w,-]+)', text):
                named.update(match.split(','))
            for match in re.findall(r'for features in ([^;]+); do', text):
                named.update(part for group in match.split() for part in group.strip('"').split(',') if part)
        self.assertGreaterEqual(named, {'advanced', 'file-io'})
        self.assertLessEqual(named, features)

    def test_rustdoc_checks_every_linted_feature_set_and_leaves_the_full_build_last(self):
        texts = read_workflows()
        self.assertEqual(violations(texts), [])
        block = ('      - name: Rustdoc feature combinations\n'
                 '        env:\n          RUSTDOCFLAGS: -D warnings\n'
                 '        run: |\n'
                 '          for features in "" image-encoding tokio smol advanced file-io advanced,file-io; do\n')
        self.assertIn(block, texts['rust.yml'])
        for changed, message in ((block.replace('tokio smol', 'tokio'), 'rustdoc feature combinations'),
                                 (block.replace('-D warnings', ''), 'rustdoc denies warnings')):
            with self.subTest(message=message):
                broken = {**texts, 'rust.yml': texts['rust.yml'].replace(block, changed)}
                self.assertTrue(any(message in error for error in violations(broken)))

    def test_local_recipe_matches_lint_and_test_steps(self):
        rust = workflows.load(read_workflows()['rust.yml'])
        readme = (ROOT / 'ci' / 'README.md').read_text()
        self.assertEqual(local_recipe_violations(rust, readme), [])
        loops = re.findall(r'for features in ([^;]+); do', readme)
        self.assertEqual(len(loops), 2)
        for loop in loops:
            with self.subTest(removed_feature=loop):
                broken = readme.replace(f'for features in {loop}; do',
                                        f'for features in {loop.replace(" smol", "")}; do', 1)
                self.assertTrue(any('feature combinations' in error
                                    for error in local_recipe_violations(rust, broken)))
        commands = [line.strip() for line in readme.split('```sh\n', 1)[1].split('```', 1)[0].splitlines()
                    if line.strip().startswith(('cargo ', 'python ', 'RUSTDOCFLAGS='))]
        for command in commands:
            if command == 'cargo build --locked --examples --all-features':
                continue
            with self.subTest(removed_command=command):
                self.assertTrue(local_recipe_violations(rust, readme.replace(command, '', 1)))
        for job, body in rust['jobs'].items():
            if job not in ('lint', 'test'):
                continue
            for index, step in enumerate(body['steps']):
                if not step.get('run'):
                    continue
                broken = copy.deepcopy(rust)
                broken['jobs'][job]['steps'][index]['run'] = ''
                expected = step.get('name') in {
                    'CI scripts and workflows', 'Format', 'Clippy', 'Bindings generator tests',
                    'Rustdoc feature combinations', 'Rustdoc', 'Documentation links and examples',
                    'Tests, all features', 'Tests, default features', 'Tests, File API without Advanced wrappers',
                }
                if expected:
                    with self.subTest(removed_step=step['name']):
                        self.assertTrue(local_recipe_violations(broken, readme))
        for command in commands:
            if command.startswith('RUSTDOCFLAGS='):
                broken_readme = readme.replace(command, command.removeprefix('RUSTDOCFLAGS="-D warnings" '), 1)
                self.assertTrue(any('does not deny warnings' in error
                                    for error in local_recipe_violations(rust, broken_readme)))
        broken = copy.deepcopy(rust)
        for step in broken['jobs']['lint']['steps']:
            if step.get('name') in ('Clippy', 'Rustdoc feature combinations'):
                step['run'] = step['run'].replace(' smol', '')
        self.assertTrue(any('feature combinations' in error
                            for error in local_recipe_violations(broken, readme)))

    def test_repository_cargo_configuration_holds_only_the_xtask_alias(self):
        """Cargo obeys this file in every job that starts in the checkout; it can run programs and redirect registries."""
        directory = ROOT / '.cargo'
        self.assertEqual(sorted(path.name for path in directory.iterdir()), ['config.toml'])
        self.assertEqual(tomllib.loads((directory / 'config.toml').read_text(encoding='utf-8')),
                         {'alias': {'xtask': 'run --package xtask --'}})

    def test_code_owners_cover_what_ci_obeys(self):
        lines = (ROOT / '.github' / 'CODEOWNERS').read_text(encoding='utf-8').splitlines()
        owned = {line.split()[0]: line.split()[1:] for line in lines if line.strip() and not line.startswith('#')}
        self.assertEqual(set(owned), {'/.github/', '/ci/', '/scripts/', '/.cargo/', '/rust-toolchain.toml',
                                      '/Cargo.toml', '/Cargo.lock', '/xtask/'})
        self.assertEqual({owner for owners in owned.values() for owner in owners}, {'@GrantSparks'})


class RepeatedFeatureScopes(unittest.TestCase):
    def test_fixed_compiler_owner_and_selection_cannot_be_removed(self):
        rust = workflows.load(read_workflows()['rust.yml'])
        self.assertEqual(repeated_feature_violations(rust), [])
        for name, command, stem in REPEATED_FEATURE_TESTS:
            for operation in (*scoped_commands(command, stem), 'scope_args=()',
                              f'while IFS= read -r argument; do scope_args+=("$argument"); done < "$scope_dir/{stem}-skips.txt"'):
                with self.subTest(step=name, missing=operation):
                    broken = copy.deepcopy(rust)
                    step = next(step for step in broken['jobs']['test']['steps'] if step.get('name') == name)
                    step['run'] = step['run'].replace(operation, '')
                    self.assertTrue(repeated_feature_violations(broken))
            for setting, value in (('env', {'NDI_SDK_DIR': 'a-different-header-build'}),
                                   ('working-directory', 'another-workspace'),
                                   ('if', 'false'), ('if', False)):
                with self.subTest(step=name, changed=setting):
                    broken = copy.deepcopy(rust)
                    step = next(step for step in broken['jobs']['test']['steps'] if step.get('name') == name)
                    step[setting] = value
                    self.assertTrue(repeated_feature_violations(broken))
        for condition in (None, 'false'):
            broken = copy.deepcopy(rust)
            steps = broken['jobs']['test']['steps']
            owner = next(step for step in steps if step.get('name') == 'Tests, all features')
            if condition is None:
                owner['run'] += ' -- --skip native_loans_and_slot_keys_require_their_owner'
            else:
                owner['if'] = condition
            self.assertTrue(repeated_feature_violations(broken))
        broken = copy.deepcopy(rust)
        steps = broken['jobs']['test']['steps']
        owner = next(step for step in steps if step.get('name') == 'Tests, all features')
        steps.remove(owner)
        steps.append(owner)
        self.assertTrue(repeated_feature_violations(broken))

    def test_scope_validation_precedes_execution_and_preserves_unknown_tests(self):
        rust = workflows.load(read_workflows()['rust.yml'])
        fixed = ('attached_modes_fail_the_internal_capture_entry_points',
                 'frame_parts_require_the_matching_descriptor_validator',
                 'native_loans_and_slot_keys_require_their_owner')
        inventory = '\n'.join(f'{name}: test' for name in (
            'handles_keep_runtime_alive_until_last_drop_and_allow_reinitialization',
            *fixed, 'unknown_native_case', 'absent_feature_error_variants_are_unavailable',
            'src/lib.rs - a_doctest (line 1)',
        ))
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            cargo = directory / 'cargo'
            cargo.write_bytes(b'''#!/bin/bash
printf '%s\n' "$*" >> "$CARGO_CALLS"
case " $* " in
  *" --list "*)
    while IFS= read -r line; do
      if [ -z "$MISSING_TEST" ] || [[ "$line" != *"$MISSING_TEST"* ]]; then printf '%s\n' "$line"; fi
    done <<< "$SCOPE_INVENTORY"
    ;;
esac
''')
            cargo.chmod(0o755)
            for name, command, stem in REPEATED_FEATURE_TESTS:
                step = next(step for step in rust['jobs']['test']['steps'] if step.get('name') == name)
                for missing in ('', fixed[0]):
                    with self.subTest(step=name, missing=missing):
                        calls_file = directory / 'calls'
                        calls_file.write_text('')
                        result = subprocess.run(['bash', '-eo', 'pipefail', '-c', step['run']], cwd=ROOT,
                                                env={**os.environ, 'PATH': str(directory) + os.pathsep + os.environ['PATH'],
                                                     'RUNNER_TEMP': str(directory), 'CARGO_CALLS': str(calls_file),
                                                     'SCOPE_INVENTORY': inventory, 'MISSING_TEST': missing},
                                                text=True, capture_output=True)
                        calls = calls_file.read_text().splitlines()
                        if missing:
                            self.assertNotEqual(result.returncode, 0)
                            self.assertIn(missing, result.stderr)
                            self.assertEqual(len(calls), 1, 'a rejected scope must not execute tests')
                        else:
                            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                            self.assertEqual(len(calls), 2)
                            expected = shlex.split(command)[1:] + ['--']
                            for test in sorted(fixed):
                                expected += ['--skip', test]
                            self.assertEqual(shlex.split(calls[-1]), expected)
                            self.assertNotIn('unknown_native_case', calls[-1])
                            self.assertNotIn('absent_feature_error_variants_are_unavailable', calls[-1])


if __name__ == '__main__':
    unittest.main()
