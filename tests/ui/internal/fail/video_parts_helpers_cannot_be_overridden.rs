// The contract test prepends the actual crate-private VideoParts declaration.
struct Frame;
unsafe impl VideoParts for Frame {
    fn layout(&self) -> &ValidatedVideoLayout { unimplemented!() }
    fn header(&self) -> VideoHeader { unimplemented!() }
    fn pixels(&self) -> *const u8 { unimplemented!() }
    fn frame_metadata(&self) -> FrameMetadataRef<'_> { unimplemented!() }
    fn sdk_timestamp(&self) -> Option<SdkTimestamp> { unimplemented!() }
    fn FORBIDDEN_HELPER(&self) {}
}
fn main() {}
