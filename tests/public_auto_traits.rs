//! The thread and unwind auto traits of the public types.
//!
//! Each line names a type and the auto traits it has, so that losing one, a
//! breaking change, fails to compile. CI builds these tests without the
//! Advanced SDK headers and with them, so a trait that holds in only one of
//! those builds fails as well: the `advanced` API has the same shape in both.
//! A trait a type lacks is its documented design, stated beside the type:
//! the File objects and FrameSync's frame references stay on their thread,
//! the Discovery Server owners, a `Router`, `AvSync`, `Genlock` and its clocks
//! are `Send` but not `Sync`, and `Error`, `Rejected` and an ordinary `Sender`
//! may hold application values that are not unwind safe. Gaining a trait is
//! not a breaking change; list it here when a type gains one, and list a new
//! public type with the traits it is meant to have.

use std::panic::{RefUnwindSafe, UnwindSafe};

/// Compile only if each type has the traits listed after it.
macro_rules! has {
    ($($type:ty: $($bound:ident),+;)+) => {
        $({
            fn check<T: ?Sized $(+ $bound)+>() {}
            check::<$type>();
        })+
    };
}

/// Infer one marker implementation only while the type lacks the bound.
/// If the type gains the bound, inference becomes ambiguous and compilation
/// fails. This keeps negative auto-trait checks independent of SDK-private
/// handle names in compiler diagnostics.
macro_rules! lacks {
    ($($type:ty: $bound:ident;)+) => {
        $({
            trait AmbiguousIfImplemented<A> { fn check() {} }
            impl<T: ?Sized> AmbiguousIfImplemented<()> for T {}
            impl<T: ?Sized + $bound> AmbiguousIfImplemented<u8> for T {}
            let _ = <$type as AmbiguousIfImplemented<_>>::check;
        })+
    };
}

#[test]
fn split_views_stay_on_the_borrowing_thread() {
    use grafton_ndi::FrameSyncCapture;
    lacks! {
        FrameSyncCapture<'static>: Send;
        FrameSyncCapture<'static>: Sync;
    }
    #[cfg(feature = "advanced")]
    {
        use grafton_ndi::{
            advanced::{AvSyncCapture, ReceiverControls},
            RawCapture,
        };
        lacks! {
            AvSyncCapture<'static>: Send;
            AvSyncCapture<'static>: Sync;
            ReceiverControls<'static, RawCapture>: Send;
            ReceiverControls<'static, RawCapture>: Sync;
        }
    }
}

/// The public types of the crate root and its modules other than those below.
#[test]
fn crate_root_types_keep_their_auto_traits() {
    #[allow(unused_imports)]
    use grafton_ndi::*;
    has! {
        AudioFormat: Send, Sync, UnwindSafe, RefUnwindSafe;
        AudioFrame: Send, Sync, UnwindSafe, RefUnwindSafe;
        AudioFrameBuilder: Send, Sync, UnwindSafe, RefUnwindSafe;
        AudioFrameRef<'static>: Send, Sync, UnwindSafe, RefUnwindSafe;
        AudioInputFormat: Send, Sync, UnwindSafe, RefUnwindSafe;
        AudioKind: Send, Sync, UnwindSafe, RefUnwindSafe;
        AudioLayout: Send, Sync, UnwindSafe, RefUnwindSafe;
        BorrowedVideoFrame<'static>: Send, Sync, UnwindSafe, RefUnwindSafe;
        BorrowedVideoFrameBuilder<'static>: Send, Sync, UnwindSafe, RefUnwindSafe;
        Capabilities: Send, Sync, UnwindSafe, RefUnwindSafe;
        Capture<'static, AudioKind>: Send, Sync, UnwindSafe, RefUnwindSafe;
        Capture<'static, MetadataKind>: Send, Sync, UnwindSafe, RefUnwindSafe;
        Capture<'static, VideoKind>: Send, Sync, UnwindSafe, RefUnwindSafe;
        ColorInfo: Send, Sync, UnwindSafe, RefUnwindSafe;
        ColorPrimaries: Send, Sync, UnwindSafe, RefUnwindSafe;
        ConnectionStats: Send, Sync, UnwindSafe, RefUnwindSafe;
        Error: Send, Sync;
        Finder: Send, Sync, UnwindSafe, RefUnwindSafe;
        FinderOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        FormatCategory: Send, Sync, UnwindSafe, RefUnwindSafe;
        FourCc: Send, Sync, UnwindSafe, RefUnwindSafe;
        FrameRate: Send, Sync, UnwindSafe, RefUnwindSafe;
        FrameSync: Send, Sync, UnwindSafe, RefUnwindSafe;
        FrameSyncAttached: Send, Sync, UnwindSafe, RefUnwindSafe;
        FrameSyncAudioRef<'static>: UnwindSafe, RefUnwindSafe;
        FrameSyncAudioRequest: Send, Sync, UnwindSafe, RefUnwindSafe;
        FrameSyncCapture<'static>: UnwindSafe, RefUnwindSafe;
        FrameSyncVideoRef<'static>: UnwindSafe, RefUnwindSafe;
        MediaTypes: Send, Sync, UnwindSafe, RefUnwindSafe;
        MetadataFrame: Send, Sync, UnwindSafe, RefUnwindSafe;
        MetadataFrameRef<'static>: Send, Sync, UnwindSafe, RefUnwindSafe;
        MetadataKind: Send, Sync, UnwindSafe, RefUnwindSafe;
        Ndi: Send, Sync, UnwindSafe, RefUnwindSafe;
        PixelFormat: Send, Sync, UnwindSafe, RefUnwindSafe;
        PixelFormatInfo: Send, Sync, UnwindSafe, RefUnwindSafe;
        ProcessLifetimeRuntime: Send, Sync, UnwindSafe, RefUnwindSafe;
        Ptz<'static>: Send, Sync, UnwindSafe, RefUnwindSafe;
        QueueLimit: Send, Sync, UnwindSafe, RefUnwindSafe;
        RawCapture: Send, Sync, UnwindSafe, RefUnwindSafe;
        RawSend: Send, Sync, UnwindSafe, RefUnwindSafe;
        Receiver: Send, Sync, UnwindSafe, RefUnwindSafe;
        Receiver<FrameSyncAttached>: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverBandwidth: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverColorFormat: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverEvent: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverItem: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverItemRef<'static>: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverStatus: Send, Sync, UnwindSafe, RefUnwindSafe;
        Rejected<VideoFrame>: Send, Sync;
        Runtime: Send, Sync, UnwindSafe, RefUnwindSafe;
        RuntimeInfo: Send, Sync, UnwindSafe, RefUnwindSafe;
        RuntimeOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        ScanType: Send, Sync, UnwindSafe, RefUnwindSafe;
        SdkTimestamp: Send, Sync, UnwindSafe, RefUnwindSafe;
        Sender<RawSend>: Send, Sync;
        SenderOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        SettingChange: Send, Sync, UnwindSafe, RefUnwindSafe;
        Source: Send, Sync, UnwindSafe, RefUnwindSafe;
        SourceAddress: Send, Sync, UnwindSafe, RefUnwindSafe;
        SourceCache<'static>: Send, Sync, UnwindSafe, RefUnwindSafe;
        SourceNameChange: Send, Sync, UnwindSafe, RefUnwindSafe;
        StreamTier: Send, Sync, UnwindSafe, RefUnwindSafe;
        Tally: Send, Sync, UnwindSafe, RefUnwindSafe;
        Timecode: Send, Sync, UnwindSafe, RefUnwindSafe;
        TransferFunction: Send, Sync, UnwindSafe, RefUnwindSafe;
        VideoFrame: Send, Sync, UnwindSafe, RefUnwindSafe;
        VideoFrameBuilder: Send, Sync, UnwindSafe, RefUnwindSafe;
        VideoFrameRef<'static>: Send, Sync, UnwindSafe, RefUnwindSafe;
        VideoKind: Send, Sync, UnwindSafe, RefUnwindSafe;
        VideoPlane<'static>: Send, Sync, UnwindSafe, RefUnwindSafe;
        VideoPlanes<'static>: Send, Sync, UnwindSafe, RefUnwindSafe;
        VideoRows<'static>: Send, Sync, UnwindSafe, RefUnwindSafe;
        YuvMatrix: Send, Sync, UnwindSafe, RefUnwindSafe;
        audio_conversion::AudioConverter: Send, Sync, UnwindSafe, RefUnwindSafe;
        audio_conversion::AudioReferenceLevel: Send, Sync, UnwindSafe, RefUnwindSafe;
        capabilities::CapabilityStatus: Send, Sync, UnwindSafe, RefUnwindSafe;
        capabilities::CapabilityUnavailable: Send, Sync, UnwindSafe, RefUnwindSafe;
        capabilities::RuntimeFeature: Send, Sync, UnwindSafe, RefUnwindSafe;
        capabilities::SdkEdition: Send, Sync, UnwindSafe, RefUnwindSafe;
        capabilities::SdkLinkage: Send, Sync, UnwindSafe, RefUnwindSafe;
        capabilities::SdkVersion: Send, Sync, UnwindSafe, RefUnwindSafe;
        routing::Router: Send, UnwindSafe, RefUnwindSafe;
        routing::RouterOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        video_conversion::V210Layout: Send, Sync, UnwindSafe, RefUnwindSafe;
        video_conversion::VideoConverter: Send, Sync, UnwindSafe, RefUnwindSafe;
    }
}

/// The public types of the `image-encoding` feature.
#[cfg(feature = "image-encoding")]
#[test]
fn image_encoding_types_keep_their_auto_traits() {
    use grafton_ndi::{
        AlphaMode, ColorPolicy, Downscale, HdrPolicy, ImageFormat, ImageOptions, PngCompression,
        SampleDepth, YuvRange,
    };
    has! {
        AlphaMode: Send, Sync, UnwindSafe, RefUnwindSafe;
        ColorPolicy: Send, Sync, UnwindSafe, RefUnwindSafe;
        Downscale: Send, Sync, UnwindSafe, RefUnwindSafe;
        HdrPolicy: Send, Sync, UnwindSafe, RefUnwindSafe;
        ImageFormat: Send, Sync, UnwindSafe, RefUnwindSafe;
        ImageOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        PngCompression: Send, Sync, UnwindSafe, RefUnwindSafe;
        SampleDepth: Send, Sync, UnwindSafe, RefUnwindSafe;
        YuvRange: Send, Sync, UnwindSafe, RefUnwindSafe;
    }
}

/// The public types of the `asynchronous` module with Tokio.
#[cfg(feature = "tokio")]
#[test]
fn tokio_types_keep_their_auto_traits() {
    #[allow(unused_imports)]
    use grafton_ndi::asynchronous::{AsyncReceiver, Tokio};
    has! {
        AsyncReceiver<Tokio>: Send, Sync, UnwindSafe, RefUnwindSafe;
        Tokio: Send, Sync, UnwindSafe, RefUnwindSafe;
    }
}

/// The public types of the `asynchronous` module with smol.
#[cfg(feature = "smol")]
#[test]
fn smol_types_keep_their_auto_traits() {
    #[allow(unused_imports)]
    use grafton_ndi::asynchronous::{AsyncReceiver, Smol};
    has! {
        AsyncReceiver<Smol>: Send, Sync, UnwindSafe, RefUnwindSafe;
        Smol: Send, Sync, UnwindSafe, RefUnwindSafe;
    }
}

/// The public types of the `discovery` module.
#[test]
fn discovery_types_keep_their_auto_traits() {
    #[allow(unused_imports)]
    use grafton_ndi::discovery::*;
    has! {
        AdvertisedReceiver: Send, Sync, UnwindSafe, RefUnwindSafe;
        AdvertisedSender: Send, Sync, UnwindSafe, RefUnwindSafe;
        AdvertiserOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        InputGroupId: Send, Sync, UnwindSafe, RefUnwindSafe;
        ListenerEvent<ReceiverId>: Send, Sync, UnwindSafe, RefUnwindSafe;
        ListenerEvent<SenderId>: Send, Sync, UnwindSafe, RefUnwindSafe;
        ListenerOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverCommand: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverId: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverStream: Send, Sync, UnwindSafe, RefUnwindSafe;
        RecvAdvertisement: Send, Sync, UnwindSafe, RefUnwindSafe;
        RecvAdvertiser: Send, UnwindSafe, RefUnwindSafe;
        RecvListener: Send, UnwindSafe, RefUnwindSafe;
        RecvRegistration: Send, Sync, UnwindSafe, RefUnwindSafe;
        SendAdvertisement: Send, Sync, UnwindSafe, RefUnwindSafe;
        SendAdvertiser: Send, UnwindSafe, RefUnwindSafe;
        SendListener: Send, UnwindSafe, RefUnwindSafe;
        SendRegistration: Send, Sync, UnwindSafe, RefUnwindSafe;
        SenderId: Send, Sync, UnwindSafe, RefUnwindSafe;
    }
}

/// The public types of the Advanced modes of `Sender` and `Receiver`.
#[cfg(feature = "advanced")]
#[test]
fn advanced_modes_types_keep_their_auto_traits() {
    #[allow(unused_imports)]
    use grafton_ndi::{
        advanced::{AvSyncAttached, PacketAttached},
        CompressedAttached, QueueAttached, Receiver, Sender,
    };
    has! {
        CompressedAttached: Send, Sync, UnwindSafe, RefUnwindSafe;
        Receiver<AvSyncAttached>: Send, Sync, UnwindSafe, RefUnwindSafe;
        Receiver<PacketAttached>: Send, Sync, UnwindSafe, RefUnwindSafe;
        Sender<CompressedAttached>: Send, Sync, UnwindSafe, RefUnwindSafe;
        Sender<QueueAttached>: Send, Sync, UnwindSafe, RefUnwindSafe;
    }
}

/// The public types of the `advanced` module.
#[cfg(feature = "advanced")]
#[test]
fn advanced_types_keep_their_auto_traits() {
    #[allow(unused_imports)]
    use grafton_ndi::{advanced::*, RawCapture};
    has! {
        AdvancedConfig: Send, Sync, UnwindSafe, RefUnwindSafe;
        AllocatedPacketReceiver: Send, Sync, UnwindSafe, RefUnwindSafe;
        AllocatedReceiver: Send, Sync, UnwindSafe, RefUnwindSafe;
        AllocationBuffer: Send;
        AllocationFormat: Send, Sync, UnwindSafe, RefUnwindSafe;
        AllocationRequest: Send, Sync, UnwindSafe, RefUnwindSafe;
        AllocatorLimits: Send, Sync, UnwindSafe, RefUnwindSafe;
        AllocatorService: Send, Sync, UnwindSafe, RefUnwindSafe;
        AllocatorUsage: Send, Sync, UnwindSafe, RefUnwindSafe;
        AudioClock<'static>: Send, UnwindSafe, RefUnwindSafe;
        AudioCodec: Send, Sync, UnwindSafe, RefUnwindSafe;
        AvSync: Send, UnwindSafe, RefUnwindSafe;
        AvSyncCapture<'static>: UnwindSafe, RefUnwindSafe;
        ReceiverControls<'static, RawCapture>: RefUnwindSafe;
        AvSyncAttached: Send, Sync, UnwindSafe, RefUnwindSafe;
        AvSyncOutcome: Send, Sync, UnwindSafe, RefUnwindSafe;
        AvSyncRequest: Send, Sync, UnwindSafe, RefUnwindSafe;
        ClockSource: Send, Sync, UnwindSafe, RefUnwindSafe;
        CompletedVideo: Send, Sync, UnwindSafe, RefUnwindSafe;
        CompressedAudioFrame: Send, Sync, UnwindSafe, RefUnwindSafe;
        CompressedSender: Send, Sync, UnwindSafe, RefUnwindSafe;
        CompressedVideoFrame: Send, Sync, UnwindSafe, RefUnwindSafe;
        EnvelopeCodec: Send, Sync, UnwindSafe, RefUnwindSafe;
        ExtendedSource: Send, Sync, UnwindSafe, RefUnwindSafe;
        Genlock: Send, UnwindSafe, RefUnwindSafe;
        KvmSession<'static, RawCapture>: Send, Sync, UnwindSafe, RefUnwindSafe;
        MouseButton: Send, Sync, UnwindSafe, RefUnwindSafe;
        PacketAttached: Send, Sync, UnwindSafe, RefUnwindSafe;
        PacketEnvelope<'static>: Send, Sync, UnwindSafe, RefUnwindSafe;
        PacketEvent: Send, Sync, UnwindSafe, RefUnwindSafe;
        PacketReceiver: Send, Sync, UnwindSafe, RefUnwindSafe;
        PassthroughPreset: Send, Sync, UnwindSafe, RefUnwindSafe;
        QualityPlan: Send, Sync, UnwindSafe, RefUnwindSafe;
        QueueOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        QueuedSender: Send, Sync, UnwindSafe, RefUnwindSafe;
        QueuedVideoFrame: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceivedAudio: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceivedVideo: Send, Sync, UnwindSafe, RefUnwindSafe;
        ScatterAudioFrame: Send, Sync, UnwindSafe, RefUnwindSafe;
        ScatterVideoFrame: Send, Sync, UnwindSafe, RefUnwindSafe;
        SubmissionId: Send, Sync, UnwindSafe, RefUnwindSafe;
        SubmittedVideo: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReclaimedVideo: Send, Sync, UnwindSafe, RefUnwindSafe;
        VideoClock<'static>: Send, UnwindSafe, RefUnwindSafe;
        VideoCodec: Send, Sync, UnwindSafe, RefUnwindSafe;
        VideoPacketFormat: Send, Sync, UnwindSafe, RefUnwindSafe;
        WheelAxis: Send, Sync, UnwindSafe, RefUnwindSafe;
    }
}

/// The public types of the `file` module.
#[cfg(feature = "file-io")]
#[test]
fn file_types_keep_their_auto_traits() {
    #[allow(unused_imports)]
    use grafton_ndi::file::*;
    has! {
        AsyncFileOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        AsyncFilePlayer: Send, Sync, UnwindSafe, RefUnwindSafe;
        AsyncFileRecorder: Send, Sync, UnwindSafe, RefUnwindSafe;
        EventText: Send, Sync, UnwindSafe, RefUnwindSafe;
        FileApi: Send, Sync, UnwindSafe, RefUnwindSafe;
        FileDuration: Send, Sync, UnwindSafe, RefUnwindSafe;
        FileFamily: Send, Sync, UnwindSafe, RefUnwindSafe;
        FileInfo: Send, Sync, UnwindSafe, RefUnwindSafe;
        FileNaming: Send, Sync, UnwindSafe, RefUnwindSafe;
        FileOperation<()>: Send, Sync, UnwindSafe, RefUnwindSafe;
        FilePlayer: UnwindSafe, RefUnwindSafe;
        FilePosition: Send, Sync, UnwindSafe, RefUnwindSafe;
        FileRecorder: UnwindSafe, RefUnwindSafe;
        FileRuntimeOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        FinishStatus: Send, Sync, UnwindSafe, RefUnwindSafe;
        PlayerEvent: Send, Sync, UnwindSafe, RefUnwindSafe;
        PlayerEventKind: Send, Sync, UnwindSafe, RefUnwindSafe;
        PlayerOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        PlayerStats: Send, Sync, UnwindSafe, RefUnwindSafe;
        RecorderEvent: Send, Sync, UnwindSafe, RefUnwindSafe;
        RecorderEventKind: Send, Sync, UnwindSafe, RefUnwindSafe;
        RecorderOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        RecorderState: Send, Sync, UnwindSafe, RefUnwindSafe;
        RecorderStats: Send, Sync, UnwindSafe, RefUnwindSafe;
        RecordingReport: Send, Sync, UnwindSafe, RefUnwindSafe;
        RecordingTarget: Send, Sync, UnwindSafe, RefUnwindSafe;
        RecordingTimecode: Send, Sync, UnwindSafe, RefUnwindSafe;
        SourceFormat: Send, Sync, UnwindSafe, RefUnwindSafe;
        SourceSpec: Send, Sync, UnwindSafe, RefUnwindSafe;
        StateError: Send, Sync, UnwindSafe, RefUnwindSafe;
        StopRequest: Send, Sync, UnwindSafe, RefUnwindSafe;
        VideoTrack: Send, Sync, UnwindSafe, RefUnwindSafe;
    }
}
