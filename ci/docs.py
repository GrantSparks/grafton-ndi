#!/usr/bin/env python3
"""Check repository documentation without fetching remote pages.

Local links and same-version GitHub links are checked against the checkout;
immutable historical GitHub links are checked against local Git objects when
available. External pages are outside this check. With --compile, standalone
Rust fences are compiled (never run) against the current crate and locked
versions of its dependencies. Use a linked runnable example for code fragments.
With --rustdoc, GitHub Pages API URLs and current-version docs.rs URLs are
checked against generated HTML. Prose API names in current guides are checked
against the same item and member names.
Documented dependency features and the README feature table are checked against
Cargo.toml so removed or renamed features cannot remain in install instructions.
Native table entry points need public SDK search aliases or an explicit internal
classification. Generated declarations also reject underscore-prefixed parameters.
"""
import argparse
from collections import Counter
from dataclasses import dataclass
import html
from html.parser import HTMLParser
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = 'https://github.com/GrantSparks/grafton-ndi'
API_SITE = 'https://grantsparks.github.io/grafton-ndi/'
FENCE = re.compile(r'^ {0,3}(`{3,}|~{3,})([^\n]*)$')
HEADING = re.compile(r'^ {0,3}#{1,6}\s+(.*?)\s*#*\s*$')
RUST_RAW_STRING = re.compile(r'(?:br|cr|r)(#*)"')
RUST_QUOTED_STRING = re.compile(r'(?:b|c)?"')
RUST_CHARACTER = re.compile(r"(?:b)?'(?:[^'\\\n]|\\(?:.|u\{[0-9a-fA-F_]+\}))'")
RUST_IDENTIFIER = re.compile(r'(?:r#)?[A-Za-z_][A-Za-z_0-9]*')

# These entry points have no public operation: ownership releases resources
# automatically, while selection caches the native version before exposing info.
# Exact names keep a new entry point from inheriting an exception accidentally.
INTERNAL_SDK_ENTRY_POINTS = {
    'NDIlib_version': 'runtime selection caches the version in RuntimeInfo',
    'NDIlib_destroy': 'the runtime initialization owner releases initialization on drop',
    'NDIlib_find_destroy': 'Finder releases its instance on drop',
    'NDIlib_recv_destroy': 'Receiver releases its instance on drop',
    'NDIlib_send_destroy': 'Sender releases its instance on drop',
    'NDIlib_framesync_destroy': 'FrameSync releases its instance on drop',
    'NDIlib_avsync_destroy': 'AvSync releases its instance on drop',
    'NDIlib_genlock_destroy': 'Genlock releases its instance on drop',
    'NDIlib_routing_destroy': 'Router releases its instance on drop',
    'NDIlib_send_advertiser_destroy': 'SendAdvertiser releases its instance on drop',
    'NDIlib_recv_advertiser_destroy': 'RecvAdvertiser releases its instance on drop',
    'NDIlib_send_listener_destroy': 'SendListener releases its instance on drop',
    'NDIlib_recv_listener_destroy': 'RecvListener releases its instance on drop',
    'NDIlib_file_player_destroy': 'FilePlayer releases its instance on its owner thread',
    'NDIlib_file_recorder_destroy': 'FileRecorder releases its instance on its owner thread',
    'NDIlib_recv_free_video_v2': 'the receiver video guard releases its frame on drop',
    'NDIlib_recv_free_audio_v3': 'the receiver audio guard releases its frame on drop',
    'NDIlib_recv_free_metadata': 'the receiver metadata guard releases its frame on drop',
    'NDIlib_recv_free_string': 'the internal returned-string guard releases SDK text',
    'NDIlib_send_free_metadata': 'the sender metadata guard releases its frame on drop',
    'NDIlib_framesync_free_video': 'the FrameSync video guard releases its frame on drop',
    'NDIlib_framesync_free_audio_v2': 'the FrameSync audio guard releases its frame on drop',
    'NDIlib_avsync_free_audio': 'the AV sync audio guard releases its frame on drop',
    'NDIlib_send_listener_free_events': 'the internal sender event-batch guard releases its array',
    'NDIlib_recv_listener_free_events': 'the internal receiver event-batch guard releases its array',
}


def rust_tokens(text):
    """Tokenize enough Rust to distinguish declarations/attributes from examples.

    Comments may nest; ordinary, byte, C and raw strings are indivisible tokens,
    so commented-out declarations or strings containing fake attributes cannot
    satisfy native coverage. Offsets retain actionable source line numbers.
    """
    tokens, index = [], 0
    while index < len(text):
        if text[index].isspace():
            index += 1
            continue
        if text.startswith('//', index):
            end = text.find('\n', index)
            index = len(text) if end < 0 else end + 1
            continue
        if text.startswith('/*', index):
            depth, index = 1, index + 2
            while index < len(text) and depth:
                if text.startswith('/*', index):
                    depth, index = depth + 1, index + 2
                elif text.startswith('*/', index):
                    depth, index = depth - 1, index + 2
                else:
                    index += 1
            continue
        raw = RUST_RAW_STRING.match(text, index)
        if raw:
            ending = '"' + raw.group(1)
            end = text.find(ending, raw.end())
            end = len(text) if end < 0 else end + len(ending)
            tokens.append((text[index:end], index))
            index = end
            continue
        quoted = RUST_QUOTED_STRING.match(text, index)
        if quoted:
            end = quoted.end()
            while end < len(text):
                if text[end] == '\\':
                    end += 2
                elif text[end] == '"':
                    end += 1
                    break
                else:
                    end += 1
            tokens.append((text[index:end], index))
            index = end
            continue
        character = RUST_CHARACTER.match(text, index)
        if character:
            tokens.append((character.group(), index))
            index = character.end()
            continue
        identifier = RUST_IDENTIFIER.match(text, index)
        if identifier:
            tokens.append((identifier.group(), index))
            index = identifier.end()
            continue
        tokens.append((text[index], index))
        index += 1
    return tokens


def native_sdk_entries(root):
    """Discover registered calls from native macro invocations, including new files."""
    found = {}
    registries = {'native_table', 'optional_tables', 'advanced_tables', 'file_families'}
    for path in sorted((root / 'src/sys').rglob('*.rs')):
        text = path.read_text(encoding='utf-8')
        if not re.search(r'\b(?:native_table|optional_tables|advanced_tables|file_families)\s*!', text):
            continue
        tokens = rust_tokens(text)
        for index in range(len(tokens) - 2):
            if tokens[index][0] not in registries or tokens[index + 1][0] != '!':
                continue
            if tokens[index + 2][0] not in {'{', '(', '['}:
                continue
            opening = tokens[index + 2][0]
            closing = {'{': '}', '(': ')', '[': ']'}[opening]
            depth, cursor = 1, index + 3
            while cursor < len(tokens) and depth:
                word = tokens[cursor][0]
                depth += (word == opening) - (word == closing)
                if (depth and word == 'fn' and cursor + 2 < len(tokens)
                        and re.fullmatch(r'NDIlib_\w+', tokens[cursor + 1][0])
                        and tokens[cursor + 2][0] == '('):
                    name, offset = tokens[cursor + 1]
                    found.setdefault(name, (path, text.count('\n', 0, offset) + 1))
                cursor += 1
    return found


def token_group_end(words, start):
    """Return the token after a balanced Rust group."""
    pairs, stack = {'(': ')', '[': ']', '{': '}'}, []
    for index in range(start, len(words)):
        word = words[index]
        if word in pairs:
            stack.append(pairs[word])
        elif stack and word == stack[-1]:
            stack.pop()
            if not stack:
                return index + 1
    return len(words)


def token_arguments(words):
    """Split a macro's arguments without splitting its nested groups."""
    result, start, index = [], 0, 0
    while index < len(words):
        if words[index] in {'(', '[', '{'}:
            index = token_group_end(words, index)
        elif words[index] == ',':
            result.append(words[start:index])
            start = index = index + 1
        else:
            index += 1
    if start < len(words):
        result.append(words[start:])
    return result


def token_attributes(words, start):
    attributes = []
    while words[start:start + 2] == ['#', '[']:
        end = token_group_end(words, start + 1)
        attributes.append(words[start + 2:end - 1])
        start = end
    return attributes, start


def attribute_aliases(attributes):
    return {attribute[4][1:-1] for attribute in attributes
            if (len(attribute) == 6 and attribute[:4] == ['doc', '(', 'alias', '=']
                and attribute[-1] == ')' and re.fullmatch(r'"NDIlib_\w+"', attribute[4]))}


def public_function_at(words, start):
    if words[start:start + 1] != ['pub']:
        return False
    start += 1
    # A restricted visibility is not part of the published API.
    if words[start:start + 1] == ['(']:
        return False
    while start < len(words) and words[start] in {'async', 'unsafe', 'const', 'extern'}:
        modifier = words[start]
        start += 1
        if modifier == 'extern' and start < len(words) and words[start].startswith('"'):
            start += 1
    return words[start:start + 1] == ['fn']


def production_words(words):
    """Exclude inline #[cfg(test)] items as well as separate test source files."""
    result, index = [], 0
    while index < len(words):
        attributes, after = token_attributes(words, index)
        if ['cfg', '(', 'test', ')'] in attributes:
            index = after
            while index < len(words) and words[index] not in {'{', ';'}:
                index += 1
            if index < len(words):
                index = token_group_end(words, index) if words[index] == '{' else index + 1
        else:
            result.extend(words[index:after] if after > index else words[index:index + 1])
            index = after if after > index else index + 1
    return result


def macro_public_attribute_slots(words):
    """Prove which metadata arguments a local macro forwards to public methods.

    The common wrapper form is `$(#[$attr:meta])* $method:ident` in the
    matcher and `$(#[$attr])* pub fn $method` in the expansion. Matching both
    metavariables prevents an ignored or privately forwarded alias from
    satisfying coverage. Ordinary literal attributes on generated public
    declarations are handled by public_function_at directly.
    """
    result = {}
    for index in range(len(words) - 4):
        if words[index:index + 2] != ['macro_rules', '!'] or words[index + 3] != '{':
            continue
        matcher = index + 4
        if words[matcher] not in {'(', '[', '{'}:
            continue
        after = token_group_end(words, matcher)
        if words[after:after + 3] != ['=', '>', '{']:
            continue
        expansion_end = token_group_end(words, after + 2)
        expansion = words[after + 3:expansion_end - 1]
        slots = set()
        for slot, argument in enumerate(token_arguments(words[matcher + 1:after - 1])):
            if (len(argument) == 15 and argument[:5] == ['$', '(', '#', '[', '$']
                    and argument[6:12] == [':', 'meta', ']', ')', '*', '$']
                    and argument[13:] == [':', 'ident']):
                attr, method = argument[5], argument[12]
                forwarded = ['$', '(', '#', '[', '$', attr, ']', ')', '*', 'pub', 'fn', '$', method]
                if any(expansion[cursor:cursor + len(forwarded)] == forwarded
                       for cursor in range(len(expansion) - len(forwarded) + 1)):
                    slots.add(slot)
        if slots:
            result[words[index + 2]] = slots
    return result


def public_sdk_aliases(words):
    aliases = set()
    words = production_words(words)
    macros = macro_public_attribute_slots(words)
    for index in range(len(words)):
        attributes, after = token_attributes(words, index)
        if attributes and public_function_at(words, after):
            aliases.update(attribute_aliases(attributes))
        if words[index] not in macros or words[index + 1:index + 2] != ['!']:
            continue
        opening = index + 2
        if words[opening:opening + 1] not in [['('], ['{'], ['[']]:
            continue
        end = token_group_end(words, opening)
        for slot, argument in enumerate(token_arguments(words[opening + 1:end - 1])):
            if slot not in macros[words[index]]:
                continue
            attrs, tail = token_attributes(argument, 0)
            if len(argument[tail:]) == 1 and RUST_IDENTIFIER.fullmatch(argument[tail]):
                aliases.update(attribute_aliases(attrs))
    return aliases


def sdk_alias_violations(root):
    """Every registered SDK call is publicly searchable or explicitly internal."""
    entries, aliases = native_sdk_entries(root), set()
    for path in sorted((root / 'src').rglob('*.rs')):
        if (path.is_relative_to(root / 'src/sys') or path.stem.endswith('_tests')
                or 'test_support' in path.parts or path.stem in {'tests', 'test_support'}):
            continue
        text = path.read_text(encoding='utf-8')
        if re.search(r'\bdoc\s*\(\s*alias', text):
            aliases.update(public_sdk_aliases([word for word, _ in rust_tokens(text)]))
    found = []
    for name, (path, line) in sorted(entries.items()):
        if name not in aliases and name not in INTERNAL_SDK_ENTRY_POINTS:
            found.append(f'{path.relative_to(root)}:{line}: SDK entry point {name} '
                         'needs a public doc(alias) or an explicit internal reason')
    return found


class HtmlDeclarations(HTMLParser):
    """Read declarations, excluding prose, examples and dependency source HTML."""

    def __init__(self):
        super().__init__()
        self.depth = 0
        self.parts = []
        self.found = []

    def handle_starttag(self, tag, attrs):
        if tag in {'area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'param', 'source', 'track', 'wbr'}:
            return
        classes = dict(attrs).get('class', '').split()
        if self.depth:
            self.depth += 1
        elif (tag == 'pre' and 'item-decl' in classes) or (tag == 'h4' and 'code-header' in classes):
            self.depth, self.parts = 1, []

    def handle_endtag(self, tag):
        if tag in {'area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'param', 'source', 'track', 'wbr'}:
            return
        if self.depth:
            self.depth -= 1
            if not self.depth:
                self.found.append(''.join(self.parts))

    def handle_data(self, data):
        if self.depth:
            self.parts.append(data)


def parameter_name_violations(root, directory):
    """Check the actual published signatures, including macro-generated methods."""
    found = []
    for path in sorted((directory / 'grafton_ndi').rglob('*.html')):
        parser = HtmlDeclarations()
        parser.feed(path.read_text(encoding='utf-8'))
        for declaration in parser.found:
            # Derived and external trait implementations also have code-header
            # blocks. Their compiler-chosen names are not authored public API.
            # Inherent methods say `pub`; a public trait item-decl includes its
            # required/default method signatures although they omit `pub`.
            if not (re.search(r'\bpub\s+(?:(?:async|unsafe|const)\s+)*fn\s+', declaration)
                    or re.search(r'\bpub\s+(?:unsafe\s+)?trait\s+', declaration)):
                continue
            for name in sorted(set(re.findall(r'(?<![\w\'])\b(_\w*)\s*:', declaration))):
                found.append(f'{path.relative_to(directory)}: public parameter {name} '
                             'starts with an underscore')
    return found


@dataclass(frozen=True)
class Snippet:
    path: Path
    line: int
    code: str


def documents(root):
    return sorted(path for path in [root / 'README.md', root / 'CONTRIBUTING.md',
                                   root / 'CHANGELOG.md', root / 'ci/README.md',
                                   root / 'examples/README.md', *root.glob('docs/**/*.md')]
                  if path.is_file())


def fences(text):
    """Yield (opening line, closing line, info, body) for fenced code blocks."""
    opening = None
    for number, line in enumerate(text.splitlines(), 1):
        match = FENCE.match(line)
        if opening is None:
            if match:
                opening = (number, match.group(1), match.group(2).strip())
                body = []
        elif (match and match.group(1)[0] == opening[1][0]
              and len(match.group(1)) >= len(opening[1]) and not match.group(2).strip()):
            yield opening[0], number, opening[2], '\n'.join(body) + '\n'
            opening = None
        else:
            body.append(line)
    if opening:
        yield opening[0], None, opening[2], '\n'.join(body) + '\n'


def prose(text):
    """Mask fences and inline code, preserving line numbers for diagnostics."""
    lines = text.splitlines()
    for first, last, _, _ in fences(text):
        for index in range(first - 1, last or len(lines)):
            lines[index] = ''
    return '\n'.join(re.sub(r'(`+).*?\1', '', line) for line in lines)


def links(text):
    """Yield destinations of inline links/images and reference definitions.

    Balanced parentheses and angle-bracket destinations cover paths and URLs
    commonly used in these guides. Code is excluded before this function runs.
    """
    for match in re.finditer(r'\]\(\s*', text):
        start = match.end()
        if start >= len(text):
            continue
        if text[start] == '<':
            end = text.find('>', start + 1)
            if end != -1:
                yield text[start + 1:end], text.count('\n', 0, match.start()) + 1
            continue
        depth, end = 0, start
        while end < len(text):
            character = text[end]
            if character == '\\':
                end += 2
                continue
            if character == '(':
                depth += 1
            elif character == ')':
                if depth == 0:
                    break
                depth -= 1
            elif character.isspace() and depth == 0:
                break
            end += 1
        if end > start:
            yield re.sub(r'\\([() ])', r'\1', text[start:end]), text.count('\n', 0, match.start()) + 1
    for match in re.finditer(r'^ {0,3}\[[^\]\n]+\]:\s*(?:<([^>]+)>|(\S+))', text, re.MULTILINE):
        yield match.group(1) or match.group(2), text.count('\n', 0, match.start()) + 1


def anchors(text):
    """GitHub-style heading anchors, including duplicate heading suffixes."""
    lines = text.splitlines()
    fenced = set()
    for first, last, _, _ in fences(text):
        fenced.update(range(first - 1, last or len(lines)))
    found, counts = set(), Counter()
    for index, line in enumerate(lines):
        if index in fenced:
            continue
        match = HEADING.match(line)
        heading = match.group(1) if match else None
        if heading is None and index + 1 < len(lines) and line.strip() and re.fullmatch(r' {0,3}(?:=+|-+)\s*', lines[index + 1]):
            heading = line.strip()
        if heading is None:
            continue
        heading = re.sub(r'\[([^]]+)\]\([^)]*\)', r'\1', heading)
        heading = re.sub(r'<[^>]*>', '', heading)
        heading = html.unescape(heading).lower()
        slug = re.sub(r'[^\w\- ]', '', heading).replace(' ', '-')
        suffix = counts[slug]
        found.add(slug + (f'-{suffix}' if suffix else ''))
        counts[slug] += 1
    found.update(re.findall(r'<a\s+[^>]*(?:id|name)=["\']([^"\']+)["\']', text, re.IGNORECASE))
    return found


def version(root):
    return tomllib.loads((root / 'Cargo.toml').read_text(encoding='utf-8'))['package']['version']


def dependency_feature_violations(label, code, manifest):
    """Check feature selections in a TOML dependency example, including renames."""
    try:
        example = tomllib.loads(code)
    except tomllib.TOMLDecodeError as error:
        return [f'{label}: invalid TOML example: {error}']
    name = manifest['package']['name']
    available = set(manifest.get('features', {}))
    found = []
    for section in ('dependencies', 'dev-dependencies', 'build-dependencies'):
        for alias, dependency in example.get(section, {}).items():
            if not isinstance(dependency, dict) or dependency.get('package', alias) != name:
                continue
            for feature in dependency.get('features', []):
                if feature not in available:
                    found.append(f'{label}: unknown {name} dependency feature: {feature}')
    return found


def readme_feature_violations(root, manifest):
    """Keep the public feature table in sync with the package's feature names."""
    table, documented, found = False, set(), []
    available = set(manifest.get('features', {})) - {'default'}
    lines = (root / 'README.md').read_text(encoding='utf-8').splitlines()
    for line, text in enumerate(lines, 1):
        cells = [cell.strip() for cell in text.strip().strip('|').split('|')]
        if text.startswith('|') and cells[0] == 'Feature':
            table = True
        elif table and text.startswith('|'):
            for feature in re.findall(r'`([^`]+)`', cells[0]):
                documented.add(feature)
                if feature not in available:
                    found.append(f'README.md:{line}: unknown public feature: {feature}')
        elif table:
            break
    if table:
        for feature in sorted(available - documented):
            found.append(f'README.md: feature table is missing: {feature}')
    return found


def doc_comment_text(text):
    """Extract line doc comments as Markdown without changing line numbers."""
    lines = []
    for line in text.splitlines():
        match = re.match(r'\s*(?:///(?!/)|//!) ?(.*)', line)
        lines.append(match.group(1) if match else '')
    return '\n'.join(lines)


def source_documentation(root):
    for path in sorted(root.glob('src/**/*.rs')):
        yield path, doc_comment_text(path.read_text(encoding='utf-8'))


def release_link_violations(root, source, text, release):
    found = []
    for url, line in links(prose(text)):
        label = f'{source.relative_to(root)}:{line}'
        if re.match(r'https://docs\.rs/grafton-ndi/latest(?:/|$)', url):
            found.append(f'{label}: API link must name version {release}: {url}')
    return found


def readme_link_violations(root):
    """The README must work outside the checkout, including on crates.io."""
    package = tomllib.loads((root / 'Cargo.toml').read_text(encoding='utf-8'))['package']
    name = package.get('readme', 'README.md')
    if name is False:
        return []
    path = root / (name if isinstance(name, str) else 'README.md')
    if not path.is_file():
        return [f'{name}: packaged README is missing']
    found = []
    for url, line in links(prose(path.read_text(encoding='utf-8'))):
        parsed = urlsplit(url)
        if not parsed.scheme and not parsed.netloc and not url.startswith('#'):
            found.append(f'{path.relative_to(root)}:{line}: packaged README links must use absolute URLs: {url}')
    return found


def destination(root, source, url, release):
    """Return (path, fragment, historical ref), or None for an external URL."""
    parsed = urlsplit(url)
    fragment = unquote(parsed.fragment)
    if parsed.scheme or parsed.netloc:
        prefix = REPOSITORY + '/'
        if not url.startswith(prefix):
            return None
        parts = parsed.path.split('/')
        if len(parts) < 6 or parts[3] not in ('blob', 'tree'):
            return None
        ref = parts[4]
        relative = unquote('/'.join(parts[5:]))
        if re.fullmatch(r'v?\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?', ref):
            return Path(relative), fragment, 'refs/tags/' + ref
        if re.fullmatch(r'[0-9a-f]{40}', ref):
            return Path(relative), fragment, ref
        # Branch links describe the checkout only for the development branch.
        if ref == 'main':
            return root / relative, fragment, None
        return None
    return (source.parent / unquote(parsed.path) if parsed.path else source), fragment, None


def link_violations(root, source, text, release, repository_only=False):
    found = []
    for url, line in links(prose(text)):
        if repository_only and not url.startswith(REPOSITORY + '/'):
            continue  # Rust intra-doc links are names, not filesystem paths.
        target = destination(root, source, url, release)
        if target is None:
            continue
        path, fragment, ref = target
        label = f'{source.relative_to(root)}:{line}'
        if ref:
            if ref.startswith('refs/tags/'):
                # CI fetches published tags. A future release name is not a
                # live URL merely because its target exists in this checkout.
                exists = subprocess.run(['git', '-C', str(root), 'rev-parse', '--verify', '--quiet', ref],
                                        text=True, capture_output=True)
                if exists.returncode:
                    found.append(f'{label}: unpublished release tag: {url}; use main for unreleased documentation')
                    continue
            # Validate the ref's actual content, including anchors, rather than
            # substituting files from the current checkout.
            command = ['git', '-C', str(root), 'show', f'{ref}:{path.as_posix()}']
            result = subprocess.run(command, text=True, capture_output=True)
            if result.returncode:
                found.append(f'{label}: historical target is absent: {url}')
                continue
            content = result.stdout
        else:
            path = path.resolve()
            if not path.is_relative_to(root.resolve()):
                found.append(f'{label}: link escapes the repository: {url}')
                continue
            if not path.exists():
                found.append(f'{label}: missing target: {url}')
                continue
            if path.is_dir():
                if not fragment:
                    continue
                path /= 'README.md'
                if not path.is_file():
                    found.append(f'{label}: directory has no README for anchor: {url}')
                    continue
            if not fragment:
                continue
            if path.suffix != '.md':
                # GitHub line anchors belong to source files, not Markdown headings.
                if re.fullmatch(r'L\d+(?:-L\d+)?', fragment):
                    lines = path.read_text(encoding='utf-8').splitlines()
                    if all(0 < int(number) <= len(lines) for number in re.findall(r'\d+', fragment)):
                        continue
                found.append(f'{label}: unsupported source anchor: {url}')
                continue
            content = path.read_text(encoding='utf-8')
        if fragment and fragment not in anchors(content):
            found.append(f'{label}: missing anchor: {url}')
    return found


def check(root):
    """Return (violations, standalone Rust snippets) for the current docs."""
    root = root.resolve()
    manifest = tomllib.loads((root / 'Cargo.toml').read_text(encoding='utf-8'))
    release = manifest['package']['version']
    found, snippets = readme_feature_violations(root, manifest), []
    for path in documents(root):
        text = path.read_text(encoding='utf-8')
        found.extend(link_violations(root, path, text, release))
        if path.name == 'CHANGELOG.md':
            continue  # Historical examples describe their named releases.
        found.extend(release_link_violations(root, path, text, release))
        lines = text.splitlines()
        for first, last, info, code in fences(text):
            label = f'{path.relative_to(root)}:{first}'
            if last is None:
                found.append(f'{label}: unclosed code fence')
            tags = [tag.strip() for tag in info.split(',')]
            if tags[0] == 'toml':
                found.extend(dependency_feature_violations(label, code, manifest))
            if not tags or tags[0] != 'rust':
                continue
            if re.search(r'^\s*#(?:\s|$)', code, re.MULTILINE):
                found.append(f'{label}: rustdoc hidden lines are visible in Markdown; use a complete example')
            if 'ignore' in tags:
                nearby = '\n'.join(lines[max(0, first - 13):min(len(lines), (last or first) + 12)])
                examples = {example.resolve() for example in (root / 'examples').glob('*.rs')}
                targets = [destination(root, path, url, release) for url, _ in links(prose(nearby))]
                if not any(target and not target[2] and target[0].resolve() in examples for target in targets):
                    found.append(f'{label}: an illustrative Rust fragment needs a nearby link to a runnable .rs example')
            elif not re.search(r'\bfn\s+main\s*\(', code):
                found.append(f'{label}: Rust fence must be standalone, or rust,ignore with a linked runnable example')
            else:
                snippets.append(Snippet(path, first, code))
    index = root / 'examples/README.md'
    if index.exists():
        linked = set()
        for url, _ in links(prose(index.read_text(encoding='utf-8'))):
            target = destination(root, index, url, release)
            if target and not target[2]:
                linked.add(target[0].resolve())
        for example in sorted((root / 'examples').glob('*.rs')):
            if example.resolve() not in linked:
                found.append(f'examples/README.md: missing example: {example.name}')
    for path, rustdoc in source_documentation(root):
        found.extend(link_violations(root, path, rustdoc, release, repository_only=True))
        found.extend(release_link_violations(root, path, rustdoc, release))
    return found, snippets


class HtmlAnchors(HTMLParser):
    """Read actual anchors from generated rustdoc rather than guessing IDs."""

    def __init__(self):
        super().__init__()
        self.found = set()
        self.links = set()

    def handle_starttag(self, tag, attrs):
        for name, value in attrs:
            if value and tag == 'a' and name == 'href':
                self.links.add(value)
            if value and (name == 'id' or (tag == 'a' and name == 'name')):
                self.found.add(value)


def rustdoc_violations(root, directory):
    """Check primary Pages and current-version docs.rs links against rustdoc."""
    root, directory = root.resolve(), directory.resolve()
    release = version(root)
    found, cache = [], {}
    sources = [(path, path.read_text(encoding='utf-8')) for path in documents(root)
               if path.name != 'CHANGELOG.md']
    sources.extend(source_documentation(root))
    for source, text in sources:
        for url, line in links(prose(text)):
            parsed = urlsplit(url)
            parts = parsed.path.strip('/').split('/')
            if url.startswith(API_SITE):
                relative = unquote('/'.join(parts[1:])) or 'grafton_ndi'
            elif parsed.netloc == 'docs.rs' and parts[:2] == ['grafton-ndi', release]:
                relative = unquote('/'.join(parts[2:])) or 'grafton_ndi'
            else:
                continue
            path = (directory / relative).resolve()
            label = f'{source.relative_to(root)}:{line}'
            if not path.is_relative_to(directory):
                found.append(f'{label}: rustdoc link escapes the generated directory: {url}')
                continue
            if path.is_dir():
                path /= 'index.html'
            if not path.is_file():
                found.append(f'{label}: missing generated rustdoc page: {url}')
                continue
            if parsed.fragment:
                if path not in cache:
                    parser = HtmlAnchors()
                    parser.feed(path.read_text(encoding='utf-8'))
                    cache[path] = parser.found
                if unquote(parsed.fragment) not in cache[path]:
                    found.append(f'{label}: missing generated rustdoc anchor: {url}')
    return found


# Native XML vocabulary is not a Rust API item. Keep exceptions explicit so
# a rename cannot silently inherit a broad SDK-name exemption.
NON_API_CODE_NAMES = {'ndi_color_info'}
RUSTDOC_ITEM = re.compile(r'(?:struct|enum|trait|type|fn|constant|static|macro|union)\.([A-Za-z_]\w*)\.html$')
RUSTDOC_MEMBER = re.compile(r'(?:method|tymethod|associatedconstant|associatedtype|variant|structfield)\.([A-Za-z_]\w*)(?:-\d+)?$')


def documented_api_names(directory):
    """Names reachable from the current crate index, excluding stale HTML."""
    crate = (directory / 'grafton_ndi').resolve()
    names, qualified = {'grafton_ndi'}, set()
    pending, visited = [crate / 'index.html'], set()
    while pending:
        page = pending.pop()
        if page in visited:
            continue
        visited.add(page)
        item = RUSTDOC_ITEM.fullmatch(page.name)
        owner = item.group(1) if item else None
        if owner:
            names.add(owner)
        elif page.name == 'index.html':
            names.add(page.parent.name)
        parser = HtmlAnchors()
        parser.feed(page.read_text(encoding='utf-8'))
        for anchor in parser.found:
            member = RUSTDOC_MEMBER.fullmatch(anchor)
            if member:
                name = member.group(1)
                names.add(name)
                if owner:
                    qualified.add(f'{owner}::{name}')
        for link in parser.links:
            parsed = urlsplit(link)
            if parsed.scheme or parsed.netloc or not parsed.path:
                continue
            target = (page.parent / unquote(parsed.path)).resolve()
            if target.is_relative_to(crate) and target.suffix == '.html' and target.is_file():
                pending.append(target)
    return names, qualified


def guide_api_name_violations(root, directory):
    """Reject stale API identifiers in prose code spans, excluding history."""
    root, directory = root.resolve(), directory.resolve()
    if not (directory / 'grafton_ndi/index.html').is_file():
        return ['missing generated rustdoc crate index: grafton_ndi/index.html']
    names, qualified = documented_api_names(directory)
    # Guide link labels also use Cargo example target names without .rs.
    names.update(example.stem for example in (root / 'examples').glob('*.rs'))
    found = []
    for source in documents(root):
        relative = source.relative_to(root)
        if source.name == 'CHANGELOG.md' or relative == Path('docs/api-conventions.md') or relative.parts[:2] == ('docs', 'migration'):
            continue
        text = source.read_text(encoding='utf-8')
        excluded = set()
        for first, last, _, _ in fences(text):
            excluded.update(range(first, (last or len(text.splitlines())) + 1))
        for line, content in enumerate(text.splitlines(), 1):
            if line in excluded:
                continue
            for span in re.finditer(r'(?P<ticks>`+)(?P<code>.+?)(?P=ticks)', content):
                token = span.group('code').strip()
                if token in NON_API_CODE_NAMES:
                    continue
                # Paths, files, target triples and globs cannot match either
                # identifier form. Module paths contain no uppercase owner.
                member = re.fullmatch(r'(?:[a-z_]\w*::)*([A-Z]\w*)::([A-Za-z_]\w*)(?:\([^()]*\)?)?', token)
                call = re.fullmatch(r'([a-z]\w*)\([^()]*\)?', token)
                snake = re.fullmatch(r'[a-z][a-z0-9]*(?:_[a-z0-9]+)+', token)
                if member:
                    present = f'{member.group(1)}::{member.group(2)}' in qualified
                elif call:
                    present = call.group(1) in names
                elif snake:
                    present = token in names
                else:
                    continue
                if not present:
                    found.append(f'{relative}:{line}: unknown documented API name: {token}')
    return found


def compile_snippets(root, snippets):
    """Compile complete Rust examples against all Cargo features, never run them."""
    if not snippets:
        return []
    with tempfile.TemporaryDirectory(prefix='grafton-ndi-docs-') as directory:
        directory = Path(directory)
        manifest = ('[package]\nname = "grafton-ndi-doc-snippets"\nversion = "0.0.0"\nedition = "2021"\n'
                    '[dependencies]\ngrafton-ndi = { path = ' + json.dumps(str(root)) + ', '
                    'features = ["advanced", "file-io", "image-encoding", "tokio", "smol"] }\n'
                    'tokio = { version = "1", features = ["macros", "rt-multi-thread"] }\n')
        (directory / 'Cargo.toml').write_text(manifest, encoding='utf-8')
        shutil.copyfile(root / 'Cargo.lock', directory / 'Cargo.lock')
        bins = directory / 'src/bin'
        bins.mkdir(parents=True)
        for index, snippet in enumerate(snippets):
            (bins / f'snippet_{index}.rs').write_text(snippet.code, encoding='utf-8')
        env = dict(os.environ)
        env['CARGO_TARGET_DIR'] = str(root / 'target/doc-snippets')
        # Reuse the checked-in dependency lock. The only lockfile edits are in
        # the temporary project, to add its package and path dependency.
        result = subprocess.run(['cargo', 'check', '--offline', '--manifest-path', str(directory / 'Cargo.toml'), '--bins'],
                                env=env, text=True, capture_output=True)
        if result.returncode:
            print(result.stderr, file=sys.stderr)
            return [f'{snippet.path.relative_to(root)}:{snippet.line}: standalone Rust snippet {index} failed (see compiler output)'
                    for index, snippet in enumerate(snippets)]
    return []


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--compile', action='store_true', help='also compile standalone Rust fences; never run them')
    parser.add_argument('--rustdoc', type=Path, help='check Pages and versioned docs.rs API links against this generated rustdoc directory')
    args = parser.parse_args()
    found, snippets = check(ROOT)
    found.extend(readme_link_violations(ROOT))
    found.extend(sdk_alias_violations(ROOT))
    if args.rustdoc:
        found.extend(rustdoc_violations(ROOT, args.rustdoc))
        found.extend(parameter_name_violations(ROOT, args.rustdoc))
        found.extend(guide_api_name_violations(ROOT, args.rustdoc))
    if not found and args.compile:
        found.extend(compile_snippets(ROOT, snippets))
    for message in found:
        print(message, file=sys.stderr)
    if not found:
        print(f'Documentation checks passed ({len(documents(ROOT))} files, {len(snippets)} standalone Rust snippets'
              + (', compiled' if args.compile else ', compilation not requested')
              + (', generated rustdoc URLs checked' if args.rustdoc else '') + ').')
    return 1 if found else 0


if __name__ == '__main__':
    raise SystemExit(main())
