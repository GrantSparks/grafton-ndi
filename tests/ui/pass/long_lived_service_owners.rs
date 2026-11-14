#![allow(dead_code)]

use grafton_ndi::{
    Finder, FinderOptions, Ndi, Receiver, ReceiverOptions, Result, Source, SourceCache,
};
use std::sync::Arc;

fn shared<T: Send + Sync + 'static>(_: T) {}
#[cfg(feature = "advanced")]
fn worker<T: Send + 'static>(_: T) {}
#[cfg(feature = "file-io")]
fn local<T: 'static>(_: T) {}

struct ServerState {
    ndi: Ndi,
    sources: SourceCache,
}

// The returned state borrows neither the runtime nor its configuration.
fn server_state(ndi: &Ndi, options: &FinderOptions) -> Result<Arc<ServerState>> {
    Ok(Arc::new(ServerState {
        ndi: ndi.clone(),
        sources: SourceCache::new(Finder::new(ndi, options)?),
    }))
}

fn core_owners(ndi: &Ndi, source: &Source) -> Result<()> {
    use grafton_ndi::{
        audio_conversion::AudioConverter, video_conversion::VideoConverter, FrameSync, Sender,
        SenderOptions,
    };
    let options = ReceiverOptions::new(source.clone());
    shared(server_state(ndi, &FinderOptions::new())?);
    shared(Receiver::new(ndi, &options)?);
    shared(FrameSync::new(Receiver::new(ndi, &options)?)?);
    shared(Sender::new(ndi, &SenderOptions::new("service"))?);
    shared(AudioConverter::new(ndi)?);
    shared(VideoConverter::new(ndi)?);

    #[cfg(feature = "tokio")]
    shared(grafton_ndi::asynchronous::tokio::AsyncReceiver::new(
        Receiver::new(ndi, &options)?,
    ));
    #[cfg(feature = "smol")]
    shared(grafton_ndi::asynchronous::smol::AsyncReceiver::new(
        Receiver::new(ndi, &options)?,
    ));
    #[cfg(feature = "image-encoding")]
    shared(grafton_ndi::ImageOptions::new(
        grafton_ndi::ImageFormat::Jpeg(85),
    ));
    Ok(())
}

#[cfg(feature = "advanced")]
fn advanced_owners(ndi: &Ndi, source: &Source) -> Result<()> {
    use grafton_ndi::{
        advanced::{
            AllocatorLimits, AllocatorService, AvSync, CompressedSender, Genlock, PacketReceiver,
            PassthroughPreset, QueueOptions, QueuedSender,
        },
        SenderOptions,
    };
    let options = ReceiverOptions::new(source.clone());
    let send = SenderOptions::new("service");
    shared(PacketReceiver::new(ndi, &options, PassthroughPreset::Hevc)?);
    shared(CompressedSender::new(ndi, &send)?);
    shared(QueuedSender::new(ndi, &send, QueueOptions::new())?);
    worker(AvSync::new(Receiver::new(ndi, &options)?)?);
    worker(Genlock::new(ndi, Some(source), None)?);
    let service = AllocatorService::new(ndi.process_lifetime(), AllocatorLimits::new())?;
    shared(service.attach(Receiver::new(ndi, &options)?)?);
    shared(service.attach_packets(PacketReceiver::new(ndi, &options, PassthroughPreset::Hevc)?)?);
    Ok(())
}

#[cfg(feature = "file-io")]
fn file_owners(ndi: &Ndi) -> Result<()> {
    use grafton_ndi::file::{
        AsyncFilePlayer, AsyncFileRecorder, FileApi, FileRuntimeOptions, PlayerOptions,
        RecorderOptions, SourceSpec,
    };
    let api = FileApi::open(ndi, &FileRuntimeOptions::new())?;
    let player = PlayerOptions::new("service");
    let recorder = RecorderOptions::new(SourceSpec::new("source")?);
    // Native objects stay on their thread; only their lifetime is unconstrained.
    local(api.create_player(&player)?);
    local(api.create_recorder(&recorder)?);
    // Deferred creation owns its API and options before its first poll.
    shared(AsyncFilePlayer::create(&api, &player));
    shared(AsyncFileRecorder::create(&api, &recorder));
    shared(api);
    Ok(())
}

fn discovery_owners(ndi: &Ndi) -> Result<()> {
    use grafton_ndi::{
        discovery::{
            AdvertiserOptions, ListenerOptions, RecvAdvertiser, RecvListener, SendAdvertiser,
            SendListener,
        },
        routing::{Router, RouterOptions},
    };
    // These owners move to a worker but intentionally do not implement Sync.
    fn discovery_worker<T: Send + 'static>(_: T) {}
    discovery_worker(Router::new(ndi, &RouterOptions::new("service"))?);
    discovery_worker(SendAdvertiser::new(ndi, &AdvertiserOptions::new())?);
    discovery_worker(RecvAdvertiser::new(ndi, &AdvertiserOptions::new())?);
    discovery_worker(SendListener::new(ndi, &ListenerOptions::new())?);
    discovery_worker(RecvListener::new(ndi, &ListenerOptions::new())?);
    Ok(())
}

fn main() {}
