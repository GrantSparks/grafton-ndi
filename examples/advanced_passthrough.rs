//! Example: relaying a source's compressed video and audio without decoding.
//!
//! This example demonstrates:
//! - Receiving codec packets with a [`PacketReceiver`]
//! - Sending them on with a [`CompressedSender`], which borrows each packet
//!   exclusively because the SDK writes to what it sends
//! - Asking the source for a keyframe when a new viewer needs one, and
//!   telling it that it is on air, through the receiver's controls
//! - Passing metadata both ways through the relay: the source's downstream,
//!   and its viewers' requests, such as PTZ, back upstream
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
    Error, Ndi, ReceiverOptions, SenderOptions, Tally,
};

#[path = "common/finder.rs"]
mod finder;

use finder::finder_with_extra_ips;

fn main() -> Result<(), Error> {
    let args: Vec<String> = env::args().collect();
    let extra_ips: Vec<&str> = args[1..].iter().map(|s| s.as_str()).collect();

    let ndi = Ndi::new()?;
    let finder = finder_with_extra_ips(&ndi, &extra_ips)?;
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
    // The packet receiver's own Receiver keeps the common controls. The relay
    // puts the source on air, so tell it so.
    receiver.receiver().set_tally(Tally::new(true, false));
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
            receiver.receiver().request_keyframe()?;
        }
        // A viewer's request, such as PTZ, is meant for the source.
        while let Some(request) = sender.sender().capture_metadata(Duration::ZERO)? {
            if let Err(error) = receiver.receiver().send_metadata(&request) {
                println!("Viewer metadata not passed upstream: {error}");
            }
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
            Some(PacketEvent::Metadata(metadata)) => sender.sender().send_metadata(&metadata),
            None => println!("Waiting for frames..."),
            Some(_) => {}
        }
    }
    let stats = receiver.receiver().connection_stats();
    println!(
        "Relayed {video} video and {audio} audio packets; {} video frames dropped",
        stats.video_frames_dropped()
    );
    Ok(())
}
