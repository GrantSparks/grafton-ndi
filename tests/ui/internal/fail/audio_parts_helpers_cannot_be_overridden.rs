// The contract test prepends the actual crate-private AudioParts declaration.
struct Frame;
unsafe impl AudioParts for Frame {
    fn layout(&self) -> &ValidatedAudioLayout {
        unimplemented!()
    }
    fn audio_timecode(&self) -> Timecode {
        unimplemented!()
    }
    fn sdk_timestamp(&self) -> Option<ReceivedTimestamp> {
        unimplemented!()
    }
    fn samples(&self) -> *const f32 {
        unimplemented!()
    }
    fn frame_metadata(&self) -> FrameMetadataRef<'_> {
        unimplemented!()
    }
    fn FORBIDDEN_HELPER(&self) {}
}
fn main() {}
