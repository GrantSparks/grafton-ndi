"""Bounded public failure evidence without copying arbitrary compiler/test output."""
from __future__ import annotations
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import traceback
from sandbox_probe import validate_report as validate_sandbox_report

MAX_INPUT_BYTES = 2 * 1024 * 1024
MAX_OUTPUT_BYTES = 4096
MAX_ITEMS = 8
SOURCE_SUFFIXES = {'.rs', '.py', '.c', '.h'}
ABI_STAGES = frozenset({'configuration', 'c-compile', 'rust-compile',
    'dependency-inventory', 'object-identity', 'section-extraction',
    'comparison', 'record-identity'})
MOBILE_SMOKE_STATES = frozenset({'result-missing', 'native-running', 'native-error',
    'native-panic', 'result-invalid', 'result-write-failed', 'app-exited', 'launch-failed'})
MOBILE_SMOKE_PHASES = frozenset({'boot', 'install', 'launch', 'result', 'terminate'})
MACOS_CONFINEMENT_PHASES = frozenset({'setup', 'barrier', 'worker', 'cleanup', 'recovery'})
# These are public system-library names in the reviewed native linker contract.
# Arbitrary linker operands, SDK library names and paths remain private.
SYSTEM_LIBRARY_REASONS = {
    'c++': 'link-system-library-missing-cxx',
    'objc': 'link-system-library-missing-objc',
    'z': 'link-system-library-missing-z',
    'dns_sd': 'link-system-library-missing-dns-sd',
    'iconv': 'link-system-library-missing-iconv',
    'System': 'link-system-library-missing-system',
    'c': 'link-system-library-missing-c',
    'm': 'link-system-library-missing-m',
}
REASONS = (
    ('missing-name', r'cannot find (?:type|function|value|trait|macro|module)|unresolved import|no method named'),
    ('type-mismatch', r'mismatched types|type mismatch|trait bound .* not satisfied|incompatible types'),
    ('target-standard-library-missing', r"can.t find crate for .(?:std|core|compiler_builtins)"),
    ('crate-dependency-missing', r"can.t find crate for"),
    ('compiler-macro-redefined', r'macro redefined|macro-redefined'),
    ('linker-failed', r'linking with .* failed|linker command failed|link.exe.*failed|ld returned \d+ exit status'),
    ('undefined-link-symbol', r'undefined reference|undefined symbols|symbol\(s\) not found|unresolved external symbol'),
    ('link-library-missing', r'cannot find -l|library not found for -l|library [\'\"].*?[\'\"] not found|cannot open input file'),
    ('link-framework-missing', r'framework not found|framework [\'\"].*?[\'\"] not found'),
    ('incompatible-native-platform', r'building for .*but (?:attempting to link|linking in)|incompatible platform|platform .* does not match'),
    ('incompatible-native-architecture', r'incompatible architecture|incompatible with .* output|architecture of input file .* incompatible|does not contain.*architecture|file was built for .*which is not the architecture|machine type .*conflicts with target machine'),
    ('duplicate-link-symbol', r'duplicate symbols?|multiple definition of|already defined in'),
    ('header-not-found', r'fatal error: .* (?:file not found|No such file or directory)'),
    ('static-library-missing', r'could not find native static library'),
    ('unsupported-native-archive', r'unsupported archive identifier|malformed archive|archive .* (?:malformed|unsupported)|not an archive'),
    ('invalid-native-object', r'malformed (?:object|mach-o)|unsupported (?:object|file) (?:format|type)|(?:object|file) format not recognized|unknown file type|file is not a valid (?:object|mach-o)'),
    ('object-inspection-tool-missing', r'LLVM readobj/objcopy are required'),
    ('object-inspection-tool-failed', r'llvm-(?:readobj|objcopy)(?:-\d+)?(?:\.exe)? failed|option not supported by llvm-objcopy'),
    ('unsupported-compiler-option', r'unknown argument|unrecognized command.line option|unsupported option'),
    ('target-unavailable', r'unsupported target|target .* not (?:installed|supported)|could not find specification for target'),
    ('rust-toolchain-unavailable', r'no release found for|toolchain .* does not exist|component .* unavailable'),
    ('compiler-failed', r'could not compile|compilation terminated|aborting due to .* error'),
    ('tests-failed', r'test result: FAILED|test failed|native test failed'),
    ('abi-mismatch', r'ABI .*mismatch|ABI comparison failed|layout .*mismatch'),
    ('permission-denied', r'permission denied|operation not permitted|access is denied'),
    ('disk-full', r'no space left on device|not enough space on the disk'),
    ('file-or-command-missing', r'no such file or directory|command not found|not recognized as .* command|system cannot find the (?:file|path) specified'),
    ('timed-out', r'timed out|TimeoutExpired'),
    ('receiver-connection-timed-out', r'^initial receiver connections? timed out$'),
    ('receiver-ptz-capability-timed-out', r'^PTZ capability enable timed out$'),
    ('receiver-web-control-capability-timed-out', r'^web-control capability update timed out$'),
    ('receiver-status-notification-timed-out', r'^control status notification timed out$'),
    ('local-source-discovery-timed-out', r'^local sender endpoint not discovered:'),
    ('process-crashed', r'segmentation fault|SIGSEGV|access violation'),
    ('download-failed', r'download failed|could not download|HTTP Error|SDK acquisition (?:failed|retry budget exhausted)'),
    ('certificate-failed', r'certificate verify failed|SSL certificate problem'),
    ('identity-mismatch', r'SHA256 mismatch|checksum.*(?:failed|mismatch)|differs from.*(?:identity|plan|package)'),
    ('runtime-identity-mismatch', r'Runtime(?:Edition|Identity|Version)Mismatch|runtime identity.*(?:mismatch|differs)'),
    ('runtime-load-failed', r'\bRuntimeLoadFailed\b|failed to load the NDI runtime'),
    ('runtime-initialization-failed', r'\bInitializationFailed\b|failed to initialize the NDI runtime'),
    ('runtime-initialize-rejected', r'\bNDIlib_initialize failed\b'),
    ('sender-create-rejected', r'\bFailed to create NDI send instance\b'),
    ('native-operation-failed', r'\bNativeOperationFailed\b|NDI SDK rejected'),
    ('native-null-pointer', r'\bNullPointer\b|encountered a null pointer in function'),
    ('capability-unavailable', r'UnsupportedCapability|missing required.*symbol'),
)


class PublicSources:
    """Only tracked source paths/identifiers can appear in exported diagnostics."""
    def __init__(self, root):
        self.root = Path(root).resolve()
        self.paths = set()
        self.identifiers = set()
        self.functions = set()
        listing = subprocess.check_output(['git', '-C', str(self.root), 'ls-files', '-z'])
        budget = 8 * MAX_INPUT_BYTES
        for raw in listing.decode('utf-8', errors='replace').split('\0'):
            path = PurePosixPath(raw)
            if path.is_absolute() or '..' in path.parts or path.suffix not in SOURCE_SUFFIXES:
                continue
            local = self.root.joinpath(*path.parts)
            if not local.is_file() or not local.resolve().is_relative_to(self.root):
                continue
            self.paths.add(path.as_posix())
            if path.suffix == '.rs' and 0 < local.stat().st_size <= min(MAX_INPUT_BYTES, budget):
                content = local.read_text(encoding='utf-8', errors='replace')
                budget -= local.stat().st_size
                self.identifiers.update(re.findall(r'\b[A-Za-z_][A-Za-z_0-9]*\b', content))
                self.functions.update(re.findall(r'\bfn\s+([A-Za-z_][A-Za-z_0-9]*)\s*[(<]', content))

    def location(self, filename, line, column=None):
        if not isinstance(filename, str) or len(filename) > 2048:
            return None
        normalized = filename.replace('\\', '/')
        root = self.root.as_posix()
        if normalized.startswith(root + '/'):
            normalized = normalized[len(root) + 1:]
        if normalized.startswith('./'):
            normalized = normalized[2:]
        if normalized not in self.paths or type(line) is not int or not 0 < line < 10_000_000:
            return None
        result = {'file': normalized, 'line': line}
        if type(column) is int and 0 < column < 100_000:
            result['column'] = column
        return result

    def test(self, value):
        if len(value) > 200 or not re.fullmatch(r'[A-Za-z_][A-Za-z_0-9]*(?:::[A-Za-z_][A-Za-z_0-9]*)*', value):
            return None
        parts = value.split('::')
        return value if parts[-1] in self.functions and all(p in self.identifiers for p in parts) else None


def summarize_text(text, sources, *, truncated=False):
    codes, reasons, locations, tests = set(), set(), {}, set()
    native_context = set()
    sandbox = None
    panic_message = False
    # Never export a matched message, diagnostic label, source line or child text.
    # Matching only selects fixed public categories and independently checked paths.
    def inspect(message):
        for reason, pattern in REASONS:
            if re.search(pattern, message, re.IGNORECASE):
                reasons.add(reason)
        for name, reason in SYSTEM_LIBRARY_REASONS.items():
            library = re.escape(name)
            pattern = (r'\blibrary [\'\"]' + library + r'[\'\"] not found'
                       r'|\b(?:library not found for|cannot find) -l' + library + r'(?=\s|$|[:,;])')
            if re.search(pattern, message, re.IGNORECASE):
                reasons.add(reason)

    def add_location(value):
        if value:
            locations[(value['file'], value['line'], value.get('column', 0))] = value

    for line in text.splitlines():
        if line.startswith('NATIVE_LOG_WRITE_FAILURE=') and len(line) <= 512:
            try:
                failure = json.loads(line[len('NATIVE_LOG_WRITE_FAILURE='):])
                if (not isinstance(failure, dict) or set(failure) != {'test', 'kind'}
                        or failure.get('kind') not in ('io', 'encoding', 'other')
                        or not isinstance(failure.get('test'), str)):
                    raise ValueError('invalid native harness failure')
                name = sources.test(failure['test'])
                if name:
                    native_context.add(('log-write', name, failure['kind']))
                    reasons.add('native-log-write-failed')
            except (ValueError, TypeError):
                pass
        confinement = re.fullmatch(
            r'(?:(?:[A-Za-z_]\w*\.)*ConfinementError: )?MACOS_CONFINEMENT_FAILURE=([a-z-]+)', line)
        if confinement and confinement[1] in MACOS_CONFINEMENT_PHASES:
            reasons.add('macos-confinement-' + confinement[1])
        if line.startswith('SANDBOX_PROBE=') and sandbox is None:
            try:
                sandbox = validate_sandbox_report(json.loads(line[len('SANDBOX_PROBE='):]))['operations']
            except (ValueError, TypeError, KeyError):
                reasons.add('sandbox-probe-invalid')
        if line in ('SANDBOX_PROBE_FAILURE=execution-failed', 'SANDBOX_PROBE_FAILURE=timed-out',
                    'SANDBOX_PROBE_FAILURE=invalid-or-unavailable'):
            reasons.add('sandbox-probe-' + line.split('=', 1)[1])
        if panic_message and line.strip():
            inspect(line)
            panic_message = False
        stage = re.fullmatch(r'ABI_PROBE_STAGE=([a-z-]+)', line)
        if stage and stage[1] in ABI_STAGES:
            reasons.add('abi-stage-' + stage[1])
        state = re.fullmatch(r'MOBILE_SMOKE_STATE=([a-z-]+)', line)
        if state and state[1] in MOBILE_SMOKE_STATES:
            reasons.add('mobile-smoke-' + state[1])
        phase = re.fullmatch(r'MOBILE_SMOKE_PHASE=([a-z-]+)', line)
        if phase and phase[1] in MOBILE_SMOKE_PHASES:
            reasons.add('mobile-phase-' + phase[1])
        try:
            record = json.loads(line)
        except (json.JSONDecodeError, ValueError):
            record = None
        if isinstance(record, dict) and record.get('reason') == 'compiler-message':
            message = record.get('message', {})
            if isinstance(message, dict) and message.get('level') in ('error', 'failure-note'):
                code = message.get('code') or {}
                if isinstance(code, dict) and re.fullmatch(r'E\d{4}', str(code.get('code', ''))):
                    codes.add(code['code'])
                inspect(str(message.get('message', '')))
                spans = message.get('spans')
                for span in spans if isinstance(spans, list) else []:
                    if isinstance(span, dict) and span.get('is_primary'):
                        add_location(sources.location(span.get('file_name'), span.get('line_start'), span.get('column_start')))
            continue
        if (re.match(r'^\s*(?:error|fatal error|warning|Traceback|\w*Error):?', line, re.IGNORECASE)
                or 'error:' in line.lower() or 'FAILED' in line
                or line.startswith('NDI smoke failed: ')
                or line.startswith('Provisioning failed: ')
                or re.search(r'(?:ld|ld.lld|lld-link):|(?:fatal )?error (?:LNK|C)\d{4}|^\s*Undefined symbols(?:\s+for architecture|:)|^\s*symbol\(s\) not found', line, re.IGNORECASE)):
            inspect(line)
        match = re.match(r'^\s*error\[(E\d{4})\]:', line)
        if match:
            codes.add(match[1])
        match = re.search(r'\b(?:fatal )?error ((?:LNK|C)\d{4})\b', line)
        if match:
            codes.add(match[1])
        match = re.match(r'^\s*-->\s+(.+):(\d+):(\d+)\s*$', line)
        if match:
            add_location(sources.location(match[1], int(match[2]), int(match[3])))
        match = re.match(r'^(.+):(\d+):(\d+):\s+(?:fatal )?error:', line)
        if match:
            add_location(sources.location(match[1], int(match[2]), int(match[3])))
        match = re.match(r'^\s*File "([^"]+)", line (\d+),', line)
        if match:
            add_location(sources.location(match[1], int(match[2])))
        match = re.match(r"^thread '[^']+'(?: \(\d+\))? panicked at (.+):(\d+):(\d+):?\s*$", line)
        if match:
            reasons.add('rust-panic')
            add_location(sources.location(match[1], int(match[2]), int(match[3])))
            panic_message = True
        match = re.match(r'^test ([^\s]+) \.\.\. FAILED\s*$', line)
        if match:
            name = sources.test(match[1])
            if name:
                tests.add(name)
        match = re.match(r'^ValueError: ([A-Za-z_][A-Za-z_0-9:]*): required native test failed', line)
        if match:
            name = sources.test(match[1])
            if name:
                tests.add(name)
    limited = any(len(group) > MAX_ITEMS for group in (codes, reasons, locations, tests, native_context))
    result = {'schema': 1, 'error_codes': sorted(codes)[:MAX_ITEMS],
        'reasons': sorted(reasons)[:MAX_ITEMS],
        'project_locations': [locations[key] for key in sorted(locations)[:MAX_ITEMS]],
        'failed_tests': sorted(tests)[:MAX_ITEMS], 'truncated': truncated or limited}
    if native_context:
        result['native_context'] = [{'phase': phase, 'test': name, 'kind': kind}
                                   for phase, name, kind in sorted(native_context)[:MAX_ITEMS]]
    if sandbox is not None:
        result['sandbox_operations'] = sandbox
    return bounded(result)


def bounded(result):
    while len(json.dumps(result, sort_keys=True).encode('utf-8')) > MAX_OUTPUT_BYTES:
        groups = [result.get(key, []) for key in ('project_locations', 'failed_tests', 'error_codes', 'reasons', 'sandbox_operations', 'native_context')]
        largest = max(groups, key=lambda group: len(json.dumps(group)))
        if not largest:
            return {'schema': 1, 'reasons': ['diagnostic-summary-truncated'], 'truncated': True}
        largest.pop()
        result['truncated'] = True
    return result


def summarize_log(path, root):
    path = Path(path)
    size = path.stat().st_size
    with path.open('rb') as stream:
        stream.seek(max(0, size - MAX_INPUT_BYTES))
        data = stream.read(MAX_INPUT_BYTES)
    if size > MAX_INPUT_BYTES:
        data = data.partition(b'\n')[2]  # A partial JSON record is not evidence.
    return summarize_text(data.decode('utf-8', errors='replace'), PublicSources(root), truncated=size > MAX_INPUT_BYTES)


def summarize_exception(error, root):
    sources = PublicSources(root)
    result = summarize_text('error: ' + str(error)[:MAX_INPUT_BYTES], sources)
    result['exception'] = next((name for name in ('FileNotFoundError', 'PermissionError',
        'TimeoutExpired', 'CalledProcessError', 'OSError', 'ValueError', 'RuntimeError')
        if any(cls.__name__ == name for cls in type(error).__mro__)), 'Failure')
    locations = [sources.location(frame.filename, frame.lineno)
                 for frame in traceback.extract_tb(error.__traceback__)]
    result['project_locations'] = [entry for entry in locations if entry][-MAX_ITEMS:]
    if type(getattr(error, 'errno', None)) is int:
        result['errno'] = error.errno
    if type(getattr(error, 'returncode', None)) is int:
        result['exit_code'] = error.returncode
    return bounded(result)
