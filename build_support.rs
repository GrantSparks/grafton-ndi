//! SDK discovery and static-archive selection, shared by the build script and its tests.
//!
//! Dynamically loaded targets need none of this to build: an SDK found here only
//! chooses the default runtime edition and, with the `advanced` feature, supplies
//! the Advanced-only headers. Statically linked Apple targets link its archive.

use std::{
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
};

/// The header only an Advanced SDK installation contains.
const ADVANCED_HEADER: &str = "Processing.NDI.Advanced.h";

/// The umbrella header every SDK installation contains.
const UMBRELLA_HEADER: &str = "Processing.NDI.Lib.h";

/// The licence grant carried by every header that may be redistributed.
#[cfg(feature = "advanced")]
const MIT_GRANT: &str = "Permission is hereby granted, free of charge";

/// An SDK edition, as named by `NDI_SDK_EDITION` and the crate's runtime options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edition {
    Standard,
    Advanced,
}

impl Edition {
    /// Parse the value of `NDI_SDK_EDITION`.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "standard" => Ok(Self::Standard),
            "advanced" => Ok(Self::Advanced),
            _ => Err(format!(
                "Invalid NDI_SDK_EDITION {value:?}; expected standard or advanced"
            )),
        }
    }

    /// The spelling embedded into the crate.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Advanced => "advanced",
        }
    }
}

/// Find the one directory named `lowercase` or `uppercase` under `root`.
///
/// Casing belongs to the installation layout, not to the target operating
/// system. Probing both spellings with `is_dir()` finds the same entry twice on
/// a case-insensitive filesystem, so names are enumerated and the returned path
/// uses the installation's own spelling on every host.
pub fn directory(root: &Path, lowercase: &str, uppercase: &str) -> Result<PathBuf, String> {
    let read_error = |error| format!("Cannot read SDK directory {}: {error}", root.display());
    let mut directories = Vec::new();
    for entry in std::fs::read_dir(root).map_err(read_error)? {
        let entry = entry.map_err(read_error)?;
        if entry.file_name().to_str().is_some_and(|name| {
            name.eq_ignore_ascii_case(lowercase) || name.eq_ignore_ascii_case(uppercase)
        }) && entry.path().is_dir()
        {
            directories.push(entry.path());
        }
    }
    // Prefer the lowercase spelling when genuine symlink aliases coexist.
    let preference = |path: &PathBuf| match path.file_name().and_then(|name| name.to_str()) {
        Some(name) if name == lowercase => 0,
        Some(name) if name == uppercase => 1,
        _ => 2,
    };
    directories.sort_by(|a, b| preference(a).cmp(&preference(b)).then_with(|| a.cmp(b)));
    let Some(selected) = directories.first() else {
        return Err(format!(
            "SDK directory missing: {} or {}",
            root.join(lowercase).display(),
            root.join(uppercase).display()
        ));
    };
    for other in directories.iter().skip(1) {
        if !matches!((selected.canonicalize(), other.canonicalize()), (Ok(a), Ok(b)) if a == b) {
            return Err(format!(
                "Ambiguous SDK layout: both {} and {} exist",
                selected.display(),
                other.display()
            ));
        }
    }
    Ok(selected.clone())
}

/// The roots the vendor installers use on `os`, in order of preference.
pub fn default_sdk_roots(os: &str) -> &'static [&'static str] {
    match os {
        "macos" => &[
            "/Library/NDI SDK for macOS",
            "/Library/NDI SDK for Apple",
            "/Library/NDI Advanced SDK for Apple",
            "/Library/NDI 6 SDK",
            "/Library/NDI SDK",
            "/Library/NewTek/NDI SDK",
            "/Library/Application Support/NDI SDK for Apple",
            "/Applications/NDI SDK for Apple",
            "/Applications/NDI 6 SDK",
        ],
        "linux" => &[
            "/usr/share/NDI Advanced SDK for Linux",
            "/usr/share/NDI SDK for Linux",
        ],
        "windows" => &[
            r"C:\Program Files\NDI\NDI 6 SDK",
            r"C:\Program Files\NDI\NDI 6 Advanced SDK",
        ],
        _ => &[],
    }
}

/// An SDK installation found at build time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sdk {
    pub root: PathBuf,
    pub include_dir: PathBuf,
}

impl Sdk {
    /// Open the installation at `root`.
    pub fn open(root: &Path) -> Result<Self, String> {
        let include_dir = directory(root, "include", "Include")?;
        if !include_dir.join(UMBRELLA_HEADER).is_file() {
            return Err(format!(
                "{UMBRELLA_HEADER} not found in {}",
                include_dir.display()
            ));
        }
        Ok(Self {
            root: root.to_owned(),
            include_dir,
        })
    }

    /// Find the SDK to consult: `explicit` (the value of `NDI_SDK_DIR`) when
    /// given, otherwise the first usable default installation for `os`.
    ///
    /// An explicit root that is not an SDK is an error; finding none is not.
    pub fn locate(explicit: Option<&Path>, os: &str) -> Result<Option<Self>, String> {
        match explicit {
            Some(root) => Self::open(root)
                .map(Some)
                .map_err(|error| format!("NDI_SDK_DIR is not an NDI SDK installation: {error}")),
            None => Ok(default_sdk_roots(os)
                .iter()
                .find_map(|root| Self::open(Path::new(root)).ok())),
        }
    }

    /// Whether this is an Advanced SDK installation.
    pub fn is_advanced(&self) -> bool {
        self.include_dir.join(ADVANCED_HEADER).is_file()
    }

    /// The installation's own edition.
    pub fn edition(&self) -> Edition {
        if self.is_advanced() {
            Edition::Advanced
        } else {
            Edition::Standard
        }
    }

    /// File names of the headers without the MIT grant. Their declarations may
    /// be generated for the local build but never checked in.
    #[cfg(feature = "advanced")]
    pub fn restricted_headers(&self) -> Result<Vec<String>, String> {
        let read_error = |error| format!("Cannot read {}: {error}", self.include_dir.display());
        let mut headers = Vec::new();
        for entry in std::fs::read_dir(&self.include_dir).map_err(read_error)? {
            let path = entry.map_err(read_error)?.path();
            if path.extension().is_none_or(|extension| extension != "h") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|error| format!("Cannot read {}: {error}", path.display()))?;
            if !text.contains(MIT_GRANT) {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| format!("Unsupported SDK header name {}", path.display()))?;
                headers.push(name.to_owned());
            }
        }
        headers.sort();
        Ok(headers)
    }
}

/// The runtime edition the crate searches for by default.
///
/// An explicit request wins; otherwise the located installation decides, and
/// with neither the Standard runtime is assumed.
pub fn default_edition(requested: Option<Edition>, sdk: Option<&Sdk>) -> Edition {
    requested
        .or_else(|| sdk.map(Sdk::edition))
        .unwrap_or(Edition::Standard)
}

/// Whether `os` links the SDK into the application instead of loading it.
pub fn links_statically(os: &str) -> bool {
    matches!(os, "ios" | "tvos" | "visionos")
}

/// The vendor archive a statically linked target uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaticLibrary {
    pub archive: PathBuf,
    pub link_name: String,
    pub edition: Edition,
}

/// Select the one archive to link for a static Apple target.
///
/// A mobile File archive already contains the core, so it replaces the core
/// archive instead of being linked beside it.
pub fn static_library(
    sdk: &Sdk,
    os: &str,
    requested: Option<Edition>,
    file_io: bool,
) -> Result<StaticLibrary, String> {
    let (subdirectory, platform) = match os {
        "ios" => ("iOS", "ios"),
        "tvos" => ("tvOS", "tvos"),
        "visionos" => ("visionOS", "visionos"),
        _ => return Err(format!("{os} does not link the NDI SDK statically")),
    };
    let directory = directory(&sdk.root, "lib", "Lib")?.join(subdirectory);
    let archive = |name: &str| directory.join(format!("lib{name}.a"));
    let (edition, core) = [
        (Edition::Advanced, format!("ndi_advanced_{platform}")),
        (Edition::Standard, format!("ndi_{platform}")),
    ]
    .into_iter()
    .filter(|(edition, _)| requested.is_none_or(|requested| requested == *edition))
    .find(|(_, name)| archive(name).is_file())
    .ok_or_else(|| {
        format!(
            "No NDI static library for {os} in {}; set NDI_SDK_DIR to a complete Apple SDK",
            directory.display()
        )
    })?;
    let file = format!("ndifile_{platform}");
    let link_name = if file_io && archive(&file).is_file() {
        file
    } else {
        core
    };
    Ok(StaticLibrary {
        archive: archive(&link_name),
        link_name,
        edition,
    })
}

/// Names of the `NDIlib_*` foreign functions bindgen declared in `bindings`.
///
/// The bindings are always formatted with one `pub fn NAME(` per declaration.
pub fn declared_functions(bindings: &str) -> BTreeSet<&str> {
    bindings
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("pub fn "))
        .filter_map(|declaration| declaration.split_once('('))
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("NDIlib_"))
        .collect()
}

/// Rust source for `linked_address`, the static provider's name-to-address map.
///
/// `declarations` pairs a module path with that module's bindgen output. Only
/// functions the linked archive actually defines are referenced, so an older
/// archive never produces an unresolved symbol.
pub fn static_symbol_map(
    declarations: &[(&str, &str)],
    exports: &HashSet<String>,
) -> Result<String, String> {
    let mut mapped = BTreeSet::new();
    let mut arms = String::new();
    for (module, bindings) in declarations {
        for name in declared_functions(bindings) {
            if exports.contains(name) && mapped.insert(name) {
                arms.push_str(&format!(
                    "        b\"{name}\" => Some({module}::{name} as *const () as *const c_void),\n"
                ));
            }
        }
    }
    if !mapped.contains("NDIlib_initialize") {
        return Err("the static NDI archive does not define NDIlib_initialize".into());
    }
    Ok(format!(
        "fn linked_address(name: &[u8]) -> Option<*const c_void> {{\n    match name {{\n{arms}        _ => None,\n    }}\n}}\n"
    ))
}
