//! Example: relaying a source's compressed video and audio without decoding.
//!
//! This example demonstrates:
//! - Receiving codec packets with a [`PacketReceiver`]
//! - Sending them on with a [`CompressedSender`], which borrows each packet
//!   exclusively because the SDK writes to what it sends
//! - Asking the source for a keyframe when a new viewer needs one
//!
//! Needs the NDI Advanced SDK at build time and an Advanced runtime.
//!
//! Run with: `cargo run --features advanced --example advanced_passthrough`
//!
//! Optional arguments:
//! - IP address to search: `cargo run --features advanced --example advanced_passthrough -- 192.168.0.110`

use std::{env, time::Duration};

use grafton_ndi::{
    advanced::{
        CompressedSender, PacketEvent, PacketReceiver, PassthroughPreset, ReceivedAudio,
        ReceivedVideo,
    },
    Error, Ndi, ReceiverOptions, SenderOptions,
};

#[path = "common/mod.rs"]
mod common;

fn main() -> Result<(), Error> {
    let args: Vec<String> = env::args().collect();
    let extra_ips: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();

    let ndi = Ndi::new()?;
    let finder = common::finder_with_extra_ips(&ndi, &extra_ips)?;
    let Some(source) = finder
        .find_sources(Duration::from_secs(5))?
        .into_iter()
        .next()
    else {
        println!("No NDI sources found!");
        return Ok(());
    };
    println!("Relaying {source}");

    let mut receiver = PacketReceiver::new(
        &ndi,
        &ReceiverOptions::new(source).name("Passthrough Example"),
        // Every compressed format, video and audio, is passed through.
        PassthroughPreset::AlphaWithAudio,
    )?;
    // The relay forwards frames as they arrive, so the sender must not pace them.
    let mut sender = CompressedSender::new(
        &ndi,
        &SenderOptions::new("Passthrough Relay")
            .clock_video(false)
            .clock_audio(false),
    )?;

    let (mut video, mut audio) = (0u32, 0u32);
    for _ in 0..600 {
        // A viewer that has just connected cannot decode until it sees a keyframe.
        if sender.sender().keyframe_required(None)? {
            receiver.request_keyframe()?;
        }
        match receiver.capture(Duration::from_secs(1))? {
            // A packet the sender cannot pass on is refused with the reason,
            // and stays with the caller.
            Some(PacketEvent::Video(ReceivedVideo::Compressed(mut packet))) => {
                match sender.send_video(&mut packet) {
                    Ok(()) => video += 1,
                    Err(error) => println!("Video packet not relayed: {error}"),
                }
            }
            Some(PacketEvent::Audio(ReceivedAudio::Compressed(mut packet))) => {
                match sender.send_audio(&mut packet) {
                    Ok(()) => audio += 1,
                    Err(error) => println!("Audio packet not relayed: {error}"),
                }
            }
            Some(PacketEvent::Video(ReceivedVideo::Decoded(frame))) => println!(
                "The source sent {}x{} video in a format this preset decodes",
                frame.width(),
                frame.height()
            ),
            Some(PacketEvent::Audio(ReceivedAudio::Decoded(frame))) => println!(
                "The source sent {} audio samples in a format this preset decodes",
                frame.num_samples()
            ),
            None => println!("Waiting for frames..."),
            Some(_) => {}
        }
    }
    println!("Relayed {video} video and {audio} audio packets");
    Ok(())
}
