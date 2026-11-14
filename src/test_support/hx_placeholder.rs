//! Recognize the picture used when an NDI|HX decoder is missing.

/// The size of the black frames the runtime delivers in place of NDI|HX video
/// it has no decoder for, whatever the size of the video.
const HX_PLACEHOLDER: (u32, u32) = (1920, 1080);

/// Whether a frame of size `received`, captured from NDI|HX video sent at size
/// `sent`, is the runtime's black placeholder for a picture it cannot decode.
///
/// On Linux without a decoder, and on the hosted Windows Server 2025 runners
/// with HEVC 4:4:4, the runtime delivers 1920x1080 frames, and no error, in
/// place of the picture. A test that receives NDI|HX video counts
/// these and, when nothing else arrived, reports the decoder for its codec
/// missing with [`skip`]; any other frame is the decoded picture and is
/// checked as such.
///
/// # Panics
///
/// Panics when `sent` is itself 1920x1080: the placeholder could not then be
/// told from the picture.
pub fn is_hx_placeholder(sent: (u32, u32), received: (u32, u32)) -> bool {
    assert_ne!(
        sent, HX_PLACEHOLDER,
        "NDI|HX video sent at 1920x1080 cannot be told from the runtime's placeholder"
    );
    received == HX_PLACEHOLDER
}
