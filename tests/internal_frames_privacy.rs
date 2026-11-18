//! Frame invariants remain sealed even against safe code in the parent module.
#[path = "support/nested_cargo.rs"]
mod nested_cargo;

#[test]
fn frame_invariant_fields_are_private_to_their_validators() {
    let output = nested_cargo::command("frames-privacy", "rustc", None, None)
        .args([
            "--lib",
            "--no-default-features",
            "--",
            "--cfg",
            "ndi_test_frames_privacy",
        ])
        .output()
        .expect("compile private frame-construction probes");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "unchecked frame literals compiled"
    );
    let errors: Vec<_> = stderr
        .lines()
        .filter_map(|line| {
            line.strip_prefix("error[")
                .and_then(|line| line.split_once(']').map(|(code, _)| code))
        })
        .collect();
    assert_eq!(
        errors,
        vec!["E0451"; 10],
        "unexpected compiler failures:\n{stderr}"
    );
    for name in [
        "ValidatedVideoLayout",
        "ValidatedAudioLayout",
        "VideoPlane",
        "VideoPlanes",
        "VideoRows",
        "AudioChannels",
        "ValidatedFrame",
        "PackedVideo",
        "BorrowedVideoFrame",
        "BorrowedAudioFrame",
    ] {
        assert!(
            stderr.contains(name),
            "missing {name} privacy diagnostic:\n{stderr}"
        );
    }
}
