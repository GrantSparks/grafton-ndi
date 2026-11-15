// The contract test prepends the actual crate-private MetadataParts declaration.
struct Frame;
unsafe impl MetadataParts for Frame {
    fn metadata_text_for_send(&self) -> Option<MetadataText<'_>> {
        unimplemented!()
    }
    fn metadata_timecode(&self) -> Timecode {
        unimplemented!()
    }
    fn FORBIDDEN_HELPER(&self) {}
}
fn main() {}
