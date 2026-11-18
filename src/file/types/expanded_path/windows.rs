use super::ExpandedPath;
use crate::{Error, InputDefect, Result};
use std::{io, path::PathBuf, ptr};
use windows_sys::Win32::{
    Foundation::ERROR_FILENAME_EXCED_RANGE, Storage::FileSystem::GetFullPathNameW,
};

/// Windows path buffers hold at most 32,767 UTF-16 units including the NUL.
const NATIVE_PATH_BUFFER: usize = i16::MAX as usize;

pub(super) fn expanded_absolute<'a>(
    pieces: impl Iterator<Item = &'a str> + Clone,
    raw_bytes: usize,
    keep: usize,
) -> Result<ExpandedPath> {
    if is_absolute(pieces.clone()) {
        return Ok(ExpandedPath {
            bytes: raw_bytes,
            path: None,
        });
    }

    let units = pieces
        .clone()
        .try_fold(0usize, |len, part| {
            len.checked_add(part.encode_utf16().count())
                .filter(|&len| len < NATIVE_PATH_BUFFER)
        })
        .ok_or_else(native_length_error)?;
    resolve_relative(pieces, units, keep)
}

/// Like Path::is_absolute on Windows. Inspect only what decides whether the
/// path has a rooted drive or a namespace prefix, including both UNC names.
fn is_absolute<'a>(pieces: impl Iterator<Item = &'a str>) -> bool {
    let mut bytes = pieces.flat_map(str::bytes);
    let (Some(first), Some(second)) = (bytes.next(), bytes.next()) else {
        return false;
    };
    if first.is_ascii_alphabetic() && second == b':' {
        return bytes.next().is_some_and(separator);
    }
    if !separator(first) || !separator(second) {
        return false;
    }
    let Some(third) = bytes.next() else {
        return false;
    };
    if separator(third) {
        return false;
    }
    let fourth = bytes.next();
    if (third == b'.' && fourth.is_some_and(separator))
        || [first, second, third, fourth.unwrap_or_default()] == *br"\\?\"
    {
        return true;
    }
    (fourth.is_some_and(separator) || bytes.any(separator))
        && bytes.next().is_some_and(|byte| !separator(byte))
}

fn separator(byte: u8) -> bool {
    matches!(byte, b'\\' | b'/')
}

fn native_length_error() -> io::Error {
    io::Error::from_raw_os_error(ERROR_FILENAME_EXCED_RANGE as i32)
}

fn resolve_relative<'a>(
    pieces: impl Iterator<Item = &'a str>,
    units: usize,
    keep: usize,
) -> Result<ExpandedPath> {
    // Native UTF-16 path resolution can remove path components and trim the
    // filename. Keep its temporary buffers within the Win32 path bound and
    // allocate the UTF-8 filename only after measuring the resolved path.
    let mut input = Vec::with_capacity(units + 1);
    input.extend(pieces.flat_map(str::encode_utf16));
    input.push(0);
    let mut output = vec![0; 512];
    let len = loop {
        // SAFETY: input is NUL terminated and output contains the specified
        // number of writable u16 elements. We request no file-part pointer.
        let len = unsafe {
            GetFullPathNameW(
                input.as_ptr(),
                output.len() as u32,
                output.as_mut_ptr(),
                ptr::null_mut(),
            )
        } as usize;
        if len == 0 {
            return Err(io::Error::last_os_error().into());
        }
        if len < output.len() {
            break len;
        }
        if len > NATIVE_PATH_BUFFER || len == output.len() {
            return Err(native_length_error().into());
        }
        output.resize(len, 0);
    };
    let chars = char::decode_utf16(output[..len].iter().copied());
    let bytes = chars
        .clone()
        .try_fold(0, |bytes, ch| ch.map(|ch| bytes + ch.len_utf8()))
        .map_err(|_| {
            Error::InvalidInput(InputDefect::PathNotUtf8 {
                what: "recording file path",
            })
        })?;
    let path = (bytes <= keep).then(|| {
        let mut path = String::with_capacity(bytes);
        path.extend(chars.filter_map(std::result::Result::ok));
        PathBuf::from(path)
    });
    Ok(ExpandedPath { bytes, path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn streamed_absolute_prefix_matches_std() {
        for text in [
            "take.mkv",
            "C:take.mkv",
            "C:/take.mkv",
            r"\take.mkv",
            r"\\server",
            r"\\server\share",
            r"\\server\\share",
            r"\\.\NUL",
            r"\\?\C:",
            r"\\?\C:/take.mkv",
            r"\\?\C:\take.mkv",
            r"\\?\UNC\server\share",
            "//?/C:/take.mkv",
            "//.\\NUL",
            "//server/share",
        ] {
            for split in 0..=text.len() {
                let (first, second) = text.split_at(split);
                assert_eq!(
                    is_absolute([first, "", second].into_iter()),
                    Path::new(text).is_absolute(),
                    "{text:?} split at {split}"
                );
            }
        }
    }

    #[test]
    fn streamed_relative_paths_use_native_normalization() {
        for (first, tail) in [
            ("C:folder", r"\..\føldér\.\take.mkv . "),
            (r"\directory", r"\\folder\..\take.mkv  "),
            ("CON1.", "ext"),
            ("NUL.", "ext"),
            ("directory//", "take.mkv"),
        ] {
            let text = format!("{first}{tail}");
            let baseline = std::path::absolute(text.as_str()).unwrap();
            let len = baseline.to_str().unwrap().len();
            for keep in [0, len] {
                let result =
                    expanded_absolute([first, tail].into_iter(), text.len(), keep).unwrap();
                assert_eq!(result.bytes, len, "{text:?}");
                assert_eq!(result.path, (len <= keep).then_some(baseline.clone()));
            }
        }
    }

    #[test]
    fn oversized_rendering_can_shrink_below_the_event_limit() {
        let parent = r"folder\..\".repeat(512);
        for filename in ["take.mkv", "NUL.ext", "føldér/take.mkv . "] {
            let text = format!("{parent}{filename}");
            let baseline = std::path::absolute(text.as_str()).unwrap();
            let result =
                expanded_absolute([parent.as_str(), filename].into_iter(), text.len(), 4094)
                    .unwrap();
            assert_eq!(result.bytes, baseline.to_str().unwrap().len());
            assert_eq!(result.path, Some(baseline));
        }
    }
}
