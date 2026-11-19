"""Read the workflow files for the tests that guard them.

The guards need the structure of a workflow, and the standard library has no YAML
parser. `load` reads the subset of YAML these workflows use: block mappings and
sequences, plain and quoted scalars, flow sequences of scalars, literal and folded
block scalars, and comments. Anything else (anchors, aliases, tags, flow mappings,
multi-line plain scalars, duplicate keys, tabs) raises `Unsupported`, so a construct
the guards cannot see through fails the tests instead of slipping past them.
Whether the files are valid workflows is checked by actionlint in the same job.
"""
from dataclasses import dataclass, replace
import re
import shlex

KEY = re.compile(r'([A-Za-z_][\w-]*):(?: +(.*))?')
SINGLE_QUOTED = re.compile(r"'((?:[^']|'')*)'")
DOUBLE_QUOTED = re.compile(r'"([^"\\]*)"')
COMMENT = re.compile(r'(?:^|\s+)#.*$')


class Unsupported(ValueError):
    """The text uses YAML that `load` does not read."""


def load(text):
    """Parse a workflow into dictionaries, lists and strings. Every scalar stays a string."""
    return _Reader(text).document()


class _Reader:
    def __init__(self, text):
        if '\t' in text:
            raise Unsupported('a tab character')
        self.lines = text.split('\n')
        self.position = 0

    def fail(self, reason):
        raise Unsupported(f'line {self.position + 1}: {reason}')

    def peek(self):
        """Indentation and content of the next line that holds anything, or None at the end."""
        while self.position < len(self.lines):
            line = self.lines[self.position]
            content = line.strip()
            if content and not content.startswith('#'):
                return len(line) - len(line.lstrip(' ')), content
            self.position += 1
        return None

    def document(self):
        value = self.block(0)
        if self.peek() is not None:
            self.fail('content after the document')
        return value

    def block(self, indent):
        """The mapping or sequence whose entries all start at `indent`."""
        line = self.peek()
        if line is None or line[0] != indent:
            self.fail('unexpected indentation')
        return self.sequence(indent) if line[1].startswith('- ') else self.mapping(indent)

    def mapping(self, indent):
        result = {}
        while (line := self.peek()) is not None and line[0] == indent:
            match = KEY.fullmatch(line[1])
            if not match:
                self.fail('not a `key: value` entry')
            key, rest = match.group(1), COMMENT.sub('', match.group(2) or '') if match.group(2) else ''
            if key in result:
                self.fail(f'duplicate key {key}')
            self.position += 1
            result[key] = self.value(match.group(2) or '', rest, indent)
        if line is not None and line[0] > indent:
            self.fail('unexpected indentation')
        return result

    def sequence(self, indent):
        result = []
        while (line := self.peek()) is not None and line[0] == indent and line[1].startswith('- '):
            content = line[1][2:]
            if content != content.lstrip():
                self.fail('more than one space after a dash')
            if KEY.fullmatch(content):
                # The item is a mapping whose first key stands where the dash was.
                self.lines[self.position] = ' ' * (indent + 2) + content
                result.append(self.mapping(indent + 2))
            else:
                self.position += 1
                result.append(self.scalar(content))
        if line is not None and line[0] >= indent:
            self.fail('unexpected line in a sequence')
        return result

    def value(self, raw, rest, indent):
        """What follows `key:` on a line indented by `indent`."""
        if not rest:
            following = self.peek()
            return self.block(following[0]) if following is not None and following[0] > indent else None
        if rest[0] in '|>':
            return self.block_scalar(rest, indent)
        return self.scalar(raw)

    def block_scalar(self, header, indent):
        if not re.fullmatch(r'[|>][+-]?', header):
            self.fail('unsupported block scalar header')
        lines = []
        while self.position < len(self.lines):
            line = self.lines[self.position]
            if line.strip() and len(line) - len(line.lstrip(' ')) <= indent:
                break
            lines.append(line)
            self.position += 1
        while lines and not lines[-1].strip():
            lines.pop()
        margin = min((len(line) - len(line.lstrip(' ')) for line in lines if line.strip()), default=0)
        body = [line[margin:] for line in lines]
        return '\n'.join(body) if header[0] == '|' else ' '.join(line.strip() for line in body if line.strip())

    def scalar(self, text):
        """A plain or quoted scalar, or a flow sequence of them, with any trailing comment removed."""
        for pattern, unescape in ((SINGLE_QUOTED, lambda value: value.replace("''", "'")), (DOUBLE_QUOTED, str)):
            match = pattern.match(text)
            if match:
                if COMMENT.sub('', text[match.end():]):
                    self.fail('text after a quoted scalar')
                return unescape(match.group(1))
        text = COMMENT.sub('', text)
        if text.startswith('['):
            if not text.endswith(']') or any(character in text[1:-1] for character in '[]{}'):
                self.fail('unsupported flow sequence')
            return [self.scalar(item.strip()) for item in text[1:-1].split(',') if item.strip()]
        if not text or text[0] in '{}]&*!%@`|>?"\'' or text.startswith('- ') or ': ' in text or text.endswith(':'):
            self.fail(f'unsupported scalar {text!r}')
        return text


def entries(node, path=''):
    """Every key and scalar of a parsed workflow, each with a readable location.

    A key is reported at `<location of its mapping>.<key> (key)`. Items of a
    sequence of mappings are named by their `name`, or else by the action they use.
    """
    if isinstance(node, dict):
        for key, value in node.items():
            location = f'{path}.{key}' if path else key
            yield f'{location} (key)', key
            yield from entries(value, location)
    elif isinstance(node, list):
        for index, item in enumerate(node):
            label = (item.get('name') or item.get('uses', '').split('@')[0] or index) if isinstance(item, dict) else index
            yield from entries(item, f'{path}[{label}]')
    elif node is not None:
        yield path, node


@dataclass(frozen=True)
class Obligation:
    """Ordered commands, control flow, failure policy and test prerequisites."""

    script: str
    failure: str = 'immediate'
    selection: str = 'none'
    runtime: str | None = None
    condition: str | None = None
    shell: str | None = None


def shell_statements(script):
    """Yield (start, end, text) for each statement in the supported scripts.

    The steps use one statement per logical line. Join explicit continuations
    before comparing and keep compound statements with their guards.
    """
    parts = []
    start = 0
    for index, source in enumerate(script.splitlines()):
        line = source.strip()
        if not parts and (not line or line.startswith('#')):
            continue
        if not parts:
            start = index
        parts.append(line.removesuffix('\\'))
        if not line.endswith('\\'):
            yield start, index + 1, ' '.join(parts)
            parts.clear()
    if parts:
        raise ValueError('unterminated shell line continuation')


def argument_vectors(script):
    """Compare arguments after tokenizing quotes, pipes and redirect operators."""
    result = []
    for _, _, statement in shell_statements(script):
        lexer = shlex.shlex(statement, posix=True, punctuation_chars='();<>|&')
        lexer.whitespace_split = True
        lexer.commenters = ''
        # Keep escapes whose meaning depends on the shell context, as in Bash
        # regular expressions. Explicit continuations were already removed.
        lexer.escape = ''
        tokens = tuple(lexer)
        if tokens[:3] == ('for', 'features', 'in') and tokens[-2:] == (';', 'do'):
            tokens = (*tokens[:3], *sorted(tokens[3:-2]), *tokens[-2:])
        result.append(tokens)
    return result


def substitute(script, replacements):
    for marker, value in replacements.items():
        script = script.replace(marker, value)
    return script


def step_violations(key, step, obligation):
    name = ' / '.join(key)
    problems = []
    if step.get('if') != obligation.condition:
        problems.append(f'{name}: scheduling condition no longer matches the obligation')
    if step.get('shell') != obligation.shell:
        problems.append(f'{name}: execution shell no longer matches the obligation')
    runtime = (step.get('env') or {}).get('GRAFTON_NDI_REQUIRE_RUNTIME')
    expected_runtime = None if obligation.runtime == 'peer exported per edition' else obligation.runtime
    if runtime != expected_runtime:
        problems.append(f'{name}: runtime requirement differs from the obligation: {expected_runtime!r}')
    expected = argument_vectors(obligation.script)
    try:
        actual = argument_vectors(step.get('run') or '')
    except ValueError as error:
        return problems + [f'{name}: cannot tokenize decisive commands: {error}']
    for number, (command, required) in enumerate(zip(actual, expected), 1):
        if command != required:
            problems.append(f'{name}: command {number} is {command!r}, expected {required!r} '
                            f'({obligation.failure} failure, {obligation.selection} selection)')
            break
    if len(actual) != len(expected):
        problems.append(f'{name}: {len(actual)} statements, expected {len(expected)}')
    return problems


def obligation_violations(parsed, table):
    found = []
    seen = set()
    for workflow, definition in parsed.items():
        if definition.get('defaults') != {'run': {'shell': 'bash'}}:
            found.append(f'{workflow}: steps require Bash with fail-fast and pipefail')
        for job, body in definition['jobs'].items():
            if 'defaults' in body:
                found.append(f'{workflow} / {job}: job defaults override the workflow shell')
            if ('GRAFTON_NDI_REQUIRE_RUNTIME' in (definition.get('env') or {})
                    or 'GRAFTON_NDI_REQUIRE_RUNTIME' in (body.get('env') or {})):
                found.append(f'{workflow} / {job}: runtime promises belong on individual steps')
            scheduled = []
            for step in body.get('steps', []):
                if 'run' not in step:
                    continue
                key = workflow, job, step.get('name') or '(unnamed run step)'
                name = ' / '.join(key)
                if key in seen:
                    found.append(f'{name}: repeated run step')
                seen.add(key)
                obligation = table.get(key)
                if obligation is None:
                    found.append(f'{name}: no workflow obligation')
                else:
                    scheduled.append(key)
                    found.extend(step_violations(key, step, obligation))
            if scheduled != [key for key in table if key[:2] == (workflow, job) and key in seen]:
                found.append(f'{workflow} / {job}: run steps appear out of their obligated order')
    found.extend(f'{" / ".join(key)}: missing step for workflow obligation' for key in sorted(table.keys() - seen))
    return found


def command_mutations(script, shell=None):
    """Remove, change arguments, and disable each guarded statement."""
    lines = script.splitlines()
    for start, stop, statement in shell_statements(script):
        # Closing delimiters and here-document bodies are compared too, but
        # are not commands and are not subject to argument mutation.
        if statement.startswith(('<', '}')) or statement in {'done', 'else', 'esac', 'fi', 'HTML'}:
            continue
        if shell == 'pwsh':
            disabled = 'if ($false) { ' + statement + ' }'
        elif statement.startswith('if '):
            disabled = 'if false; then'
        else:
            disabled = 'if false; then ' + statement + '; fi'
        for change, replacement in (('remove', []),
                                    ('append', [statement + ' --unexpected-argument']),
                                    ('disable', [disabled])):
            yield change, statement, '\n'.join([*lines[:start], *replacement, *lines[stop:]])

# Steps are reviewed as ordered argv and control statements. Keep command
# literals here so changed arguments or disabled error checks fail together.
def obligations(msrv, features, miri_groups):
    table = {}
    # receiver-diagnostics.yml, probe
    table['receiver-diagnostics.yml', 'probe', 'Select the diagnostic target'] = Obligation(
        r'''
        case "$CONFIGURATION" in
          receiver-diagnostic-macos)
            target=aarch64-apple-darwin
            package=6.3.2-standard-apple
            require=1
            ;;
          receiver-diagnostic-linux-x86)
            target=i686-unknown-linux-gnu
            package=current-standard-linux
            require=1,discovery-server
            ;;
          receiver-diagnostic-linux-x64)
            target=x86_64-unknown-linux-gnu
            package=current-standard-linux
            require=1,discovery-server
            ;;
          receiver-diagnostic-linux-arm64)
            target=aarch64-unknown-linux-gnu
            package=current-standard-linux
            require=1,discovery-server
            ;;
          *) exit 1 ;;
        esac
        echo "target=$target" >> "$GITHUB_OUTPUT"
        {
          echo "TARGET=$target"
          echo "PACKAGE=$package"
          echo "GRAFTON_NDI_REQUIRE_RUNTIME=$require"
        } >> "$GITHUB_ENV"
        ''',
    )
    table['receiver-diagnostics.yml', 'probe', 'Linux diagnostic prerequisites'] = Obligation(
        r'''
        ci/ubuntu-primary-archive.sh
        if [ "$TARGET" = i686-unknown-linux-gnu ]; then
          sudo dpkg --add-architecture i386
        fi
        sudo apt-get update
        sudo apt-get install --yes --no-install-recommends bubblewrap gdb libavahi-client3
        if [ "$TARGET" = i686-unknown-linux-gnu ]; then
          sudo apt-get install --yes --no-install-recommends gcc-multilib libc6-dev:i386 libavahi-client3:i386
        fi
        if [ -e /proc/sys/kernel/apparmor_restrict_unprivileged_userns ]; then
          sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
        fi
        ''',
        condition="runner.os == 'Linux'",
    )
    table['receiver-diagnostics.yml', 'probe', 'Provision the diagnostic SDK'] = Obligation(
        'python ci/sdk.py install "$PACKAGE" --dest "$RUNNER_TEMP/ndi-sdk" --target "$TARGET" --github-env "$GITHUB_ENV"',
    )
    table['receiver-diagnostics.yml', 'probe', 'Verify loopback isolation'] = Obligation(
        r'''
        python ci/offline/check.py --online
        case "$RUNNER_OS" in
          Linux) ci/offline/linux.sh python3 ci/offline/check.py ;;
          macOS) ci/offline/macos.sh python3 ci/offline/check.py ;;
          *) exit 1 ;;
        esac
        ''',
    )
    table['receiver-diagnostics.yml', 'probe', 'Check stack capture on a deliberate stall'] = Obligation(
        r'''
        case "$RUNNER_OS" in
          Linux) ci/offline/linux.sh python3 ci/receiver_watchdog.py --check-debugger ;;
          macOS) ci/offline/macos.sh python3 ci/receiver_watchdog.py --check-debugger ;;
          *) exit 1 ;;
        esac
        ''',
    )
    table['receiver-diagnostics.yml', 'probe', 'Repeat the reconnect case'] = Obligation(
        r'''
        for attempt in 1 2 3 4 5; do
          echo "Receiver diagnostic attempt $attempt"
          cargo test --locked --config ci/offline.toml --target "$TARGET" --all-features --lib -- --exact receiver::tests::reconnect_returns_while_a_capture_attempt_is_in_flight
        done
        ''',
        condition="${{ !cancelled() && steps.isolation.outcome == 'success' }}",
    )
    table['receiver-diagnostics.yml', 'probe', 'Repeat receiver overlap cases'] = Obligation(
        r'''
        for attempt in 1 2 3; do
          echo "Receiver diagnostic attempt $attempt"
          cargo test --locked --config ci/offline.toml --target "$TARGET" --all-features --test receiver_connect_overlap_sdk
        done
        ''',
        condition="${{ !cancelled() && steps.isolation.outcome == 'success' }}",
    )

    table['rust.yml', 'verified-tree', 'Verify the completed pull request run'] = Obligation(
        'python3 ci/verified_tree.py reuse',
    )

    # rust.yml, lint
    table['rust.yml', 'lint', 'CI scripts and workflows'] = Obligation(
        r'''
        python -m unittest discover -s ci/tests
        shellcheck ci/*.sh ci/offline/*.sh scripts/*.sh
        curl --fail --silent --show-error --location --output "$RUNNER_TEMP/actionlint.tar.gz" \
          "https://github.com/rhysd/actionlint/releases/download/v${ACTIONLINT_VERSION}/actionlint_${ACTIONLINT_VERSION}_linux_amd64.tar.gz"
        echo "${ACTIONLINT_SHA256}  $RUNNER_TEMP/actionlint.tar.gz" | sha256sum --check --quiet
        tar -xzf "$RUNNER_TEMP/actionlint.tar.gz" -C "$RUNNER_TEMP" actionlint
        "$RUNNER_TEMP/actionlint"
        ''',
    )
    table['rust.yml', 'lint', 'Format'] = Obligation(
        'cargo fmt --all -- --check',
    )
    table['rust.yml', 'lint', 'Clippy'] = Obligation(
        r'''
        cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
        for features in @FEATURES@; do
          cargo clippy --locked --all-targets --no-default-features ${features:+--features "$features"} -- -D warnings
        done
        ''',
    )
    table['rust.yml', 'lint', 'Bindings generator tests'] = Obligation(
        'cargo test --locked --package xtask',
    )
    table['rust.yml', 'lint', 'Draft Linux regression tests'] = Obligation(
        'cargo test --locked --lib --all-features',
        condition="github.event_name == 'pull_request' && github.event.pull_request.draft",
    )
    table['rust.yml', 'lint', 'Rustdoc feature combinations'] = Obligation(
        r'''
        for features in @FEATURES@; do
          cargo doc --locked --no-deps --no-default-features ${features:+--features "$features"}
        done
        ''',
    )
    table['rust.yml', 'lint', 'Rustdoc'] = Obligation(
        'cargo doc --locked --no-deps --all-features',
    )
    table['rust.yml', 'lint', 'Documentation links and examples'] = Obligation(
        'python ci/docs.py --compile --rustdoc target/doc',
    )
    table['rust.yml', 'lint', 'Redirect the site root to the API reference'] = Obligation(
        r'''
        cat > target/doc/index.html <<'HTML'
        <!doctype html>
        <html lang="en">
        <meta charset="utf-8">
        <title>grafton-ndi API reference</title>
        <meta http-equiv="refresh" content="0; url=grafton_ndi/">
        <a href="grafton_ndi/">grafton-ndi API reference</a>
        </html>
        HTML
        ''',
        condition="github.event_name == 'push' && github.repository == 'GrantSparks/grafton-ndi' && github.ref == 'refs/heads/main'",
    )
    table['rust.yml', 'lint', 'Minimum supported Rust version'] = Obligation(
        r'''
        rustup toolchain install @MSRV@ --profile minimal
        cargo +@MSRV@ check --locked --workspace --all-targets --all-features
        ''',
    )
    table['rust.yml', 'lint', 'Packaged crate builds'] = Obligation(
        r'''
        files=$(cargo package --list --locked --package grafton-ndi)
        if grep -E '_tests\.rs$|/tests\.rs$|test_support|test_alloc|/mock\.rs$|/sdk_tests/' <<< "$files"; then
          echo '::error::The package contains test-only sources'
          exit 1
        fi
        cargo package --locked --package grafton-ndi
        ''',
    )
    table['rust.yml', 'lint', 'Semver check against the last release'] = Obligation(
        'python ci/semver.py',
    )
    table['rust.yml', 'lint', 'Dependency advisories'] = Obligation(
        r'''
        cargo install cargo-audit --version 0.23.0 --locked
        cargo audit --deny warnings
        ''',
    )

    # rust.yml, test
    table['rust.yml', 'test', 'Tests, all features'] = Obligation(
        'cargo test --locked --no-fail-fast --all-features',
    )
    table['rust.yml', 'test', 'Tests, default features'] = Obligation(
        r'''
        scope_dir="${RUNNER_TEMP:-target/ci}"
        mkdir -p "$scope_dir"
        cargo test --locked --no-fail-fast -- --list > "$scope_dir/default-tests.txt"
        python ci/sdk_test_scope.py --scope fixed-graph --inventory "$scope_dir/default-tests.txt" --output "$scope_dir/default-skips.txt"
        scope_args=()
        while IFS= read -r argument; do scope_args+=("$argument"); done < "$scope_dir/default-skips.txt"
        cargo test --locked --no-fail-fast -- "${scope_args[@]}"
        ''',
        selection='validated-skips',
    )
    table['rust.yml', 'test', 'Tests, File API without Advanced wrappers'] = Obligation(
        r'''
        scope_dir="${RUNNER_TEMP:-target/ci}"
        mkdir -p "$scope_dir"
        cargo test --locked --no-fail-fast --no-default-features --features file-io -- --list > "$scope_dir/file-tests.txt"
        python ci/sdk_test_scope.py --scope fixed-graph --inventory "$scope_dir/file-tests.txt" --output "$scope_dir/file-skips.txt"
        scope_args=()
        while IFS= read -r argument; do scope_args+=("$argument"); done < "$scope_dir/file-skips.txt"
        cargo test --locked --no-fail-fast --no-default-features --features file-io -- "${scope_args[@]}"
        ''',
        selection='validated-skips',
    )
    table['rust.yml', 'test', 'Manual measurement fixtures'] = Obligation(
        r'''
        inventory="$RUNNER_TEMP/ndi-measurement-tests.txt"
        selection="$RUNNER_TEMP/ndi-measurement-selected.txt"
        cargo test --locked --no-fail-fast --release --all-features --lib -- --ignored --list > "$inventory"
        python3 ci/sdk_test_scope.py select --group measurements --inventory "$inventory" --output "$selection"
        mapfile -t tests < "$selection"
        cargo test --locked --no-fail-fast --release --all-features --lib -- --ignored --exact "${tests[@]}" --test-threads=1
        ''',
        selection='nonempty',
        condition="runner.os == 'Linux'",
    )
    table['rust.yml', 'test', 'Memcheck over scripted capture and encoder padding'] = Obligation(
        r'''
        sudo apt-get update
        sudo apt-get install --yes --no-install-recommends valgrind
        runner='target.x86_64-unknown-linux-gnu.runner=["valgrind","--tool=memcheck","--error-exitcode=99","--trace-children=yes","--trace-children-skip=*/rustc,*/rustc-*"]'
        cargo test --locked --no-fail-fast --all-features --config "$runner" --test mixed_capture_boundary --test frame_metadata_boundary -- --test-threads=1
        memcheck_dir="${RUNNER_TEMP:-target/ci}"
        mkdir -p "$memcheck_dir"
        inventory="$memcheck_dir/ndi-encoder-padding-tests.txt"
        selection="$memcheck_dir/ndi-encoder-padding-selected.txt"
        cargo test --locked --no-fail-fast --all-features --lib -- --list > "$inventory"
        python3 ci/sdk_test_scope.py select --group encoder-padding --inventory "$inventory" --output "$selection"
        mapfile -t tests < "$selection"
        cargo test --locked --no-fail-fast --all-features --config "$runner" --lib -- --exact "${tests[@]}" --test-threads=1
        ''',
        selection='nonempty',
        condition="runner.os == 'Linux'",
    )

    # rust.yml, miri
    table['rust.yml', 'miri', 'Pinned Miri toolchain'] = Obligation(
        r'''
        rustup toolchain install nightly-2026-11-10 --profile minimal --component miri,rust-src
        cargo +nightly-2026-11-10 miri setup
        ''',
    )
    table['rust.yml', 'miri', 'Initialized rows, metadata and synthetic native guards'] = Obligation(
        r'''
        inventory="$RUNNER_TEMP/ndi-miri-tests.txt"
        selection="$RUNNER_TEMP/ndi-miri-selected.txt"
        cargo +nightly-2026-11-10 miri test --locked --lib --features advanced,file-io,image-encoding,tokio -- --list > "$inventory"
        for group in @MIRI_GROUPS@; do
          python3 ci/sdk_test_scope.py select --group "$group" --inventory "$inventory" --output "$selection"
          mapfile -t tests < "$selection"
          cargo +nightly-2026-11-10 miri test --locked --lib --features advanced,file-io,image-encoding,tokio -- --exact "${tests[@]}" --test-threads=1
        done
        ''',
        selection='nonempty',
    )

    # rust.yml, cross
    table['rust.yml', 'cross', 'Clippy for every dynamically loaded target'] = Obligation(
        r'''
        read -r -a targets <<< "$TARGETS"
        rustup target add "${targets[@]}"
        failed=()
        for target in "${targets[@]}"; do
          cargo clippy --locked --all-targets --all-features --target "$target" -- -D warnings || failed+=("$target")
        done
        if [ "${#failed[@]}" -ne 0 ]; then
          echo "::error::clippy failed for: ${failed[*]}"
          exit 1
        fi
        ''',
        failure='accumulate',
    )
    table['rust.yml', 'cross', 'Link the examples for Android'] = Obligation(
        'ANDROID_NDK_HOME="$ANDROID_NDK_LATEST_HOME" scripts/build-android.sh aarch64-linux-android',
    )

    # rust.yml, hx-pictures
    table['rust.yml', 'hx-pictures', 'Generate test pictures'] = Obligation(
        r'''
        ci/ubuntu-primary-archive.sh
        sudo apt-get update
        sudo apt-get install --yes --no-install-recommends ffmpeg
        python3 tests/support/generate_hx.py --output-dir "$RUNNER_TEMP/ndi-hx-pictures"
        ''',
    )

    # rust.yml, sdk
    table['rust.yml', 'sdk', 'Linux prerequisites'] = Obligation(
        r'''
        if [ "$SDK_MATRIX_NAME" = 'linux-x64 standard-6.3.2' ] || [ "$GRAFTON_NDI_RECEIVER_DIAGNOSTICS" = 1 ]; then
          ci/ubuntu-primary-archive.sh
        fi
        sudo apt-get update
        sudo apt-get install --yes --no-install-recommends bubblewrap libavahi-client3
        if [ "$GRAFTON_NDI_RECEIVER_DIAGNOSTICS" = 1 ]; then
          sudo apt-get install --yes --no-install-recommends gdb
        fi
        if [ -e /proc/sys/kernel/apparmor_restrict_unprivileged_userns ]; then
          sudo sysctl -w kernel.apparmor_restrict_unprivileged_userns=0
        fi
        ''',
        condition="runner.os == 'Linux'",
    )
    table['rust.yml', 'sdk', 'Linux libclang'] = Obligation(
        r'''
        sudo apt-get install --yes --no-install-recommends libclang-19-dev
        echo 'LIBCLANG_PATH=/usr/lib/llvm-19/lib' >> "$GITHUB_ENV"
        ''',
        condition="runner.os == 'Linux' && (contains(matrix.package, 'advanced') || matrix.bindings)",
    )
    table['rust.yml', 'sdk', '32-bit Linux linker and runtime dependencies'] = Obligation(
        r'''
        sudo dpkg --add-architecture i386
        sudo apt-get update
        sudo apt-get install --yes --no-install-recommends gcc-multilib libc6-dev:i386 libavahi-client3:i386
        ''',
        condition="matrix.target == 'i686-unknown-linux-gnu'",
    )
    table['rust.yml', 'sdk', 'Windows libclang'] = Obligation(
        r'''
        $version = $env:WINDOWS_LLVM_VERSION
        $installer = Join-Path $env:RUNNER_TEMP "LLVM-$version-win64.exe"
        $directory = Join-Path $env:RUNNER_TEMP "llvm-$version"
        Invoke-WebRequest "https://github.com/llvm/llvm-project/releases/download/llvmorg-$version/LLVM-$version-win64.exe" -OutFile $installer
        $digest = (Get-FileHash -Algorithm SHA256 -LiteralPath $installer).Hash
        if ($digest -ne $env:WINDOWS_LLVM_SHA256) { throw "LLVM $version SHA-256 mismatch: $digest" }
        Write-Host "Verified LLVM $version SHA-256: $digest"
        $process = Start-Process -Wait -PassThru -FilePath $installer -ArgumentList "/S /D=$directory"
        if ($process.ExitCode -ne 0) { throw "LLVM installer exited $($process.ExitCode)" }
        $env:LIBCLANG_PATH = Join-Path $directory 'bin'
        $clang = Join-Path $env:LIBCLANG_PATH 'clang.exe'
        $resource = & $clang -print-resource-dir
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        if (!(Test-Path -LiteralPath (Join-Path $resource 'include/stdbool.h') -PathType Leaf)) { throw "LLVM builtin headers are missing from $resource" }
        python ci/libclang_version.py
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
        $bindgenArgs = 'BINDGEN_EXTRA_CLANG_ARGS="-resource-dir={0}"' -f $resource.Replace('\', '/')
        Write-Host "LLVM resource directory: $resource"
        "LIBCLANG_PATH=$env:LIBCLANG_PATH" | Out-File $env:GITHUB_ENV -Append -Encoding utf8
        "CLANG_PATH=$clang" | Out-File $env:GITHUB_ENV -Append -Encoding utf8
        $bindgenArgs | Out-File $env:GITHUB_ENV -Append -Encoding utf8
        ''',
        condition="runner.os == 'Windows' && contains(matrix.package, 'advanced')",
        shell='pwsh',
    )
    table['rust.yml', 'sdk', 'Use the same test pictures in every Advanced row'] = Obligation(
        r'''
        pictures="$RUNNER_TEMP/ndi-hx-pictures"
        if [ "$RUNNER_OS" = macOS ]; then
          shasum -a 256 "$pictures/quadrants-420.h264" "$pictures/chroma-444.hevc"
        else
          sha256sum "$pictures/quadrants-420.h264" "$pictures/chroma-444.hevc"
        fi
        echo "GRAFTON_NDI_HX_PICTURES=$pictures" >> "$GITHUB_ENV"
        ''',
        condition="contains(matrix.package, 'advanced')",
    )
    table['rust.yml', 'sdk', 'Provision the SDK'] = Obligation(
        r'''
        python ci/sdk.py install "$PACKAGE" --dest "$RUNNER_TEMP/ndi-sdk" --target "$TARGET" --github-env "$GITHUB_ENV" --github-output "$GITHUB_OUTPUT"
        if [ -n "${{ matrix.hx-driver }}" ]; then
          python ci/sdk.py install-driver "${{ matrix.hx-driver }}" --sdk "$RUNNER_TEMP/ndi-sdk" --target "$TARGET"
        fi
        if [ "${{ matrix.role }}" != primary ]; then
          python ci/sdk.py install "${{ matrix.compatibility }}" --dest "$RUNNER_TEMP/ndi-compat" --target "$TARGET" --github-env "$RUNNER_TEMP/ndi-compat-standard.env"
          python ci/sdk.py install "${{ matrix.compatibility-advanced }}" --dest "$RUNNER_TEMP/ndi-compat-advanced" --target "$TARGET" --github-env "$RUNNER_TEMP/ndi-compat-advanced.env"
        fi
        ''',
    )
    table['rust.yml', 'sdk', 'Test runner leaves loopback networking only'] = Obligation(
        r'''
        case "$RUNNER_OS" in
          Linux) python ci/offline/check.py --online && ci/offline/linux.sh python3 ci/offline/check.py ;;
          macOS) python ci/offline/check.py --online && ci/offline/macos.sh python3 ci/offline/check.py ;;
          Windows) echo 'Windows runs the runtime tests without a loopback-only runner.' ;;
        esac
        ''',
    )
    table['rust.yml', 'sdk', 'Tests against the real runtime, all features'] = Obligation(
        r'''
        skip_args=()
        if [ "${{ matrix.test-scope }}" = synthetic-headers ]; then
          inventory="$RUNNER_TEMP/ndi-primary-tests.txt"
          skip_file="$RUNNER_TEMP/ndi-primary-skips.txt"
          cargo test --locked --config ci/offline.toml --target "$TARGET" --no-fail-fast --all-features -- --list > "$inventory"
          python ci/sdk_test_scope.py --scope synthetic-headers --inventory "$inventory" --output "$skip_file"
          while IFS= read -r argument; do skip_args+=("$argument"); done < "$skip_file"
        fi
        cargo test --locked --config ci/offline.toml --target "$TARGET" --no-fail-fast --all-features -- "${skip_args[@]}"
        ''',
        selection='validated-skips',
        runtime='${{ matrix.require }}',
        condition="${{ !cancelled() && steps.isolation.outcome == 'success' && matrix.role == 'primary' }}",
    )
    table['rust.yml', 'sdk', 'Tests against the real runtime, no default features'] = Obligation(
        'cargo test --locked --config ci/offline.toml --target "$TARGET" --no-fail-fast --no-default-features',
        runtime="${{ matrix.require-no-default || '1' }}",
        condition="${{ !cancelled() && steps.isolation.outcome == 'success' && matrix.role == 'primary' }}",
    )
    table['rust.yml', 'sdk', 'Memcheck over SDK padding'] = Obligation(
        r'''
        sudo apt-get install --yes --no-install-recommends valgrind
        memcheck_dir="${RUNNER_TEMP:-target/ci}"
        mkdir -p "$memcheck_dir"
        inventory="$memcheck_dir/ndi-sdk-padding-tests.txt"
        selection="$memcheck_dir/ndi-sdk-padding-selected.txt"
        cargo test --locked --config ci/offline.toml --target "$TARGET" --no-fail-fast --features image-encoding --test video_padding_sdk -- --list > "$inventory"
        python3 ci/sdk_test_scope.py select --group sdk-padding --inventory "$inventory" --output "$selection"
        mapfile -t tests < "$selection"
        cargo test --locked --config ci/offline.toml --config 'target.x86_64-unknown-linux-gnu.runner=["ci/offline/linux.sh","valgrind","--tool=memcheck","--track-origins=yes","--error-exitcode=99"]' --target "$TARGET" --no-fail-fast --features image-encoding --test video_padding_sdk -- --exact "${tests[@]}" --test-threads=1
        ''',
        selection='nonempty',
        runtime='1',
        condition="${{ !cancelled() && steps.isolation.outcome == 'success' && matrix.name == 'linux-x64 standard-6.3.2' }}",
    )
    table['rust.yml', 'sdk', 'Lint and document the Advanced implementation'] = Obligation(
        r'''
        rustup toolchain install @MSRV@ --profile minimal --target "$TARGET"
        cargo +@MSRV@ check --locked --all-targets --target "$TARGET" --all-features
        cargo clippy --locked --all-targets --target "$TARGET" --all-features -- -D warnings
        cargo clippy --locked --all-targets --target "$TARGET" --no-default-features --features advanced -- -D warnings
        cargo clippy --locked --all-targets --target "$TARGET" --no-default-features --features advanced,file-io -- -D warnings
        cargo doc --locked --no-deps --target "$TARGET" --all-features
        ''',
        condition="${{ !cancelled() && steps.isolation.outcome == 'success' && matrix.role == 'primary' && contains(matrix.package, 'advanced') }}",
    )
    table['rust.yml', 'sdk', 'Vendored bindings are current'] = Obligation(
        'cargo xtask bindgen --check --sdk "$NDI_SDK_DIR"',
        condition="${{ !cancelled() && steps.isolation.outcome == 'success' && matrix.bindings }}",
    )
    table['rust.yml', 'sdk', 'Build for the statically linked Apple mobile targets'] = Obligation(
        'ci/apple-mobile.sh',
        condition="${{ !cancelled() && steps.isolation.outcome == 'success' && matrix.apple-mobile }}",
    )
    table['rust.yml', 'sdk', 'Current Advanced headers with baseline runtimes'] = Obligation(
        r'''
        export GRAFTON_NDI_PEER_RUNTIME="${{ steps.sdk.outputs.GRAFTON_NDI_EXPECTED_RUNTIME }}"
        export GRAFTON_NDI_PEER_VERSION="${{ steps.sdk.outputs.GRAFTON_NDI_EXPECTED_VERSION }}"
        export GRAFTON_NDI_PEER_EDITION="${{ steps.sdk.outputs.GRAFTON_NDI_EXPECTED_EDITION }}"
        inventory="$RUNNER_TEMP/ndi-current-headers-tests.txt"
        skip_file="$RUNNER_TEMP/ndi-current-headers-skips.txt"
        cargo test --locked --config ci/offline.toml --target "$TARGET" --no-fail-fast --all-features -- --list > "$inventory"
        python ci/sdk_test_scope.py --inventory "$inventory" --target "$TARGET" --headers advanced --require-peer --output "$skip_file"
        skip_args=()
        while IFS= read -r argument; do skip_args+=("$argument"); done < "$skip_file"
        failed=0
        for edition in standard advanced; do
          while IFS='=' read -r name value; do export "$name=$value"; done < "$RUNNER_TEMP/ndi-compat-$edition.env"
          export NDI_SDK_DIR="${{ steps.sdk.outputs.NDI_SDK_DIR }}"
          if [ "$edition" = advanced ]; then export GRAFTON_NDI_REQUIRE_RUNTIME=advanced,peer; else export GRAFTON_NDI_REQUIRE_RUNTIME=1,peer; fi
          cargo test --locked --config ci/offline.toml --target "$TARGET" --no-fail-fast --all-features -- "${skip_args[@]}" || failed=1
        done
        exit "$failed"
        ''',
        failure='accumulate',
        selection='validated-skips',
        runtime='peer exported per edition',
        condition="${{ !cancelled() && steps.isolation.outcome == 'success' && matrix.role == 'current-headers' }}",
    )
    table['rust.yml', 'sdk', 'Baseline SDK builds with the current Advanced runtime'] = Obligation(
        r'''
        failed=0
        for edition in standard advanced; do
          while IFS='=' read -r name value; do export "$name=$value"; done < "$RUNNER_TEMP/ndi-compat-$edition.env"
          export GRAFTON_NDI_PEER_RUNTIME="$GRAFTON_NDI_EXPECTED_RUNTIME"
          export GRAFTON_NDI_PEER_VERSION="$GRAFTON_NDI_EXPECTED_VERSION"
          export GRAFTON_NDI_PEER_EDITION="$GRAFTON_NDI_EXPECTED_EDITION"
          export NDI_RUNTIME_DIR_V6="${{ steps.sdk.outputs.NDI_RUNTIME_DIR_V6 }}"
          export GRAFTON_NDI_EXPECTED_RUNTIME="${{ steps.sdk.outputs.GRAFTON_NDI_EXPECTED_RUNTIME }}"
          export GRAFTON_NDI_EXPECTED_VERSION="${{ steps.sdk.outputs.GRAFTON_NDI_EXPECTED_VERSION }}"
          export GRAFTON_NDI_EXPECTED_EDITION="${{ steps.sdk.outputs.GRAFTON_NDI_EXPECTED_EDITION }}"
          require="${{ matrix.require }}"
          require="${require/,h264-decoder/}"; require="${require/,hevc-444-decoder/}"
          if [ "$edition" = standard ]; then require="${require/advanced/1}"; fi
          export GRAFTON_NDI_REQUIRE_RUNTIME="$require,peer"
          inventory="$RUNNER_TEMP/ndi-baseline-$edition-tests.txt"
          skip_file="$RUNNER_TEMP/ndi-baseline-$edition-skips.txt"
          cargo test --locked --config ci/offline.toml --target "$TARGET" --no-fail-fast --all-features -- --list > "$inventory" || { failed=1; continue; }
          python ci/sdk_test_scope.py --inventory "$inventory" --target "$TARGET" --headers "$edition" --require-peer --output "$skip_file" || { failed=1; continue; }
          skip_args=()
          while IFS= read -r argument; do skip_args+=("$argument"); done < "$skip_file"
          cargo test --locked --config ci/offline.toml --target "$TARGET" --no-fail-fast --all-features -- "${skip_args[@]}" || failed=1
        done
        exit "$failed"
        ''',
        failure='accumulate',
        selection='validated-skips',
        runtime='peer exported per edition',
        condition="${{ !cancelled() && steps.isolation.outcome == 'success' && matrix.role == 'baseline-headers' }}",
    )

    # rust.yml, required-ci
    table['rust.yml', 'required-ci', 'Every job this event requires succeeded'] = Obligation(
        'python3 ci/gate.py',
    )
    table['rust.yml', 'required-ci', 'Record the fully tested pull request tree'] = Obligation(
        'python3 ci/verified_tree.py record "$RUNNER_TEMP/ci-tested-tree.json"',
        condition="github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && needs.sdk.result == 'success'",
    )

    # rust.yml, deploy-docs
    table['rust.yml', 'deploy-docs', 'Confirm this commit is still the tip of main'] = Obligation(
        r'''
        tip=$(gh api "repos/$GITHUB_REPOSITORY/git/ref/heads/main" --jq '.object.sha')
        if [[ ! "$tip" =~ ^[0-9a-f]{40}$ ]]; then
          echo '::error::Could not determine the current tip of main'
          exit 1
        fi
        if [ "$tip" != "$GITHUB_SHA" ]; then
          echo "::notice::Skipping Pages deployment for $GITHUB_SHA because main is now $tip"
          exit 0
        fi
        echo 'deploy=true' >> "$GITHUB_OUTPUT"
        ''',
    )

    # publish.yml, release
    table['publish.yml', 'release', 'Validate the tag and look up full CI for its tree'] = Obligation(
        'python3 ci/release.py --prepare',
    )

    # publish.yml, verify
    table['publish.yml', 'verify', 'Package and build the packaged crate'] = Obligation(
        r'''
        cargo package --locked --package grafton-ndi
        crate="target/package/grafton-ndi-${RELEASE_VERSION}.crate"
        {
          echo "sha256=$(sha256sum "$crate" | cut -d' ' -f1)"
          echo "toolchain=$(rustc --version | cut -d' ' -f2)"
        } >> "$GITHUB_OUTPUT"
        mkdir "$RUNNER_TEMP/release"
        cp "$crate" "$RUNNER_TEMP/release/"
        ''',
    )

    # publish.yml, publish
    table['publish.yml', 'publish', 'Re-create the verified package'] = Obligation(
        r'''
        if ! [[ "$VERIFIED_SHA256" =~ ^[0-9a-f]{64}$ && "$VERIFIED_TOOLCHAIN" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
          echo "::error::the verify job did not report a SHA-256 and a toolchain version"
          exit 1
        fi
        rustup toolchain install "$VERIFIED_TOOLCHAIN" --profile minimal
        export RUSTUP_TOOLCHAIN="$VERIFIED_TOOLCHAIN" CARGO_TARGET_DIR="$RUNNER_TEMP/target"
        {
          echo "RUSTUP_TOOLCHAIN=$RUSTUP_TOOLCHAIN"
          echo "CARGO_TARGET_DIR=$CARGO_TARGET_DIR"
        } >> "$GITHUB_ENV"
        cargo package --locked --no-verify --manifest-path "$GITHUB_WORKSPACE/Cargo.toml" --package grafton-ndi
        actual=$(sha256sum "$CARGO_TARGET_DIR/package/grafton-ndi-${RELEASE_VERSION}.crate" | cut -d' ' -f1)
        if [ "$actual" != "$VERIFIED_SHA256" ]; then
          echo "::error::this package is $actual, the verified package is $VERIFIED_SHA256"
          exit 1
        fi
        ''',
    )
    table['publish.yml', 'publish', 'Publish'] = Obligation(
        'cargo publish --locked --no-verify --manifest-path "$GITHUB_WORKSPACE/Cargo.toml" --package grafton-ndi',
    )

    replacements = {'@MSRV@': msrv, '@FEATURES@': shlex.join(('', *sorted(features), 'advanced,file-io')),
                    '@MIRI_GROUPS@': shlex.join(miri_groups)}
    return {key: replace(entry, script=substitute(entry.script, replacements)) for key, entry in table.items()}
