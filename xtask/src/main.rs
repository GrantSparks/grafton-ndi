//! Maintenance tasks for the grafton-ndi workspace.
//!
//! `cargo xtask bindgen --sdk <SDK root>` regenerates the vendored bindings in `src/sys`
//! from the MIT-licensed headers of a Standard NDI SDK: `bindings.rs` holds the types
//! and constants, `declarations.rs` the foreign functions. `--check` verifies the
//! checked-in files instead of writing them. Both modes prove that one pair of files
//! is correct for every target.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

/// Umbrella headers whose declarations are vendored.
const UMBRELLA_HEADERS: [&str; 2] = ["Processing.NDI.Lib.h", "Processing.NDI.Lib.File.h"];

/// Text every header must carry before its declarations may be redistributed.
const MIT_GRANT: &str = "Permission is hereby granted, free of charge";

/// Target the vendored file is generated for.
const PRIMARY_TARGET: &str = "x86_64-unknown-linux-gnu";

/// The edition rustfmt formats the vendored files for: the crate's own, so they
/// pass the same `cargo fmt` check as hand-written code.
const CRATE_EDITION: &str = "2021";

/// Targets whose bindings must equal the vendored file, C-enum signedness aside.
const EQUIVALENCE_TARGETS: [&str; 10] = [
    "i686-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "armv7-unknown-linux-gnueabihf",
    "x86_64-pc-windows-msvc",
    "i686-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "aarch64-apple-ios",
    "aarch64-linux-android",
];

/// Constants a header declares for a single platform. The crate exposes none of
/// them, so they are left out of the vendored file rather than made conditional.
const PLATFORM_ONLY_CONSTANTS: [&str; 1] =
    ["NDIlib_recv_color_format_e_NDIlib_recv_color_format_BGRX_BGRA_flipped"];

const USAGE: &str = "usage: cargo xtask bindgen --sdk <NDI SDK root> [--check]";

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: Vec<String>) -> Result<String, String> {
    let mut arguments = arguments.into_iter();
    if arguments.next().as_deref() != Some("bindgen") {
        return Err(USAGE.into());
    }
    let mut sdk = None;
    let mut check = false;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--sdk" => sdk = Some(PathBuf::from(arguments.next().ok_or(USAGE)?)),
            "--check" => check = true,
            _ => return Err(USAGE.into()),
        }
    }
    let sdk = Sdk::open(&sdk.ok_or(USAGE)?)?;
    let shim = Shim::create()?;
    let mut report = Vec::new();
    for part in [Part::Types, Part::Functions] {
        let bindings = sdk.generate(part, PRIMARY_TARGET, &shim)?;
        for target in EQUIVALENCE_TARGETS {
            equivalent(&bindings, &sdk.generate(part, target, &shim)?).map_err(|difference| {
                format!("{} for {target} differ: {difference}", part.file_name())
            })?;
        }
        let vendored = shim.rustfmt(&sdk.vendored_source(part, &bindings)?)?;
        let destination = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../src/sys")
            .join(part.file_name());
        if check {
            let current = fs::read_to_string(&destination)
                .map_err(|error| format!("cannot read {}: {error}", destination.display()))?;
            if current != vendored {
                return Err(format!(
                    "{} is stale; run `cargo xtask bindgen --sdk <NDI SDK root>`",
                    destination.display()
                ));
            }
            report.push(format!(
                "{} is current and identical for {} targets",
                destination.display(),
                EQUIVALENCE_TARGETS.len() + 1
            ));
        } else {
            fs::write(&destination, vendored)
                .map_err(|error| format!("cannot write {}: {error}", destination.display()))?;
            report.push(format!("wrote {}", destination.display()));
        }
    }
    Ok(report.join("\n"))
}

/// One of the two vendored files.
///
/// Functions are kept apart from types so that code importing the types cannot
/// call a foreign declaration directly; every call goes through a loaded table.
#[derive(Clone, Copy)]
enum Part {
    Types,
    Functions,
}

impl Part {
    fn file_name(self) -> &'static str {
        match self {
            Self::Types => "bindings.rs",
            Self::Functions => "declarations.rs",
        }
    }

    fn codegen(self) -> bindgen::CodegenConfig {
        match self {
            Self::Types => bindgen::CodegenConfig::all() - bindgen::CodegenConfig::FUNCTIONS,
            Self::Functions => bindgen::CodegenConfig::FUNCTIONS,
        }
    }

    /// Rust the file needs before the generated items.
    fn prelude(self) -> &'static str {
        match self {
            Self::Types => "",
            Self::Functions => "use super::bindings::*;\n\n",
        }
    }
}

/// An installed SDK whose redistributable headers have been identified.
struct Sdk {
    include: PathBuf,
    version: String,
    notice: String,
    /// Header file names carrying the MIT grant.
    licensed: Vec<String>,
    /// Header file names without it; nothing declared there may be emitted.
    unlicensed: Vec<String>,
}

impl Sdk {
    fn open(root: &Path) -> Result<Self, String> {
        let include = ["include", "Include"]
            .into_iter()
            .map(|name| root.join(name))
            .find(|path| path.is_dir())
            .ok_or_else(|| format!("{} has no include directory", root.display()))?;
        let mut licensed = Vec::new();
        let mut unlicensed = Vec::new();
        let entries = fs::read_dir(&include)
            .map_err(|error| format!("cannot read {}: {error}", include.display()))?;
        for entry in entries {
            let path = entry.map_err(|error| error.to_string())?.path();
            if path.extension().is_none_or(|extension| extension != "h") {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| format!("unsupported header name {}", path.display()))?
                .to_owned();
            let text = fs::read_to_string(&path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            if text.contains(MIT_GRANT) {
                licensed.push(name);
            } else {
                unlicensed.push(name);
            }
        }
        licensed.sort();
        unlicensed.sort();
        for umbrella in UMBRELLA_HEADERS {
            if !licensed.iter().any(|name| name == umbrella) {
                return Err(format!(
                    "{umbrella} is missing from {} or lacks the MIT notice",
                    include.display()
                ));
            }
        }
        let umbrella = fs::read_to_string(include.join(UMBRELLA_HEADERS[0]))
            .map_err(|error| error.to_string())?;
        let version = fs::read_to_string(root.join("Version.txt"))
            .map_err(|error| format!("cannot read the SDK Version.txt: {error}"))?
            .split_whitespace()
            .last()
            .ok_or("the SDK Version.txt is empty")?
            .to_owned();
        Ok(Self {
            notice: license_notice(&umbrella)?,
            include,
            version,
            licensed,
            unlicensed,
        })
    }

    /// The complete text of a vendored file around its generated `bindings`.
    fn vendored_source(&self, part: Part, bindings: &str) -> Result<String, String> {
        Ok(format!(
            "// @generated by `cargo xtask bindgen` from the NDI SDK {} headers with bindgen {}.\n\
             // Do not edit. One file serves every target: C enum types use the non-MSVC\n\
             // `unsigned int` form, which has the same size and calling convention as MSVC's `int`.\n\
             //\n\
             // The declarations derive from SDK headers distributed under this notice:\n\
             //\n\
             {}\n\
             {}{bindings}",
            self.version,
            bindgen_version()?,
            self.notice,
            part.prelude()
        ))
    }

    /// One part of the licensed headers as bindgen renders it for `target`.
    fn generate(&self, part: Part, target: &str, shim: &Shim) -> Result<String, String> {
        let mut builder = bindgen::Builder::default()
            .header_contents(
                "wrapper.h",
                &UMBRELLA_HEADERS
                    .iter()
                    .map(|header| format!("#include <{header}>\n"))
                    .collect::<String>(),
            )
            .clang_arg(format!("--target={target}"))
            // Only the compiler's own freestanding headers are visible, so the
            // result does not depend on the host or an installed target sysroot.
            .clang_args(["-ffreestanding", "-nostdlibinc"])
            .clang_arg(format!("-I{}", shim.directory.display()))
            .clang_arg(format!("-I{}", self.include.display()))
            .clang_arg("-DPROCESSINGNDILIBFILE_NO_DYNAMIC_LOAD_HELPERS")
            .with_codegen_config(part.codegen())
            .derive_default(true)
            .layout_tests(false)
            .formatter(bindgen::Formatter::Prettyplease)
            // Library file names are target-specific and belong to the loader.
            .blocklist_item("NDILIB_.*");
        for header in &self.licensed {
            builder = builder.allowlist_file(header_pattern(header));
        }
        for header in &self.unlicensed {
            builder = builder.blocklist_file(header_pattern(header));
        }
        builder
            .generate()
            .map(|bindings| bindings.to_string())
            .map_err(|error| format!("bindgen failed for {target}: {error}"))
    }
}

/// A scratch directory holding empty stand-ins for hosted C headers the SDK
/// includes but does not use.
struct Shim {
    directory: PathBuf,
}

impl Shim {
    fn create() -> Result<Self, String> {
        let directory = env::temp_dir().join(format!("grafton-ndi-xtask-{}", std::process::id()));
        fs::create_dir_all(&directory)
            .and_then(|()| fs::write(directory.join("string.h"), ""))
            .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
        Ok(Self { directory })
    }

    /// `source` as rustfmt renders it.
    ///
    /// The vendored files are ordinary sources to every formatting check,
    /// including ones that hand a changed file straight to rustfmt, so they
    /// are stored already formatted.
    fn rustfmt(&self, source: &str) -> Result<String, String> {
        let file = self.directory.join("vendored.rs");
        fs::write(&file, source)
            .map_err(|error| format!("cannot write {}: {error}", file.display()))?;
        let rustfmt = env::var_os("RUSTFMT").unwrap_or_else(|| "rustfmt".into());
        let status = Command::new(&rustfmt)
            .args(["--edition", CRATE_EDITION])
            .arg(&file)
            .status()
            .map_err(|error| format!("cannot run {}: {error}", rustfmt.display()))?;
        if !status.success() {
            return Err(format!("{} failed: {status}", rustfmt.display()));
        }
        fs::read_to_string(&file)
            .map_err(|error| format!("cannot read {}: {error}", file.display()))
    }
}

impl Drop for Shim {
    fn drop(&mut self) {
        // Best effort: a leftover scratch directory in the temp folder is harmless.
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// The bindgen release pinned by the workspace lockfile.
fn bindgen_version() -> Result<String, String> {
    let lockfile = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.lock");
    let text = fs::read_to_string(&lockfile)
        .map_err(|error| format!("cannot read {}: {error}", lockfile.display()))?;
    locked_version(&text, "bindgen")
        .map(str::to_owned)
        .ok_or_else(|| format!("{} does not pin bindgen", lockfile.display()))
}

/// The version a lockfile records for `package`.
fn locked_version<'a>(lockfile: &'a str, package: &str) -> Option<&'a str> {
    let mut lines = lockfile.lines();
    let name = format!("name = \"{package}\"");
    lines.find(|line| *line == name)?;
    lines
        .next()?
        .strip_prefix("version = \"")?
        .strip_suffix('"')
}

/// A regular expression matching `header` in any directory.
fn header_pattern(header: &str) -> String {
    let mut pattern = String::from(r".*[/\\]");
    for character in header.chars() {
        if !character.is_ascii_alphanumeric() && character != '_' {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern
}

/// The copyright line and MIT permission text of a header, as Rust line comments.
fn license_notice(header: &str) -> Result<String, String> {
    let lines: Vec<&str> = header
        .lines()
        .skip_while(|line| !line.contains("Copyright (C)"))
        .take_while(|line| line.starts_with("//") && !line.contains("****"))
        .collect();
    let complete = lines.iter().any(|line| line.contains(MIT_GRANT))
        && lines
            .iter()
            .any(|line| line.contains("DEALINGS IN THE SOFTWARE"));
    if !complete {
        return Err("the umbrella header's MIT notice has an unexpected layout".into());
    }
    Ok(lines.join("\n").trim_end_matches(['/', '\n']).to_owned())
}

/// Whether two bindings differ only in the signedness of C enum integer types
/// and in the named platform-only constants.
fn equivalent(vendored: &str, other: &str) -> Result<(), String> {
    let portable = |line: &&str| {
        !PLATFORM_ONLY_CONSTANTS
            .iter()
            .any(|name| line.starts_with(&format!("pub const {name}:")))
    };
    let mut vendored_lines = vendored.lines().filter(portable);
    let mut other_lines = other.lines().filter(portable);
    loop {
        match (vendored_lines.next(), other_lines.next()) {
            (None, None) => return Ok(()),
            (Some(a), Some(b))
                if a == b || enum_alias(a).is_some_and(|x| enum_alias(b) == Some(x)) => {}
            (a, b) => {
                return Err(format!(
                    "`{}` versus `{}`",
                    a.unwrap_or("<end of file>"),
                    b.unwrap_or("<end of file>")
                ))
            }
        }
    }
}

/// The alias name when `line` declares a C enum's underlying 32-bit integer type.
fn enum_alias(line: &str) -> Option<&str> {
    let (name, underlying) = line
        .strip_prefix("pub type ")?
        .strip_suffix(';')?
        .split_once(" = ")?;
    matches!(
        underlying,
        "::std::os::raw::c_int" | "::std::os::raw::c_uint"
    )
    .then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_signedness_is_the_only_tolerated_difference() {
        let vendored = "pub type NDIlib_frame_type_e = ::std::os::raw::c_uint;\npub fn a();";
        let msvc = "pub type NDIlib_frame_type_e = ::std::os::raw::c_int;\npub fn a();";
        assert!(equivalent(vendored, msvc).is_ok());
        assert!(equivalent(
            vendored,
            "pub type Other = ::std::os::raw::c_int;\npub fn a();"
        )
        .is_err());
        assert!(equivalent(vendored, &msvc.replace("fn a", "fn b")).is_err());
        assert!(equivalent(vendored, &format!("{msvc}\npub fn extra();")).is_err());
        let windows_only = format!(
            "{msvc}\npub const {}: NDIlib_recv_color_format_e = 1000;",
            PLATFORM_ONLY_CONSTANTS[0]
        );
        assert!(equivalent(vendored, &windows_only).is_ok());
    }

    #[test]
    fn reads_a_package_version_from_a_lockfile() {
        let lockfile = "[[package]]\nname = \"bindgen\"\nversion = \"0.72.1\"\n";
        assert_eq!(locked_version(lockfile, "bindgen"), Some("0.72.1"));
        assert_eq!(locked_version(lockfile, "syn"), None);
    }

    #[test]
    fn header_patterns_escape_every_separator() {
        assert_eq!(
            header_pattern("Processing.NDI.Lib.h"),
            r".*[/\\]Processing\.NDI\.Lib\.h"
        );
    }

    #[test]
    fn notice_keeps_copyright_and_permission_text() {
        let header = "#pragma once\n// NOTE\n//\n//****\n//\n// Copyright (C) 2023 X.\n//\n// Permission is hereby granted, free of charge, to all.\n// OTHER DEALINGS IN THE SOFTWARE.\n//\n//****\nint x;\n";
        let notice = license_notice(header).unwrap();
        assert!(notice.starts_with("// Copyright (C) 2023 X."));
        assert!(notice.ends_with("DEALINGS IN THE SOFTWARE."));
        assert!(license_notice("// Copyright (C) 2023 X.\nint x;").is_err());
    }
}
