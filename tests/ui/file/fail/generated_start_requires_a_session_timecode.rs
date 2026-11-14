use grafton_ndi::{file::RecordingTimecode, Timecode};

fn main() {
    let _ = RecordingTimecode::GenerateFrom(Timecode::SYNTHESIZE);
    let _ = RecordingTimecode::GenerateFrom(Timecode::default());
}
