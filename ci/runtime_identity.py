"""Closed public evidence for the exact runtime selected by the native probe."""
from __future__ import annotations
import re


MARKER = 'NDI_RUNTIME_IDENTITY='
_NUMBER = r'(?:0|[1-9][0-9]{0,9})'
_IDENTITY = re.compile(re.escape(MARKER) + r'1\|(' + _NUMBER +
                       r'(?:\.' + _NUMBER + r'){3})\|(standard|advanced)\|(true|false)')


def validate_identity(identity, *, edition=None):
    """Reject raw text and arbitrary fields in a public runtime receipt."""
    if (not isinstance(identity, dict)
            or set(identity) != {'schema', 'version', 'edition', 'edition_verified'}
            or type(identity.get('schema')) is not int or identity['schema'] != 1
            or not isinstance(identity.get('version'), list) or len(identity['version']) != 4
            or any(type(value) is not int or not 0 <= value <= 0xffffffff
                   for value in identity['version'])
            or identity.get('edition') not in ('standard', 'advanced')
            or type(identity.get('edition_verified')) is not bool
            or (edition is not None and identity['edition'] != edition)):
        raise ValueError('runtime identity is missing, malformed or contradicts the selected edition')
    return identity


def parse_identity(output, *, edition=None):
    """Read one owned marker; never export other native stdout or stderr."""
    markers = [line for line in output.splitlines() if MARKER in line]
    if len(markers) != 1:
        raise ValueError('runtime identity requires exactly one marker')
    match = _IDENTITY.fullmatch(markers[0])
    if match is None:
        raise ValueError('runtime identity marker is malformed')
    return validate_identity({'schema': 1, 'version': [int(value) for value in match[1].split('.')],
                              'edition': match[2], 'edition_verified': match[3] == 'true'},
                             edition=edition)
