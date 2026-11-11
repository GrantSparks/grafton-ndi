//! Example: relaying a source's compressed video and audio without decoding.
//!
//! This example demonstrates:
//! - Receiving codec packets with a [`PacketReceiver`]
//! - Sending them unchanged with a [`CompressedSender`]
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
        AdvancedConfig, CompressedSender, PacketEvent, PacketReceiver, PassthroughPreset,
        ReceivedAudio, ReceivedVideo,
    },
    Error, ReceiverOptions, SenderOptions, NDI,
};

#[path = "common/mod.rs"]
mod common;

fn main() -> Result<(), Error> {
    let args: Vec<String> = env::args().collect();
    let extra_ips: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();

    let ndi = NDI::new()?;
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
        &ReceiverOptions::builder(source)
            .name("Passthrough Example")
            .build(),
        PassthroughPreset::HevcWithAudio,
        &AdvancedConfig::parse("{}")?,
    )?;
    // The relay forwards frames as they arrive, so the sender must not pace them.
    let mut sender = CompressedSender::new(
        &ndi,
        &SenderOptions::builder("Passthrough Relay")
            .clock_video(false)
            .clock_audio(false)
            .build(),
    )?;

    for _ in 0..600 {
        // A viewer that has just connected cannot decode until it sees a keyframe.
        if sender.keyframe_required(None)? {
            receiver.request_keyframe()?;
        }
        match receiver.capture(Duration::from_secs(1))? {
            PacketEvent::Video(ReceivedVideo::Compressed(packet)) => {
                match packet.validate_for_send() {
                    Ok(()) => sender.send_video(&packet)?,
                    Err(error) => println!("Video packet kept but not relayed: {error}"),
                }
            }
            PacketEvent::Audio(ReceivedAudio::Compressed(packet)) => {
                match packet.validate_for_send() {
                    Ok(()) => sender.send_audio(&packet)?,
                    Err(error) => println!("Audio packet kept but not relayed: {error}"),
                }
            }
            PacketEvent::Video(ReceivedVideo::Decoded(frame)) => println!(
                "The source sent {}x{} video in a format this preset decodes",
                frame.width(),
                frame.height()
            ),
            PacketEvent::Timeout => println!("Waiting for frames..."),
            _ => {}
        }
    }
    Ok(())
}
