//! Example: playing a recording back as an NDI source with the File API.
//!
//! Probes a recording, advertises an NDI source and plays the file through it
//! once, printing the player's events.
//!
//! Run with:
//! `cargo run --example file_play --features file-io -- take_001.mkv "Replay"`
//!
//! Arguments: the recording to play, and optionally the name of the NDI
//! source to advertise ("grafton-ndi File Player" by default).
//!
//! The File library ships with the NDI SDK, beside the runtime library. On
//! Linux and macOS it needs the Advanced SDK runtime.

use std::{env, time::Duration};

use grafton_ndi::{
    file::{FileApi, FileRuntimeOptions, PlayerEventKind, PlayerOptions},
    Ndi,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args().skip(1);
    let Some(recording) = arguments.next() else {
        eprintln!("usage: file_play <recording.mkv> [NDI source name]");
        std::process::exit(2);
    };
    let name = arguments
        .next()
        .unwrap_or_else(|| "grafton-ndi File Player".to_owned());

    let ndi = Ndi::new()?;
    let file = FileApi::open(&ndi, &FileRuntimeOptions::default())?;

    // Probing reads the file's layout without advertising anything.
    let info = file.probe(&recording)?;
    println!(
        "{recording}: {:.1} s, {} video / {} audio / {} metadata track(s)",
        info.duration().value().duration().as_secs_f64(),
        info.video_track_count(),
        info.audio_track_count(),
        info.metadata_track_count()
    );

    // The source is advertised from here until the player is dropped.
    let mut player = file.create_player(&PlayerOptions::new(name))?;
    println!("Advertising {}", player.advertised_source()?);
    player.start(&recording)?;

    // Keep taking events until the file has ended and none is left.
    loop {
        match player.poll_event(Duration::from_millis(250))? {
            Some(event) => match event.kind() {
                PlayerEventKind::Ended => println!("Reached the end of the file."),
                PlayerEventKind::Error => {
                    return Err(
                        format!("playback failed: {}", event.message().to_string_lossy()).into(),
                    );
                }
                kind => println!("{kind:?}"),
            },
            None if !player.is_running() => break,
            None => {}
        }
    }
    if let Some(stats) = player.stats()? {
        println!("Sent {} video frame(s).", stats.video_frames()[0]);
    }
    Ok(())
}
