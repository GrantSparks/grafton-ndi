//! Generate encoded pictures only when a native test needs them.

#[path = "fixture.rs"]
mod fixture;

use std::{process::Command, sync::OnceLock};

fn pictures() -> &'static (Vec<u8>, Vec<u8>) {
    static PICTURES: OnceLock<(Vec<u8>, Vec<u8>)> = OnceLock::new();
    PICTURES.get_or_init(|| {
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
        let read =
            |name| std::fs::read(directory.path().join(name)).expect("read generated picture");
        (read("quadrants-420.h264"), read("chroma-444.hevc"))
    })
}

pub fn h264() -> &'static [u8] {
    &pictures().0
}

pub fn hevc() -> &'static [u8] {
    &pictures().1
}
