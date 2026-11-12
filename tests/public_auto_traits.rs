//! The thread and unwind auto traits of the public types.
//!
//! Each line names a type and the auto traits it has, so that losing one, a
//! breaking change, fails to compile. CI builds these tests without the
//! Advanced SDK headers and with them, so a trait that holds in only one of
//! those builds fails as well: the `advanced` API has the same shape in both.
//! A trait a type lacks is its documented design: the File objects and
//! `Genlock` stay on their thread, `AvSync`, the Discovery Server owners and
//! `AllocationBuffer` are `Send` only, and `Error` and an ordinary `Sender`
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

/// The public types of the crate root.
#[test]
fn crate_root_types_keep_their_auto_traits() {
    #[allow(unused_imports)]
    use grafton_ndi::*;
    has! {
        AudioFrame: Send, Sync, UnwindSafe, RefUnwindSafe;
        AudioInputFormat: Send, Sync, UnwindSafe, RefUnwindSafe;
        Capabilities: Send, Sync, UnwindSafe, RefUnwindSafe;
        ConnectionStats: Send, Sync, UnwindSafe, RefUnwindSafe;
        Error: Send, Sync;
        Finder: Send, Sync, UnwindSafe, RefUnwindSafe;
        FrameSync: Send, Sync, UnwindSafe, RefUnwindSafe;
        FrameSyncAttached: Send, Sync, UnwindSafe, RefUnwindSafe;
        FrameSyncAudioRequest: Send, Sync, UnwindSafe, RefUnwindSafe;
        MetadataFrame: Send, Sync, UnwindSafe, RefUnwindSafe;
        Ndi: Send, Sync, UnwindSafe, RefUnwindSafe;
        ProcessLifetimeRuntime: Send, Sync, UnwindSafe, RefUnwindSafe;
        RawCapture: Send, Sync, UnwindSafe, RefUnwindSafe;
        RawSend: Send, Sync, UnwindSafe, RefUnwindSafe;
        Receiver: Send, Sync, UnwindSafe, RefUnwindSafe;
        Receiver<FrameSyncAttached>: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverBandwidth: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverColorFormat: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverEvent: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverStatus: Send, Sync, UnwindSafe, RefUnwindSafe;
        Runtime: Send, Sync, UnwindSafe, RefUnwindSafe;
        RuntimeInfo: Send, Sync, UnwindSafe, RefUnwindSafe;
        Sender<RawSend>: Send, Sync;
        SenderOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        SettingChange: Send, Sync, UnwindSafe, RefUnwindSafe;
        Source: Send, Sync, UnwindSafe, RefUnwindSafe;
        SourceNameChange: Send, Sync, UnwindSafe, RefUnwindSafe;
        Tally: Send, Sync, UnwindSafe, RefUnwindSafe;
        VideoFrame: Send, Sync, UnwindSafe, RefUnwindSafe;
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
        ListenerOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverCommand: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceiverStream: Send, Sync, UnwindSafe, RefUnwindSafe;
        RecvAdvertisement: Send, Sync, UnwindSafe, RefUnwindSafe;
        RecvAdvertiser: Send, UnwindSafe, RefUnwindSafe;
        RecvListener: Send, UnwindSafe, RefUnwindSafe;
        RecvRegistration: Send, Sync, UnwindSafe, RefUnwindSafe;
        SendAdvertisement: Send, Sync, UnwindSafe, RefUnwindSafe;
        SendAdvertiser: Send, UnwindSafe, RefUnwindSafe;
        SendListener: Send, UnwindSafe, RefUnwindSafe;
        SendRegistration: Send, Sync, UnwindSafe, RefUnwindSafe;
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
        Sender<CompressedAttached>: Send, Sync, UnwindSafe;
        Sender<QueueAttached>: Send, Sync, UnwindSafe;
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
        AudioCodec: Send, Sync, UnwindSafe, RefUnwindSafe;
        AvSync: Send, UnwindSafe, RefUnwindSafe;
        AvSyncAttached: Send, Sync, UnwindSafe, RefUnwindSafe;
        AvSyncOutcome: Send, Sync, UnwindSafe, RefUnwindSafe;
        AvSyncRequest: Send, Sync, UnwindSafe, RefUnwindSafe;
        ClockSource: Send, Sync, UnwindSafe, RefUnwindSafe;
        CompletedVideo: Send, Sync, UnwindSafe, RefUnwindSafe;
        CompressedAudioFrame: Send, Sync, UnwindSafe, RefUnwindSafe;
        CompressedSender: Send, Sync, UnwindSafe;
        CompressedVideoFrame: Send, Sync, UnwindSafe, RefUnwindSafe;
        EnvelopeCodec: Send, Sync, UnwindSafe, RefUnwindSafe;
        ExtendedSource: Send, Sync, UnwindSafe, RefUnwindSafe;
        Genlock: UnwindSafe, RefUnwindSafe;
        KvmSession<'static, RawCapture>: Send, Sync, UnwindSafe, RefUnwindSafe;
        MouseButton: Send, Sync, UnwindSafe, RefUnwindSafe;
        PacketAttached: Send, Sync, UnwindSafe, RefUnwindSafe;
        PacketEvent: Send, Sync, UnwindSafe, RefUnwindSafe;
        PacketReceiver: Send, Sync, UnwindSafe, RefUnwindSafe;
        PassthroughPreset: Send, Sync, UnwindSafe, RefUnwindSafe;
        QualityPlan: Send, Sync, UnwindSafe, RefUnwindSafe;
        QueueOptions: Send, Sync, UnwindSafe, RefUnwindSafe;
        QueuedSender: Send, Sync, UnwindSafe;
        QueuedVideoFrame: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceivedAudio: Send, Sync, UnwindSafe, RefUnwindSafe;
        ReceivedVideo: Send, Sync, UnwindSafe, RefUnwindSafe;
        ScatterAudioFrame: Send, Sync, UnwindSafe, RefUnwindSafe;
        ScatterVideoFrame: Send, Sync, UnwindSafe, RefUnwindSafe;
        SubmissionId: Send, Sync, UnwindSafe, RefUnwindSafe;
        SubmittedVideo: Send, Sync, UnwindSafe, RefUnwindSafe;
        UnsentVideo: Send, Sync, UnwindSafe, RefUnwindSafe;
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
