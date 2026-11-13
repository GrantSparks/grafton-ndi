//! Write ten seconds of clock-corrected, interleaved stereo f32 PCM to stdout.
//!
//! Run with a full source name; pipe stdout into a PCM playback sink configured
//! for 48 kHz, two channels, little-endian f32. Diagnostics go to stderr.
//! `cargo run --example framesync_audio_playback -- 'HOST (Audio)' > output.f32`
//!
//! A software timer supplies this example's output clock. An audio device
//! should request each block at its own cadence instead. FrameSync may supply
//! silence when input is unavailable; silence does not prove fresh reception.
use grafton_ndi::{
    audio_conversion::AudioConverter, Error, Finder, FinderOptions, FrameSync,
    FrameSyncAudioRequest, Ndi, Receiver, ReceiverBandwidth, ReceiverOptions,
};
use std::{
    env,
    io::{self, Write},
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args().skip(1);
    let name = arguments.next().ok_or("provide the complete source name")?;
    if arguments.next().is_some() {
        return Err("provide exactly one complete source name".into());
    }
    let ndi = Ndi::new()?;
    let finder = Finder::new(&ndi, &FinderOptions::new().extra_ips("127.0.0.1"))?;
    let source = finder
        .find_source_by_name(&name, Duration::from_secs(10))?
        .ok_or("selected audio source was not discovered within ten seconds")?;
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::new(source).bandwidth(ReceiverBandwidth::AudioOnly),
    )?;
    let framesync = FrameSync::new(receiver).map_err(Error::from)?;
    let converter = AudioConverter::new(&ndi)?;
    let request = FrameSyncAudioRequest::capture_at(48_000, 2, 480)?;
    let mut interleaved = vec![0.0f32; 960];
    let mut bytes = vec![0u8; interleaved.len() * 4];
    let stdout = io::stdout();
    let mut output = stdout.lock();
    eprintln!("Writing 48 kHz stereo f32 PCM for ten seconds");
    for _ in 0..1000 {
        let tick = Instant::now();
        let audio = framesync.capture_audio_ref(request)?;
        // The native converter reads the borrowed channel stride directly.
        // No owned planar frame or per-block wrapper allocation is required.
        let written = converter.to_interleaved_f32(&audio, &mut interleaved)?;
        for (sample, bytes) in interleaved[..written]
            .iter()
            .zip(bytes.as_chunks_mut::<4>().0)
        {
            bytes.copy_from_slice(&sample.to_le_bytes());
        }
        output.write_all(&bytes[..written * 4])?;
        std::thread::sleep(Duration::from_millis(10).saturating_sub(tick.elapsed()));
    }
    output.flush()?;
    Ok(())
}
