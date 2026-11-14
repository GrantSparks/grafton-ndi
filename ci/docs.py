#!/usr/bin/env python3
"""Check repository documentation without fetching remote pages.

Local links and same-version GitHub links are checked against the checkout;
immutable historical GitHub links are checked against local Git objects when
available. External pages are outside this check. With --compile, standalone
Rust fences are compiled (never run) against the current crate and locked
versions of its dependencies. Use a linked runnable example for code fragments.
With --rustdoc, current-version docs.rs URLs are checked against generated HTML.
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
FENCE = re.compile(r'^ {0,3}(`{3,}|~{3,})([^\n]*)$')
HEADING = re.compile(r'^ {0,3}#{1,6}\s+(.*?)\s*#*\s*$')


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
        if url.startswith(REPOSITORY + '/'):
            parts = urlsplit(url).path.split('/')
            if (len(parts) >= 6 and parts[3] in ('blob', 'tree')
                    and parts[4] in ('main', 'master', 'HEAD')):
                relative = '/'.join(parts[5:])
                if relative.startswith('docs/') or relative in ('README.md', 'examples/README.md'):
                    found.append(f'{label}: guide link must name a release or immutable commit, not a moving branch: {url}')
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
        if ref in (release, 'v' + release):
            return root / relative, fragment, None
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
            # Existing historical objects are authoritative. No network access
            # or assumption that a release tag already exists is needed.
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
    release = version(root)
    found, snippets = [], []
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

    def handle_starttag(self, tag, attrs):
        for name, value in attrs:
            if value and (name == 'id' or (tag == 'a' and name == 'name')):
                self.found.add(value)


def rustdoc_violations(root, directory):
    """Check current-version docs.rs destinations against generated rustdoc."""
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
            if parsed.netloc != 'docs.rs' or parts[:2] != ['grafton-ndi', release]:
                continue
            relative = unquote('/'.join(parts[2:])) or 'grafton_ndi'
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
    parser.add_argument('--rustdoc', type=Path, help='check versioned docs.rs links against this generated rustdoc directory')
    args = parser.parse_args()
    found, snippets = check(ROOT)
    if args.rustdoc:
        found.extend(rustdoc_violations(ROOT, args.rustdoc))
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
