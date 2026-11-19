use grafton_ndi::{
    AudioChannels, AudioFormat, AudioFrame, AudioSource, ColorInfo, FrameMetadataStatus, FrameRate,
    PictureAspectRatio, PixelFormat, Result, ScanType, ReceivedTimestamp, Timecode, VideoFrame,
    VideoPlane, VideoPlanes, VideoSource,
};

struct Unchecked;

// Every public method is implemented. Rejection must come from the private
// validated-parts supertraits, rather than an incomplete public interface.
impl VideoSource for Unchecked {
    fn width(&self) -> u32 {
        unreachable!()
    }
    fn height(&self) -> u32 {
        unreachable!()
    }
    fn pixel_format(&self) -> PixelFormat {
        unreachable!()
    }
    fn frame_rate(&self) -> FrameRate {
        unreachable!()
    }
    fn picture_aspect_ratio(&self) -> PictureAspectRatio {
        unreachable!()
    }
    fn scan_type(&self) -> ScanType {
        unreachable!()
    }
    fn timecode(&self) -> Timecode {
        unreachable!()
    }
    fn timestamp(&self) -> Option<ReceivedTimestamp> {
        unreachable!()
    }
    fn line_stride_bytes(&self) -> usize {
        unreachable!()
    }
    fn metadata(&self) -> Option<&str> {
        unreachable!()
    }
    fn metadata_status(&self) -> FrameMetadataStatus {
        unreachable!()
    }
    fn color_info(&self) -> Result<Option<ColorInfo>> {
        unreachable!()
    }
    fn plane(&self, _index: usize) -> Option<VideoPlane<'_>> {
        unreachable!()
    }
    fn planes(&self) -> VideoPlanes<'_> {
        unreachable!()
    }
    fn data(&self) -> Option<&[u8]> {
        unreachable!()
    }
    fn to_owned(&self) -> VideoFrame {
        unreachable!()
    }
    fn copy_into(&self, _destination: &mut VideoFrame) {
        unreachable!()
    }
    fn sends_in_place(&self) -> bool {
        unreachable!()
    }
    #[cfg(feature = "image-encoding")]
    fn encode_image(
        &self,
        _options: &grafton_ndi::image_encoding::ImageOptions,
    ) -> Result<Vec<u8>> {
        unreachable!()
    }
    #[cfg(feature = "image-encoding")]
    fn encode_image_to<W: std::io::Write>(
        &self,
        _options: &grafton_ndi::image_encoding::ImageOptions,
        _out: W,
    ) -> Result<()>
    where
        Self: Sized,
    {
        unreachable!()
    }
    #[cfg(feature = "image-encoding")]
    fn encode_image_data_url(
        &self,
        _options: &grafton_ndi::image_encoding::ImageOptions,
    ) -> Result<String> {
        unreachable!()
    }
}

impl AudioSource for Unchecked {
    fn sample_rate(&self) -> u32 {
        unreachable!()
    }
    fn num_channels(&self) -> u32 {
        unreachable!()
    }
    fn num_samples(&self) -> u32 {
        unreachable!()
    }
    fn format(&self) -> AudioFormat {
        unreachable!()
    }
    fn channel_stride_bytes(&self) -> usize {
        unreachable!()
    }
    fn timecode(&self) -> Timecode {
        unreachable!()
    }
    fn timestamp(&self) -> Option<ReceivedTimestamp> {
        unreachable!()
    }
    fn metadata(&self) -> Option<&str> {
        unreachable!()
    }
    fn metadata_status(&self) -> FrameMetadataStatus {
        unreachable!()
    }
    fn channel_data(&self, _channel: usize) -> Option<&[f32]> {
        unreachable!()
    }
    fn channels(&self) -> AudioChannels<'_> {
        unreachable!()
    }
    fn data(&self) -> Option<&[f32]> {
        unreachable!()
    }
    fn to_owned(&self) -> AudioFrame {
        unreachable!()
    }
    fn copy_into(&self, _destination: &mut AudioFrame) {
        unreachable!()
    }
}

fn main() {}
