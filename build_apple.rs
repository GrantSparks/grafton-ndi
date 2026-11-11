//! Validate the selected Apple static slice without loading or executing it.
use std::{collections::HashSet, fs, ops::Range, path::Path};

#[derive(Debug)]
pub struct StaticArchive {
    pub deployment_floor: u32,
    pub symbols: HashSet<String>,
    bytes: Vec<u8>,
}

impl StaticArchive {
    /// Materialize exactly the ordinary archive whose objects were validated.
    /// Rust's static bundling accepts an ar archive, not a universal fat wrapper.
    pub fn write_thin(&self, destination: &Path) -> Result<(), String> {
        fs::write(destination, &self.bytes)
            .map_err(|e| format!("Write {}: {e}", destination.display()))
    }
}

fn u32_at(data: &[u8], offset: usize, big: bool) -> Result<u32, String> {
    let v: [u8; 4] = data
        .get(offset..offset.saturating_add(4))
        .ok_or("Truncated Apple artifact")?
        .try_into()
        .map_err(|_| "Invalid integer")?;
    Ok(if big {
        u32::from_be_bytes(v)
    } else {
        u32::from_le_bytes(v)
    })
}

fn u64_at(data: &[u8], offset: usize, big: bool) -> Result<u64, String> {
    let v: [u8; 8] = data
        .get(offset..offset.saturating_add(8))
        .ok_or("Truncated Apple artifact")?
        .try_into()
        .map_err(|_| "Invalid integer")?;
    Ok(if big {
        u64::from_be_bytes(v)
    } else {
        u64::from_le_bytes(v)
    })
}

fn archive_range(data: &[u8], cpu: u32) -> Result<Range<usize>, String> {
    if data.starts_with(b"!<arch>\n") {
        return Ok(0..data.len());
    }
    let (big, wide) = match u32_at(data, 0, true)? {
        0xcafe_babe => (true, false),
        0xbeba_feca => (false, false),
        0xcafe_babf => (true, true),
        0xbfba_feca => (false, true),
        _ => return Err("Expected an Apple fat or archive artifact".into()),
    };
    let count = u32_at(data, 4, big)? as usize;
    let stride = if wide { 32 } else { 20 };
    let table_end = count
        .checked_mul(stride)
        .and_then(|size| size.checked_add(8))
        .filter(|&end| end <= data.len())
        .ok_or("Truncated or overflowing fat architecture table")?;
    let mut found = None;
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for index in 0..count {
        let base = 8 + index * stride; // bounded by the complete table above
        let (start, size, align) = if wide {
            if u32_at(data, base + 28, big)? != 0 {
                return Err("Invalid reserved fat64 architecture field".into());
            }
            (
                u64_at(data, base + 8, big)?,
                u64_at(data, base + 16, big)?,
                u32_at(data, base + 24, big)?,
            )
        } else {
            (
                u64::from(u32_at(data, base + 8, big)?),
                u64::from(u32_at(data, base + 12, big)?),
                u32_at(data, base + 16, big)?,
            )
        };
        let alignment = 1u64
            .checked_shl(align)
            .ok_or("Invalid fat slice alignment")?;
        if start % alignment != 0 {
            return Err("Misaligned fat archive slice".into());
        }
        let start = usize::try_from(start).map_err(|_| "Fat slice offset overflow")?;
        let size = usize::try_from(size).map_err(|_| "Fat slice size overflow")?;
        let end = start.checked_add(size).ok_or("Fat slice overflow")?;
        if start < table_end || size < 8 || end > data.len() {
            return Err("Fat slice overlaps its table or lies outside the artifact".into());
        }
        let range = start..end;
        ranges.push(range.clone());
        if u32_at(data, base, big)? == cpu {
            if found.is_some() {
                return Err("Ambiguous duplicate Apple CPU slices".into());
            }
            found = Some(range);
        }
    }
    ranges.sort_unstable_by_key(|range| range.start);
    if ranges.windows(2).any(|pair| pair[0].end > pair[1].start) {
        return Err("Overlapping fat archive slices".into());
    }
    found.ok_or_else(|| "Required Apple CPU slice is missing".into())
}

/// Return the largest deployment floor declared by objects in this slice.
pub fn inspect(path: &Path, arch: &str, os: &str) -> Result<StaticArchive, String> {
    let data = fs::read(path).map_err(|e| format!("Read {}: {e}", path.display()))?;
    let cpu = match arch {
        "aarch64" => 0x0100_000c,
        "x86_64" => 0x0100_0007,
        _ => return Err("Unsupported Apple CPU".into()),
    };
    let platform = match (os, arch) {
        ("ios", "aarch64") => 2,
        ("ios", "x86_64") => 7,
        ("tvos", "aarch64") => 3,
        ("tvos", "x86_64") => 8,
        ("visionos", "aarch64") => 11,
        _ => return Err("No supplied Apple platform/simulator slice".into()),
    };
    let archive = &data[archive_range(&data, cpu)?];
    if !archive.starts_with(b"!<arch>\n") {
        return Err("Apple slice is not a static archive".into());
    }
    let mut offset = 8usize;
    let mut floor = 0;
    let mut platform_objects = 0;
    let mut symbols = HashSet::new();
    while offset < archive.len() {
        let header = archive
            .get(offset..offset.saturating_add(60))
            .ok_or("Truncated archive header")?;
        if &header[58..60] != b"`\n" {
            return Err("Invalid archive member header".into());
        }
        let length = std::str::from_utf8(&header[48..58])
            .map_err(|_| "Invalid archive length")?
            .trim()
            .parse::<usize>()
            .map_err(|_| "Invalid archive length")?;
        let start = offset.checked_add(60).ok_or("Archive offset overflow")?;
        let end = start.checked_add(length).ok_or("Archive size overflow")?;
        let member = archive.get(start..end).ok_or("Truncated archive member")?;
        let name = std::str::from_utf8(&header[..16])
            .map_err(|_| "Invalid archive member name")?
            .trim();
        let skip = if let Some(length) = name.strip_prefix("#1/") {
            length
                .parse::<usize>()
                .map_err(|_| "Invalid extended archive name")?
        } else {
            0
        };
        let object = member
            .get(skip..)
            .ok_or("Truncated extended archive name")?;
        if object.starts_with(&0xfeed_facfu32.to_le_bytes()) {
            if u32_at(object, 4, false)? != cpu {
                return Err("Mismatched object CPU inside Apple archive slice".into());
            }
            let commands = u32_at(object, 16, false)? as usize;
            let mut at = 32usize;
            let mut saw_platform = false;
            for _ in 0..commands {
                let cmd = u32_at(object, at, false)?;
                let size = u32_at(object, at + 4, false)? as usize;
                if size < 8 {
                    return Err("Invalid Mach-O load command".into());
                }
                let end = at.checked_add(size).ok_or("Load command overflow")?;
                if end > object.len() {
                    return Err("Truncated Mach-O load command".into());
                }
                if cmd == 0x2 {
                    let symoff = u32_at(object, at + 8, false)? as usize;
                    let nsyms = u32_at(object, at + 12, false)? as usize;
                    let stroff = u32_at(object, at + 16, false)? as usize;
                    let strsize = u32_at(object, at + 20, false)? as usize;
                    let strings = object
                        .get(stroff..stroff.checked_add(strsize).ok_or("String table overflow")?)
                        .ok_or("Truncated Mach-O string table")?;
                    for index in 0..nsyms {
                        let symbol = symoff
                            .checked_add(index.checked_mul(16).ok_or("Symbol table overflow")?)
                            .ok_or("Symbol table overflow")?;
                        let entry = object
                            .get(symbol..symbol.checked_add(16).ok_or("Symbol entry overflow")?)
                            .ok_or("Truncated Mach-O symbol")?;
                        let kind = entry[4];
                        if kind & 0xe0 != 0 || kind & 1 == 0 || kind & 0x0e == 0 {
                            continue;
                        }
                        let string = u32_at(entry, 0, false)? as usize;
                        let bytes = strings.get(string..).ok_or("Invalid Mach-O symbol name")?;
                        let length = bytes
                            .iter()
                            .position(|&b| b == 0)
                            .ok_or("Unterminated Mach-O symbol")?;
                        if let Ok(name) = std::str::from_utf8(&bytes[..length]) {
                            if let Some(name) =
                                name.strip_prefix('_').filter(|n| n.starts_with("NDIlib_"))
                            {
                                symbols.insert(name.to_owned());
                            }
                        }
                    }
                } else if cmd == 0x32 {
                    if u32_at(object, at + 8, false)? != platform {
                        return Err("Apple device/simulator platform does not match target (CPU alone is insufficient)".into());
                    }
                    floor = floor.max(u32_at(object, at + 12, false)?);
                    saw_platform = true;
                } else if matches!(cmd, 0x25 | 0x2f) {
                    let legacy_os = if cmd == 0x25 { "ios" } else { "tvos" };
                    if legacy_os != os {
                        return Err("Legacy Apple platform does not match target".into());
                    }
                    floor = floor.max(u32_at(object, at + 8, false)?);
                    saw_platform = true;
                }
                at = end;
            }
            if saw_platform {
                platform_objects += 1;
            }
        }
        offset = end
            .checked_add(length % 2)
            .ok_or("Archive padding overflow")?;
    }
    if platform_objects == 0 {
        return Err("No verifiable platform objects in Apple archive".into());
    }
    Ok(StaticArchive {
        deployment_floor: floor,
        symbols,
        bytes: archive.to_vec(),
    })
}

#[allow(dead_code)]
pub fn validate(path: &Path, arch: &str, os: &str) -> Result<u32, String> {
    inspect(path, arch, os).map(|a| a.deployment_floor)
}
