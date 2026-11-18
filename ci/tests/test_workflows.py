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
import sdk_test_scope  # noqa: E402
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


def read_workflows():
    return {path.name: path.read_text(encoding='utf-8') for path in sorted(WORKFLOWS.glob('*.yml'))}


def cargo_manifest():
    return tomllib.loads((ROOT / 'Cargo.toml').read_text(encoding='utf-8'))


def msrv_version(manifest):
    value = manifest['package']['rust-version']
    return value + '.0' if value.count('.') == 1 else value


def workflow_obligations(manifest):
    return workflows.obligations(msrv_version(manifest), set(manifest['features']) - {'default'},
                                 sdk_test_scope.MIRI_GROUPS)


def platform_coverage(text):
    section = text.partition('| Platform | Rust targets | Configured coverage |')[2].split('\n\n', 1)[0]
    return {target: row.split('|')[3].strip() for row in section.splitlines() if row.startswith('|')
            for target in re.findall(r'`([^`]+)`', row.split('|')[2])}


def feature_loops(step):
    return [shlex.split(loop) for loop in re.findall(r'for features in ([^;]+); do', step.get('run', ''))]


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
    manifest = cargo_manifest()
    return (found + workflows.obligation_violations(parsed, workflow_obligations(manifest))
            + gate_violations(parsed['rust.yml']) + publish_violations(parsed['publish.yml'])
            + docs_violations(parsed['rust.yml']) + memory_checker_violations(parsed['rust.yml'])
            + repeated_feature_violations(parsed['rust.yml'])
            + sdk_input_violations(parsed['rust.yml'])
            + source_consistency_violations(parsed, manifest, sdk.load_pins(),
                                            (ROOT / 'docs/setup.md').read_text(encoding='utf-8')))


def source_consistency_violations(parsed, manifest, pins, setup):
    """Compare duplicated coverage with the manifest, SDK pins and platform table."""
    jobs = parsed['rust.yml']['jobs']
    lint = {step.get('name'): step for step in jobs['lint']['steps']}
    sdk_steps = {step.get('name'): step for step in jobs['sdk']['steps']}
    minimum = msrv_version(manifest)
    found = []
    for name, steps in (('Minimum supported Rust version', lint),
                        ('Lint and document the Advanced implementation', sdk_steps)):
        step = steps.get(name, {})
        if step.get('env', {}).get('RUSTFLAGS') != '-D warnings':
            found.append(f'rust.yml: {name} must deny warnings on Cargo.toml rust-version {minimum}')
    for workflow, body in parsed.items():
        for job, definition in body['jobs'].items():
            for step in definition.get('steps', []):
                versions = re.findall(r'(?:rustup toolchain install |cargo \+)(\d+\.\d+\.\d+)\b',
                                      step.get('run', ''))
                if any(version != minimum for version in versions):
                    found.append(f'{workflow}: {job} step {step.get("name")} contradicts '
                                 f'Cargo.toml rust-version {minimum}')

    expected_features = (set(manifest['features']) - {'default'}) | {'', 'advanced,file-io'}
    for name in ('Clippy', 'Rustdoc feature combinations'):
        loops = feature_loops(lint.get(name, {}))
        if len(loops) != 1 or set(loops[0]) != expected_features or len(loops[0]) != len(expected_features):
            found.append(f'rust.yml: {name} feature combinations must cover Cargo.toml features and advanced,file-io')

    coverage = platform_coverage(setup)
    dynamic = {target for target, description in coverage.items() if not description.startswith('Static build, ')}
    # GitHub's Ubuntu 24.04 x64 runner compiles its own host through the lint job.
    host = {'ubuntu-24.04': 'x86_64-unknown-linux-gnu'}.get(jobs['lint'].get('runs-on'))
    targets = jobs['cross'].get('env', {}).get('TARGETS', '').split() + [host]
    if set(targets) != dynamic or len(targets) != len(dynamic):
        found.append('rust.yml: cross TARGETS and lint host differ from the dynamic targets in docs/setup.md')
    found += sdk_version_violations(jobs['sdk']['strategy']['matrix']['include'], pins, coverage)
    return found


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

    pin = rust.get('env', {})
    if (not re.fullmatch(r'\d+\.\d+\.\d+', pin.get('WINDOWS_LLVM_VERSION', ''))
            or not re.fullmatch(r'[0-9a-f]{64}', pin.get('WINDOWS_LLVM_SHA256', ''))):
        found.append('rust.yml: Windows libclang needs a version and SHA-256 pin')
    return found


def docs_violations(docs):
    """Only the checked HTML build reaches the Pages deployment identity."""
    jobs = docs['jobs']
    deploy = jobs['deploy-docs']
    upload = next(step for step in jobs['lint']['steps'] if step.get('name') == 'Upload Pages artifact')
    steps = {step.get('id'): step for step in deploy['steps']}
    tip, deployment = steps.get('latest', {}), steps.get('deployment', {})
    return (differs('rust.yml: job order', deploy.get('needs'), 'required-ci')
            + differs('rust.yml: artifact condition', upload.get('if'), "github.event_name == 'push' && github.repository == 'GrantSparks/grafton-ndi' && github.ref == 'refs/heads/main'")
            + differs('rust.yml: artifact source', upload.get('with'), {'path': 'target/doc'})
            + differs('rust.yml: deployment environment', deploy.get('environment'),
                      {'name': 'github-pages', 'url': '${{ steps.deployment.outputs.page_url }}'})
            + differs('rust.yml: deployment scripts', [step.get('id') for step in deploy['steps'] if 'run' in step],
                      ['latest'])
            + differs('rust.yml: Pages tip check token', tip.get('env'), {'GH_TOKEN': '${{ github.token }}'})
            + differs('rust.yml: Pages deployment tip guard', deployment.get('if'), "steps.latest.outputs.deploy == 'true'")
            + rustdoc_feature_violations(jobs['lint']['steps']) + sdk_matrix_violations(docs))


def rustdoc_feature_violations(steps):
    """Every linted feature set is documented, before the published full build."""
    named = {step.get('name'): step for step in steps}
    clippy = named.get('Clippy', {})
    rustdoc = named.get('Rustdoc feature combinations', {})

    found = differs('rust.yml: rustdoc feature combinations', feature_loops(rustdoc), feature_loops(clippy))
    found += differs('rust.yml: rustdoc denies warnings', rustdoc.get('env'), {'RUSTDOCFLAGS': '-D warnings'})
    names = [step.get('name') for step in steps]
    if 'Rustdoc feature combinations' not in names or names.index('Rustdoc feature combinations') >= names.index('Rustdoc'):
        found.append('rust.yml: all-features rustdoc must be built last for checking and publication')
    return found


REPEATED_FEATURE_TESTS = ('Tests, default features', 'Tests, File API without Advanced wrappers')


def repeated_feature_violations(rust):
    """Fixed nested compiler graphs retain one earlier owner on each host."""
    steps = rust['jobs']['test']['steps']
    names = [step.get('name') for step in steps]
    owner = next((step for step in steps if step.get('name') == 'Tests, all features'), {})
    found = []
    for name in REPEATED_FEATURE_TESTS:
        step = next((step for step in steps if step.get('name') == name), {})
        for setting in ('env', 'working-directory'):
            if step.get(setting) != owner.get(setting):
                found.append(f'rust.yml: repeated feature step {name} changes fixed compiler inputs: {setting}')
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
        ('lint', 'CI scripts and workflows', ('python ',)),
        ('lint', 'Format', ('cargo ',)),
        ('lint', 'Clippy', ('cargo ',)),
        ('lint', 'Bindings generator tests', ('cargo ',)),
        ('lint', 'Rustdoc feature combinations', ('cargo ',)),
        ('lint', 'Rustdoc', ('cargo ',)),
        ('lint', 'Documentation links and examples', ('python ',)),
        ('test', 'Tests, all features', ('cargo ',)),
        *(('test', name, ('cargo ', 'python ')) for name in REPEATED_FEATURE_TESTS),
    )
    positions = []
    for job, name, prefixes in checks:
        step = next((step for step in rust['jobs'][job]['steps'] if step.get('name') == name), {})
        commands = [line for _, _, line in workflows.shell_statements(step.get('run', ''))
                    if line.startswith(prefixes)]
        if not commands:
            found.append(f'rust.yml: local recipe step {name} has no documented command')
        for command in commands:
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
        expected.extend(feature_loops(step))
    actual = [shlex.split(loop) for loop in re.findall(r'for features in ([^;]+); do', recipe)]
    found += differs('ci/README.md: local recipe feature combinations', actual, expected)
    return found


MEMCHECK_RECIPES = (
    ('test', 'Memcheck over scripted capture and encoder padding', 'SDK-free capture and encoder padding:'),
    ('sdk', 'Memcheck over SDK padding', 'Live SDK padding on Linux x64:'),
)


def memcheck_recipe_violations(rust, readme):
    found = []
    for job, name, heading in MEMCHECK_RECIPES:
        step = next((step for step in rust['jobs'][job]['steps'] if step.get('name') == name), {})
        recipe = readme.partition(heading)[2].partition('```sh\n')[2].partition('```')[0]
        actual = [shlex.split(line) for line in recipe.splitlines() if line.strip()]
        expected = []
        for line in step.get('run', '').splitlines():
            if not line or line.startswith('sudo apt-get '):
                continue
            if job == 'sdk' and line.startswith('cargo test '):
                required = step.get('env', {}).get('GRAFTON_NDI_REQUIRE_RUNTIME')
                line = f'GRAFTON_NDI_REQUIRE_RUNTIME={required} ' + line.replace('"$TARGET"', 'x86_64-unknown-linux-gnu')
            expected.append(shlex.split(line))
        if not actual or actual != expected:
            found.append(f'ci/README.md: {name} recipe differs from the workflow')
    return found


# The native obligations are independent of their scheduling roles. Keep the
# original configurations explicit so deleting a row cannot shrink coverage.
PRIMARY_SDK_CONFIGURATIONS = {
    ('x86_64-unknown-linux-gnu', '6.1.1-standard-linux'),
    ('x86_64-unknown-linux-gnu', '6.3.2-standard-linux'),
    ('x86_64-unknown-linux-gnu', '6.3.2-advanced-linux'),
    ('x86_64-unknown-linux-gnu', 'current-standard-linux'),
    ('x86_64-unknown-linux-gnu', 'current-advanced-linux'),
    ('i686-unknown-linux-gnu', 'current-standard-linux'),
    ('aarch64-unknown-linux-gnu', 'current-standard-linux'),
    ('x86_64-pc-windows-msvc', '6.3.2-standard-windows'),
    ('x86_64-pc-windows-msvc', '6.3.2-advanced-windows'),
    ('x86_64-pc-windows-msvc', 'current-standard-windows'),
    ('x86_64-pc-windows-msvc', 'current-advanced-windows'),
    ('i686-pc-windows-msvc', 'current-advanced-windows'),
    ('aarch64-apple-darwin', '6.3.2-standard-apple'),
    ('aarch64-apple-darwin', '6.3.2-advanced-apple'),
    ('aarch64-apple-darwin', 'current-standard-apple'),
    ('aarch64-apple-darwin', 'current-advanced-apple'),
    ('x86_64-apple-darwin', 'current-standard-apple'),
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
    ('x86_64-unknown-linux-gnu', '6.3.2-advanced-linux'):
        ('x86_64-unknown-linux-gnu', 'current-advanced-linux'),
    ('x86_64-pc-windows-msvc', '6.3.2-advanced-windows'):
        ('x86_64-pc-windows-msvc', 'current-advanced-windows'),
    ('i686-pc-windows-msvc', 'current-advanced-windows'):
        ('x86_64-pc-windows-msvc', 'current-advanced-windows'),
    ('aarch64-apple-darwin', '6.3.2-advanced-apple'):
        ('aarch64-apple-darwin', 'current-advanced-apple'),
}


def sdk_row_key(row):
    return row.get('target'), row.get('package')


def sdk_version_violations(rows, pins, coverage):
    """Compare SDK row and setup table versions with the installed package pins."""
    found = []
    actual = {}
    architectures = {'x86_64': 'x64', 'i686': 'x86', 'aarch64': 'arm64'}
    for row in rows:
        target, package = sdk_row_key(row)
        data = pins.get(package, {})
        edition, version = data.get('edition'), data.get('version')
        if not edition or not version or target not in sdk.RUNTIMES:
            found.append(f'rust.yml: {row.get("name")} needs a known target and package from ci/sdk-packages.json')
            continue
        platform = sdk.RUNTIMES[target][0]
        if platform != data['platform']:
            found.append(f'rust.yml: {row.get("name")} package platform differs from ci/sdk-packages.json')
        host = 'macos' if platform == 'apple' else platform
        host = f'{host}-{architectures[target.split("-")[0]]}'
        if row.get('role') == 'primary':
            actual.setdefault(target, set()).add((edition, version))
            label = '.'.join(version.split('.')[:2]) if package.startswith('current-') else version
            expected = f'{host} {edition}-{label}'
        else:
            expected = f'{host} {row.get("role")}'
        found += differs('rust.yml: SDK row name from ci/sdk-packages.json and target', row.get('name'), expected)

    documented = {}
    for target, description in coverage.items():
        if not description.startswith('Runtime tests: '):
            continue
        versions = set()
        for group in description.removeprefix('Runtime tests: ').split('; '):
            match = re.fullmatch(r'(Standard and Advanced|Standard|Advanced) (\d+(?:\.\d+)+(?:, \d+(?:\.\d+)+)*)', group)
            if not match:
                found.append(f'docs/setup.md: invalid configured runtime coverage for {target}: {group}')
                continue
            editions, releases = match.groups()
            versions.update((edition.lower(), version)
                            for edition in editions.split(' and ') for version in releases.split(', '))
        documented[target] = versions
    found += differs('docs/setup.md: configured runtime coverage from SDK rows and ci/sdk-packages.json',
                     documented, actual)
    return found


def sdk_matrix_violations(rust):
    """Every crossed build has exactly one primary compiler/configuration owner."""
    rows = rust['jobs']['sdk']['strategy']['matrix']['include']
    primary = [row for row in rows if row.get('role') == 'primary']
    actual = {sdk_row_key(row) for row in primary}
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
        ('bindings', {('x86_64-unknown-linux-gnu', 'current-standard-linux')}),
        ('apple-mobile', {('aarch64-apple-darwin', 'current-standard-apple'),
                          ('aarch64-apple-darwin', 'current-advanced-apple')}),
    ):
        found += differs(f'rust.yml: {field} configuration owners',
                         {sdk_row_key(row) for row in rows if row.get(field) == 'true'}, owners)
    found += differs('rust.yml: synthetic-header deduplication owners',
                     {sdk_row_key(row) for row in rows if row.get('test-scope') == 'synthetic-headers'},
                     set(SYNTHETIC_HEADER_OWNERS))
    for row in primary:
        if row.get('test-scope') == 'synthetic-headers':
            owners = [owner for owner in primary
                      if sdk_row_key(owner) == SYNTHETIC_HEADER_OWNERS.get(sdk_row_key(row))
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


def memory_checker_violations(rust):
    """The workflow schedules both Miri models."""
    miri = rust['jobs'].get('miri', {})
    return (differs('rust.yml: Miri aliasing models',
                    miri.get('strategy', {}).get('matrix', {}).get('model'), ['stacked', 'tree'])
            + differs('rust.yml: Miri model selection', miri.get('env', {}).get('MIRIFLAGS'),
                      "${{ matrix.model == 'tree' && '-Zmiri-tree-borrows' || '' }}"))


def gate_violations(rust):
    """required-ci waits for every check; deployment follows the gate."""
    jobs = rust['jobs']
    gate = jobs['required-ci']
    step = gate['steps'][-1]
    return (differs('rust.yml: jobs required-ci waits for', sorted(gate.get('needs') or []),
                    sorted(set(jobs) - {'required-ci', 'deploy-docs'}))
            + differs('rust.yml: the gate step', step.get('env'), {
                'RESULTS': '${{ toJSON(needs) }}', 'SDK_EXPECTED': f'${{{{ {SECRETS_AVAILABLE} }}}}'})
            + differs('rust.yml: the step given the secrets',
                      [(item.get('name'), item['env']) for item in jobs['sdk']['steps']
                       if any('secrets' in value.lower() for value in (item.get('env') or {}).values())],
                      [('Provision the SDK', PASSED_SECRETS)]))


def publish_violations(publish):
    """The job that holds the publishing identity runs nothing from the repository."""
    jobs = publish['jobs']
    job = jobs['publish']
    scripts = [step for step in job['steps'] if 'run' in step]
    found = (differs('publish.yml: job order', {name: body.get('needs') for name, body in jobs.items()}, PUBLISH_NEEDS)
             + differs('publish.yml: secrets passed to CI', jobs['ci'].get('secrets'), PASSED_SECRETS)
             + differs('publish.yml: environment of publish', job.get('environment'), 'crates-io')
             + differs('publish.yml: values publish takes from verify', job.get('env'), {
                 'VERIFIED_SHA256': '${{ needs.verify.outputs.sha256 }}',
                 'VERIFIED_TOOLCHAIN': '${{ needs.verify.outputs.toolchain }}'})
             + differs('publish.yml: where the registry token goes',
                       {place: value for place, value in workflows.entries(publish) if 'steps.auth' in value.lower()},
                       {'jobs.publish.steps[Publish].env.CARGO_REGISTRY_TOKEN': '${{ steps.auth.outputs.token }}'}))
    # Cargo reads configuration from the working directory upwards, so it must not start in the checkout.
    found += [f'publish.yml: step {step["name"]!r} of publish runs in the checkout'
              for step in scripts if step.get('working-directory') != OUTSIDE_THE_CHECKOUT]
    return found


# Plausible wrong edits: (name, workflow, text to replace, replacement, words the report must contain).
WRONG_EDITS = (
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
    ('SDK no longer waits for pictures', 'rust.yml', '    needs: hx-pictures\n', '', 'SDK picture dependency'),
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

    def test_every_decisive_command_is_protected_from_removal_extra_arguments_and_bypass(self):
        parsed = {name: workflows.load(text) for name, text in read_workflows().items()}
        table = workflow_obligations(cargo_manifest())
        self.assertEqual(workflows.obligation_violations(parsed, table), [])
        for key, obligation in table.items():
            workflow, job, name = key
            step = next(step for step in parsed[workflow]['jobs'][job]['steps']
                        if step.get('name') == name)
            for change, statement, script in workflows.command_mutations(step['run'], obligation.shell):
                with self.subTest(step=name, change=change, statement=statement):
                    report = workflows.step_violations(key, {**step, 'run': script}, obligation)
                    self.assertTrue(any(name in error for error in report), report)

    def test_every_run_step_has_one_obligation(self):
        parsed = {name: workflows.load(text) for name, text in read_workflows().items()}
        table = workflow_obligations(cargo_manifest())
        steps = parsed['rust.yml']['jobs']['cross']['steps']
        steps.append({'name': 'Unclassified', 'run': 'true'})
        steps.append(dict(steps[-2]))
        name = 'Clippy for every dynamically loaded target'
        steps.remove(next(step for step in steps if step.get('name') == name))
        report = workflows.obligation_violations(parsed, table)
        for expected in ('Unclassified: no workflow obligation', 'Link the examples for Android: repeated run step',
                         f'{name}: missing step for workflow obligation'):
            self.assertTrue(any(expected in error for error in report), report)


class ExecutionPolicies(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.jobs = workflows.load(read_workflows()['rust.yml'])['jobs']

    def run_step(self, job, name, tools, *, directory=ROOT, **extra_env):
        step = next(step for step in self.jobs[job]['steps'] if step.get('name') == name)
        return subprocess.run(['bash', '-eo', 'pipefail', '-c', tools + '\n' + step['run']],
                              cwd=directory, env={**os.environ, **extra_env}, text=True, capture_output=True)

    def test_cross_checks_later_targets_after_a_failure(self):
        tools = """rustup() { true; }
cargo() {
  while [ "$1" != --target ]; do shift; done
  printf '%s\n' "$2"
  [ "$2" != fail ]
}
"""
        result = self.run_step('cross', 'Clippy for every dynamically loaded target', tools,
                               TARGETS='first fail last')
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(result.stdout.splitlines(), ['first', 'fail', 'last', '::error::clippy failed for: fail'])

    def test_isolation_fails_if_only_the_online_check_passes(self):
        with tempfile.TemporaryDirectory() as directory:
            runner = Path(directory) / 'ci/offline/linux.sh'
            runner.parent.mkdir(parents=True)
            runner.write_text('#!/bin/bash\nprintf "isolated %s\\n" "$*"\nexit 15\n')
            runner.chmod(0o755)
            tools = 'python() { printf "online %s\\n" "$*"; }'
            result = self.run_step('sdk', 'Test runner leaves loopback networking only', tools,
                                   directory=directory, RUNNER_OS='Linux')
        self.assertEqual(result.returncode, 15, result.stdout + result.stderr)
        self.assertEqual(result.stdout.splitlines(), ['online ci/offline/check.py --online',
                                                      'isolated python3 ci/offline/check.py'])

    def test_sdk_memcheck_fails_before_running_an_empty_inventory(self):
        tools = """sudo() { true; }
cargo() {
  case " $* " in
    *" --list "*) echo '0 tests, 0 benchmarks' ;;
    *) echo 'unexpected test invocation'; exit 99 ;;
  esac
}
"""
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_step('sdk', 'Memcheck over SDK padding', tools,
                                   TARGET='x86_64-unknown-linux-gnu', RUNNER_TEMP=directory)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn("test selection group 'sdk-padding' inventory differs", result.stderr)
        self.assertNotIn('unexpected test invocation', result.stdout)

    def test_reader_refuses_yaml_it_cannot_see_through(self):
        self.assertEqual(workflows.load('a:\n  - b: c\n    d: [e, \'f\']  # g\n  - h\ni: >-\n  j\n  k\nl: |\n  m\n  # n\n'),
                         {'a': [{'b': 'c', 'd': ['e', 'f']}, 'h'], 'i': 'j k', 'l': 'm\n# n'})
        for text in ('a: &x 1\n', 'a: *x\n', 'a: {b: c}\n', 'a: !!str 1\n', 'a:\n\tb: 1\n', 'a: 1\na: 2\n',
                     'a: b\n  c\n', '---\na: 1\n', 'a:\n- b\n', 'a: "x\\ny"\n', 'a: [b, [c]]\n', 'a: b: c\n'):
            with self.subTest(text=text), self.assertRaises(workflows.Unsupported):
                workflows.load(text)


def checkable(target, pin, *, all_features, headers=None, linux_decoder=False, decoders=True, peer=None):
    """What tests can check in a build for `target` running against the package `pin`.

    `headers` is the package the build reads its headers from, `pin` itself
    unless given, and `linux_decoder` indicates the FFmpeg 7 libraries are installed.
    This restates the crate's test policy: the Discovery Server tests are Linux
    only and need a 6.4 runtime; the Advanced tests need the `advanced` feature,
    an Advanced runtime and Advanced headers; the File tests need the `file-io`
    feature and a File library the runtime can drive, which is an Advanced
    runtime everywhere except Windows, where the library is edition-neutral and
    ships for x64. The H.264 and HEVC 4:4:4 decode tests are Advanced tests that
    need an NDI|HX decoder, and the HEVC one a 6.4 runtime for its Best444
    receiver. The macOS runtime decodes both itself; on Linux the decoder uses
    FFmpeg 7 shared libraries. The Windows runtime decodes H.264
    itself, but on the hosted Windows runners it delivers its black placeholder
    frames instead of HEVC 4:4:4, so the HEVC test runs there without being
    counted.
    `decoders=False` leaves both out, as the compatibility runs do.
    `peer` supplies a second pinned runtime for their transport exchange.
    """
    platform, advanced = pin['platform'], pin['edition'] == 'advanced'
    declared = (headers or pin)['edition'] == 'advanced'
    current = sdk.version_tuple(pin['version']) >= (6, 4, 0, 0)
    decoder = decoders and (platform == 'apple' or (platform == 'linux' and linux_decoder))
    h264 = decoder or (decoders and platform == 'windows')
    found = {'1'}
    if peer is not None and peer['platform'] == platform and peer['version'] != pin['version']:
        found.add('peer')
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
             if line.lstrip().startswith('for edition in')
             or re.search(r'(?:^|[ ;])(?:export )?(?:GRAFTON_NDI_REQUIRE_RUNTIME|require)=', line)]
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

    def test_each_step_promises_exactly_what_its_build_can_check(self):
        """A promise no test in the build can check fails that build; a missing one lets tests skip."""
        for row in self.primary:
            pin, target = self.pins[row['package']], row['target']
            for step, promise, all_features in (('all features', row['require'], True),
                                                ('no default features', row.get('require-no-default', '1'), False)):
                with self.subTest(row=row['name'], step=step):
                    # `1` is implied by every other entry, so it is spelt out only when it stands alone.
                    self.assertEqual(set(promise.split(',')) | {'1'},
                                     checkable(target, pin, all_features=all_features,
                                               linux_decoder=row['package'] == 'current-advanced-linux'))
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
                                               if sdk_row_key(row) == ('x86_64-unknown-linux-gnu', 'current-advanced-linux')))
                duplicate['name'] += ' duplicate'
                altered.append(duplicate)
            elif mutation == 'missing owner':
                altered.remove(next(row for row in altered if sdk_row_key(row) == ('x86_64-unknown-linux-gnu', '6.1.1-standard-linux')))
            elif mutation == 'unknown role':
                altered[-1]['role'] = 'unknown'
            elif mutation == 'different baseline':
                altered[-1]['compatibility'] = '6.1.1-standard-linux'
            elif mutation == 'mobile check removed':
                del next(row for row in altered if sdk_row_key(row) == ('aarch64-apple-darwin', 'current-advanced-apple'))['apple-mobile']
            elif mutation == 'bindings check removed':
                del next(row for row in altered if sdk_row_key(row) == ('x86_64-unknown-linux-gnu', 'current-standard-linux'))['bindings']
            elif mutation == 'synthetic owner omitted':
                next(row for row in altered if sdk_row_key(row) == ('x86_64-unknown-linux-gnu', 'current-advanced-linux'))['test-scope'] = 'synthetic-headers'
            else:
                next(row for row in altered if sdk_row_key(row) == ('x86_64-unknown-linux-gnu', 'current-standard-linux'))['test-scope'] = 'runtime'
            with self.subTest(mutation=mutation):
                self.assertTrue(any(expected in line for line in sdk_matrix_violations(changed)))

    def test_each_compatibility_pass_promises_exactly_what_it_can_check(self):
        """The first step builds with the row's headers against each baseline runtime, the second with each
        baseline's headers against the row's runtime; a Standard baseline has no headers a build reads.
        The decode tests still run and pass or skip visibly in the crossed builds."""
        baseline_runtimes, baseline_headers = [step for step in self.job['steps']
                                               if step.get('if') in {when("matrix.role == 'current-headers'"),
                                                 when("matrix.role == 'baseline-headers'")}]
        for row in (row for row in self.rows if 'compatibility' in row):
            current, target = self.pins[row['package']], row['target']
            baselines = {'standard': self.pins[row['compatibility']], 'advanced': self.pins[row['compatibility-advanced']]}
            with self.subTest(row=row['name'], step=baseline_runtimes['name']):
                self.assertEqual(exported_promises(baseline_runtimes, row['require']),
                                 {edition: checkable(target, pin, all_features=True, headers=current, decoders=False, peer=current)
                                  for edition, pin in baselines.items()})
            with self.subTest(row=row['name'], step=baseline_headers['name']):
                self.assertEqual(exported_promises(baseline_headers, row['require']),
                                 {edition: checkable(target, current, all_features=True, headers=pin,
                                                     decoders=False, peer=pin)
                                  for edition, pin in baselines.items()})

class MemoryCheckers(unittest.TestCase):
    def test_memcheck_recipes_match_the_workflow(self):
        rust = workflows.load(read_workflows()['rust.yml'])
        readme = (ROOT / 'ci/README.md').read_text(encoding='utf-8')
        self.assertEqual(memcheck_recipe_violations(rust, readme), [])
        for job, step_name, missing in (
            ('test', 'Memcheck over scripted capture and encoder padding', '--test mixed_capture_boundary'),
            ('test', 'Memcheck over scripted capture and encoder padding', '--test-threads=1'),
            ('sdk', 'Memcheck over SDK padding', '--track-origins=yes'),
            ('sdk', 'Memcheck over SDK padding', '--no-fail-fast'),
        ):
            with self.subTest(job=job, missing=missing):
                altered = copy.deepcopy(rust)
                step = next(step for step in altered['jobs'][job]['steps'] if step.get('name') == step_name)
                step['run'] = step['run'].replace(missing, '')
                self.assertTrue(any(step_name in error for error in memcheck_recipe_violations(altered, readme)))
                parts = readme.split('### Memory checkers', 1)
                broken_docs = '### Memory checkers'.join((parts[0], parts[1].replace(missing, '')))
                self.assertTrue(any(step_name in error for error in memcheck_recipe_violations(rust, broken_docs)))

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

    def test_missing_aliasing_model_is_reported(self):
        rust = workflows.load(read_workflows()['rust.yml'])
        self.assertEqual(memory_checker_violations(rust), [])
        rust['jobs']['miri']['strategy']['matrix']['model'].remove('tree')
        self.assertTrue(any('aliasing models' in error for error in memory_checker_violations(rust)))


class Repository(unittest.TestCase):
    def test_pages_deploys_only_the_current_tip(self):
        rust = workflows.load(read_workflows()['rust.yml'])
        tip, deployment = rust['jobs']['deploy-docs']['steps']
        self.assertEqual(deployment.get('if'), "steps.latest.outputs.deploy == 'true'")
        script = 'gh() { printf "%s\\n" "$REMOTE_TIP"; }\n' + tip['run']
        sha = 'a' * 40
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'output'
            for remote, status, expected in ((sha, 0, 'deploy=true\n'), ('b' * 40, 0, ''), ('', 1, '')):
                with self.subTest(remote=remote):
                    output.write_text('')
                    result = subprocess.run(['bash', '-eo', 'pipefail', '-c', script], text=True, capture_output=True,
                                            env={**os.environ, 'GITHUB_REPOSITORY': 'owner/repo', 'GITHUB_SHA': sha,
                                                 'GITHUB_OUTPUT': str(output), 'REMOTE_TIP': remote})
                    self.assertEqual(result.returncode, status, result.stdout + result.stderr)
                    self.assertEqual(output.read_text(), expected)
                    if remote and remote != sha:
                        self.assertIn('::notice::Skipping Pages deployment', result.stdout)

    def test_ci_detects_drift_from_manifest_packages_and_platform_table(self):
        parsed = {name: workflows.load(text) for name, text in read_workflows().items()}
        manifest = cargo_manifest()
        pins = sdk.load_pins()
        setup = (ROOT / 'docs/setup.md').read_text(encoding='utf-8')

        def violations_for(workflow=parsed, cargo=manifest, packages=pins, platforms=setup):
            return source_consistency_violations(workflow, cargo, packages, platforms)

        self.assertEqual(violations_for(), [])
        for job, step_name in (('lint', 'Minimum supported Rust version'),
                               ('sdk', 'Lint and document the Advanced implementation')):
            broken = copy.deepcopy(parsed)
            step = next(step for step in broken['rust.yml']['jobs'][job]['steps']
                        if step.get('name') == step_name)
            del step['env']['RUSTFLAGS']
            self.assertTrue(any(step_name in error and 'deny warnings' in error
                                for error in violations_for(workflow=broken)))
        min_version = msrv_version(manifest)
        major, minor, _ = min_version.split('.')
        next_version = f'{major}.{int(minor) + 1}'
        new_manifest = copy.deepcopy(manifest)
        new_manifest['package']['rust-version'] = next_version
        self.assertTrue(any('Cargo.toml rust-version' in error for error in violations_for(cargo=new_manifest)))

        changed_msrv = copy.deepcopy(parsed)
        lint, sdk_job = (changed_msrv['rust.yml']['jobs'][job] for job in ('lint', 'sdk'))
        lint_step = next(step for step in lint['steps'] if step.get('name') == 'Minimum supported Rust version')
        advanced_step = next(step for step in sdk_job['steps']
                             if step.get('name') == 'Lint and document the Advanced implementation')
        lint_step['run'] = lint_step['run'].replace(min_version, next_version + '.0')
        self.assertTrue(any('Cargo.toml rust-version' in error for error in violations_for(workflow=changed_msrv)))
        advanced_step['run'] = advanced_step['run'].replace(min_version, next_version + '.0')
        self.assertEqual(violations_for(workflow=changed_msrv, cargo=new_manifest), [])
        self.assertEqual(workflows.obligation_violations(changed_msrv, workflow_obligations(new_manifest)), [])

        new_feature = copy.deepcopy(manifest)
        new_feature['features']['metrics'] = []
        errors = violations_for(cargo=new_feature)
        self.assertEqual(sum('Cargo.toml features' in error for error in errors), 2)

        cross = copy.deepcopy(parsed)
        cross_env = cross['rust.yml']['jobs']['cross']['env']
        cross_env['TARGETS'] = cross_env['TARGETS'].replace('armv7-unknown-linux-gnueabihf ', '')
        self.assertTrue(any('dynamic targets in docs/setup.md' in error for error in violations_for(workflow=cross)))

        new_pin = copy.deepcopy(pins)
        new_pin['current-standard-linux']['version'] = '6.5.0.1'
        self.assertEqual(sum('row name from ci/sdk-packages.json' in error
                             for error in violations_for(packages=new_pin)), 3)
        self.assertTrue(any('configured runtime coverage' in error
                            for error in violations_for(platforms=setup.replace('6.1.1', '6.0.0'))))

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
    def test_vendored_bindings_cover_exactly_the_supported_platform_targets(self):
        setup = (ROOT / 'docs/setup.md').read_text(encoding='utf-8')
        supported = set(platform_coverage(setup))
        xtask = (ROOT / 'xtask/src/main.rs').read_text(encoding='utf-8')
        primary = re.search(r'const PRIMARY_TARGET: &str = "([^"]+)";', xtask).group(1)
        equivalents = re.search(r'const EQUIVALENCE_TARGETS: \[&str; \d+\] = \[(.*?)\];',
                                xtask, re.DOTALL).group(1)
        checked = [primary, *re.findall(r'"([^"]+)"', equivalents)]
        self.assertEqual(len(checked), len(set(checked)), 'ABI targets must not repeat')
        self.assertEqual(set(checked), supported)

    def test_workflows_name_only_features_the_crate_has(self):
        features = set(cargo_manifest()['features']) - {'default'}
        named = set()
        for text in read_workflows().values():
            for match in re.findall(r'--features[ =]"?([\w,-]+)', text):
                named.update(match.split(','))
        self.assertLessEqual(named, features)

    def test_rustdoc_checks_every_linted_feature_set_and_leaves_the_full_build_last(self):
        texts = read_workflows()
        self.assertEqual(violations(texts), [])
        block = ('      - name: Rustdoc feature combinations\n'
                 '        env:\n          RUSTDOCFLAGS: -D warnings\n'
                 '        run: |\n')
        self.assertIn(block, texts['rust.yml'])
        changed = texts['rust.yml'].replace(block, block.replace('-D warnings', ''))
        self.assertTrue(any('rustdoc denies warnings' in error
                            for error in violations({**texts, 'rust.yml': changed})))

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
                         {'alias': {'xtask': 'run --locked --package xtask --'}})

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
        for name in REPEATED_FEATURE_TESTS:
            for setting, value in (('env', {'NDI_SDK_DIR': 'a-different-header-build'}),
                                   ('working-directory', 'another-workspace')):
                with self.subTest(step=name, changed=setting):
                    broken = copy.deepcopy(rust)
                    step = next(step for step in broken['jobs']['test']['steps'] if step.get('name') == name)
                    step[setting] = value
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
            for name in REPEATED_FEATURE_TESTS:
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
                            expected = shlex.split(calls[0])[:shlex.split(calls[0]).index('--') + 1]
                            for test in sorted(fixed):
                                expected += ['--skip', test]
                            self.assertEqual(shlex.split(calls[-1]), expected)
                            self.assertNotIn('unknown_native_case', calls[-1])
                            self.assertNotIn('absent_feature_error_variants_are_unavailable', calls[-1])


if __name__ == '__main__':
    unittest.main()
