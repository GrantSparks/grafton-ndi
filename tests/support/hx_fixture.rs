//! Load the CI test pictures or generate them on demand for local runs.

#[path = "fixture.rs"]
mod fixture;

use std::{path::Path, process::Command, sync::OnceLock};

fn read_pictures(directory: &Path) -> (Vec<u8>, Vec<u8>) {
    let read = |name| {
        let path = directory.join(name);
        std::fs::read(&path)
            .unwrap_or_else(|error| panic!("read NDI|HX test picture {}: {error}", path.display()))
    };
    (read("quadrants-420.h264"), read("chroma-444.hevc"))
}

fn pictures() -> &'static (Vec<u8>, Vec<u8>) {
    static PICTURES: OnceLock<(Vec<u8>, Vec<u8>)> = OnceLock::new();
    PICTURES.get_or_init(|| {
        if let Some(directory) = std::env::var_os("GRAFTON_NDI_HX_PICTURES") {
            return read_pictures(Path::new(&directory));
        }
        let directory = fixture::FixtureDir::create("hx-pictures");
        let python = if cfg!(windows) { "python" } else { "python3" };
        let output = Command::new(python)
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/support/generate_hx.py"
            ))
            .arg("--output-dir")
            .arg(directory.path())
            .output()
            .expect("NDI|HX test pictures need Python 3 and FFmpeg with libx264/libx265");
        assert!(
            output.status.success(),
            "NDI|HX test picture generation failed; install FFmpeg with libx264/libx265: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        read_pictures(directory.path())
    })
}

pub fn h264() -> &'static [u8] {
    &pictures().0
}

pub fn hevc() -> &'static [u8] {
    &pictures().1
}
