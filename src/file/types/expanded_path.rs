use std::path::{Path, PathBuf};

use crate::{Error, InputDefect, Result};

#[cfg(windows)]
mod windows;

pub(super) struct ExpandedPath {
    pub(super) bytes: usize,
    pub(super) path: Option<PathBuf>,
}

pub(super) fn absolute<'a>(
    pieces: impl Iterator<Item = &'a str> + Clone,
    keep: usize,
) -> Result<ExpandedPath> {
    let raw_bytes = pieces
        .clone()
        .try_fold(0usize, |total, piece| total.checked_add(piece.len()))
        .unwrap_or(usize::MAX);
    if raw_bytes > keep {
        #[cfg(windows)]
        return windows::expanded_absolute(pieces, raw_bytes, keep);
        #[cfg(not(windows))]
        return expanded_absolute(pieces, raw_bytes, keep);
    }

    let mut name = String::with_capacity(raw_bytes);
    pieces.for_each(|piece| name.push_str(piece));
    let path = PathBuf::from(name);
    let path = if path.is_absolute() {
        path
    } else {
        std::path::absolute(path)?
    };
    let bytes = path_text(&path)?.len();
    Ok(ExpandedPath {
        bytes,
        path: (bytes <= keep).then_some(path),
    })
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or(Error::InvalidInput(InputDefect::PathNotUtf8 {
            what: "recording file path",
        }))
}

#[cfg(not(windows))]
fn expanded_absolute<'a>(
    pieces: impl Iterator<Item = &'a str> + Clone,
    raw_bytes: usize,
    keep: usize,
) -> Result<ExpandedPath> {
    // The recorder uses an absolute pattern verbatim, including redundant
    // separators. Only relative patterns can shrink during resolution.
    if pieces
        .clone()
        .find(|piece| !piece.is_empty())
        .is_some_and(|first| first.starts_with('/'))
    {
        return Ok(ExpandedPath {
            bytes: raw_bytes,
            path: None,
        });
    }

    let current = std::env::current_dir()?;
    let current = path_text(&current)?;
    let mut bytes = current.len();
    relative_components(pieces.clone(), current, |part| {
        bytes = bytes.saturating_add(part.len());
    });
    let path = (bytes <= keep).then(|| {
        let mut text = String::with_capacity(bytes);
        text.push_str(current);
        relative_components(pieces, current, |part| text.push_str(part));
        PathBuf::from(text)
    });
    Ok(ExpandedPath { bytes, path })
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn relative_resolution_matches_std_across_piece_boundaries() {
        for raw in [
            ".",
            ".//",
            "././/./",
            "..//caméra/./.././",
            "./.filename/.//some//dir/..",
            "folder//./caméra_001.mkv",
        ] {
            let expected = std::path::absolute(raw).unwrap();
            let bytes = expected.to_str().unwrap().len();
            for (split, _) in raw.char_indices() {
                let (start, end) = raw.split_at(split);
                let pieces = [start, end].into_iter();
                let resolved = expanded_absolute(pieces.clone(), raw.len(), bytes).unwrap();
                assert_eq!(resolved.bytes, bytes, "{raw:?} split at {split}");
                assert_eq!(resolved.path.unwrap().as_os_str(), expected.as_os_str());
                let refused = expanded_absolute(pieces, raw.len(), bytes - 1).unwrap();
                assert_eq!(refused.bytes, bytes, "{raw:?} split at {split}");
                assert!(refused.path.is_none());
            }
        }
    }
}

/// Match `std::path::absolute` for relative Unix paths. Stream components
/// across piece boundaries, dropping `.` and redundant separators while
/// retaining `..` and a trailing separator.
#[cfg(not(windows))]
fn relative_components<'a>(
    pieces: impl Iterator<Item = &'a str>,
    current: &str,
    mut append: impl FnMut(&str),
) {
    enum Component {
        Start,
        Dot,
        Name,
    }

    let mut component = Component::Start;
    let mut needs_separator = !current.ends_with('/');
    let mut trailing_separator = false;
    for fragment in pieces.flat_map(|piece| piece.split_inclusive('/')) {
        trailing_separator = fragment.ends_with('/');
        let text = fragment.strip_suffix('/').unwrap_or(fragment);
        if !text.is_empty() {
            match component {
                Component::Name => append(text),
                Component::Start if text == "." => component = Component::Dot,
                Component::Start | Component::Dot => {
                    if needs_separator {
                        append("/");
                    }
                    if matches!(component, Component::Dot) {
                        append(".");
                    }
                    append(text);
                    needs_separator = true;
                    component = Component::Name;
                }
            }
        }
        if trailing_separator {
            component = Component::Start;
        }
    }
    if trailing_separator && needs_separator {
        append("/");
    }
}
