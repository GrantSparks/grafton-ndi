"""Read the workflow files for the tests that guard them.

The guards need the structure of a workflow, and the standard library has no YAML
parser. `load` reads the subset of YAML these workflows use: block mappings and
sequences, plain and quoted scalars, flow sequences of scalars, literal and folded
block scalars, and comments. Anything else (anchors, aliases, tags, flow mappings,
multi-line plain scalars, duplicate keys, tabs) raises `Unsupported`, so a construct
the guards cannot see through fails the tests instead of slipping past them.
Whether the files are valid workflows is checked by actionlint in the same job.
"""
import re

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
