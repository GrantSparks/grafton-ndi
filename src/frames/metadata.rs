//! Metadata's bounded UTF-8 and sole trailing NUL are established here.
use super::{FrameMetadataStatus, MAX_METADATA_BYTES};
use crate::sdk_copy::bounded_sdk_text;
use crate::{sys::bindings::NDIlib_metadata_frame_t, Error, Rejected, Result};
use std::{
    ffi::{CStr, CString},
    fmt,
    os::raw::c_char,
    ptr, slice, str,
};

/// Validated metadata text, borrowed together with its terminator: the
/// pointer and length of a C string that is known to be valid.
///
/// `with_nul` is UTF-8 whose only NUL byte is its last, and it is at most
/// [`MAX_METADATA_BYTES`] long. `EMPTY`, `from_cstr`, `from_terminated_bytes`,
/// the bounded C-string observation, and the unsafe `reborrow` establish this
/// invariant. Owned text is validated by `TerminatedText`; a cached SDK
/// observation retains its measured extent.
/// Absent text (a null pointer, or a
/// standalone frame's empty null frame) is `None` of
/// `Option<MetadataText>`; an explicit, non-null empty C string is `"\0"`.
///
/// It is a shared string borrow, so a frame that holds one keeps its thread
/// and unwind auto traits.
#[derive(Clone, Copy)]
pub(crate) struct MetadataText<'a> {
    with_nul: &'a str,
}

impl<'a> MetadataText<'a> {
    pub(crate) const EMPTY: Self = Self { with_nul: "\0" };

    pub(super) fn from_terminated_bytes(bytes: &'a [u8]) -> Result<Self> {
        validate_metadata_len_with_nul(bytes.len())?;
        let Some((&0, payload)) = bytes.split_last() else {
            return Err(Error::InvalidFrame(
                crate::FrameDefect::MetadataMissingTerminator,
            ));
        };
        if payload.contains(&0) {
            return Err(Error::InvalidFrame(crate::FrameDefect::MetadataInteriorNul));
        }
        let with_nul = str::from_utf8(bytes).map_err(Error::InvalidUtf8)?;
        Ok(Self { with_nul })
    }

    /// Reborrow bytes already measured as bounded, solely terminated UTF-8.
    ///
    /// # Safety
    /// `pointer` must retain that same observation unchanged for `'a`,
    /// including all `len_with_nul` bytes and the terminal NUL.
    unsafe fn reborrow(pointer: *const c_char, len_with_nul: usize) -> Self {
        // SAFETY: the caller retains the validated bytes for the borrow.
        let with_nul = unsafe {
            str::from_utf8_unchecked(slice::from_raw_parts(pointer.cast(), len_with_nul))
        };
        Self { with_nul }
    }

    /// Validate application metadata: its length and UTF-8. A `CStr` ends in
    /// its only NUL, so its known length bounds every check, and nothing
    /// scans raw memory.
    pub(crate) fn from_cstr(metadata: &'a CStr) -> Result<Self> {
        let with_nul = metadata.to_bytes_with_nul();
        validate_metadata_len_with_nul(with_nul.len())?;
        let with_nul = str::from_utf8(with_nul).map_err(Error::InvalidUtf8)?;
        Ok(Self { with_nul })
    }

    #[cfg(test)]
    pub(super) fn as_cstr(self) -> &'a CStr {
        CStr::from_bytes_with_nul(self.with_nul.as_bytes())
            .expect("validated metadata ends in its only NUL byte")
    }

    /// The text, without its terminator.
    pub(crate) fn as_str(self) -> &'a str {
        // The last byte is the one-byte NUL character, so this is a
        // character boundary.
        &self.with_nul[..self.with_nul.len() - 1]
    }

    /// The text as a C string for the SDK: the terminator follows it.
    pub(crate) fn as_ptr(self) -> *const c_char {
        self.with_nul.as_ptr().cast()
    }

    /// The text's length, terminator included.
    pub(crate) fn len_with_nul(self) -> usize {
        self.with_nul.len()
    }

    /// The checked text length in the SDK's signed descriptor field.
    pub(crate) fn sdk_len(self) -> i32 {
        // MetadataText's constructors bound this length by MAX_METADATA_BYTES;
        // frames asserts that the cap is smaller than i32::MAX.
        self.with_nul.len() as i32
    }

    /// Complete the UTF-8 check after bounded_sdk_text has established the
    /// cap and the sole terminal NUL. Used only after that bounded observation.
    fn from_measured_bytes(bytes: &'a [u8]) -> Result<Self> {
        let with_nul = str::from_utf8(bytes).map_err(Error::InvalidUtf8)?;
        Ok(Self { with_nul })
    }
}

impl fmt::Debug for MetadataText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("MetadataText").field(&self.as_str()).finish()
    }
}

/// Validate standalone metadata frame layout from raw FFI fields.
///
/// The SDK `length` field includes the trailing NUL terminator. The accepted
/// empty null frame (`length == 0 && p_data == NULL`) is `None` and never
/// requires reading from `p_data`.
///
/// This validates only memory layout and UTF-8. XML well-formedness is
/// intentionally not checked because NDI receivers are expected to tolerate
/// badly formed XML metadata.
///
/// # Safety
///
/// `raw.p_data`, when non-null, must reference readable storage for the
/// declared length or a NUL-terminated string within the metadata cap, live
/// and unchanged for as long as `raw` is borrowed.
pub(crate) unsafe fn validate_metadata_layout(
    raw: &NDIlib_metadata_frame_t,
) -> Result<Option<MetadataText<'_>>> {
    let len_with_nul = validate_metadata_prefix(raw)?;

    if len_with_nul == 0 {
        if raw.p_data.is_null() {
            return Ok(None);
        }

        // The SDK permits zero length as a C-string sentinel. Apply the same
        // bounded scan and SDK pointer-validity contract as per-frame metadata;
        // a non-null pointer validates to `Some`.
        // SAFETY: the caller keeps a non-null `p_data` of zero length
        // readable up to its first NUL or the metadata cap, and unchanged,
        // while `raw` is borrowed.
        return unsafe { validate_frame_metadata(raw.p_data) };
    }

    // SAFETY: `p_data` is non-null, and the caller keeps the declared
    // `len_with_nul` bytes at it readable and unchanged while `raw` is
    // borrowed; `u8` needs no alignment, and the length is within the
    // metadata cap.
    let bytes = unsafe { slice::from_raw_parts(raw.p_data.cast::<u8>(), len_with_nul) };
    MetadataText::from_terminated_bytes(bytes).map(Some)
}

/// Validate metadata returned specifically by `NDIlib_send_capture`.
///
/// `Processing.NDI.structs.h` independently guarantees NUL-terminated UTF-8
/// `p_data`, and the SDK's Send_VirtualPTZ sample reads captured sender metadata
/// as a C string. Positive reported counts are checked for invalid values and
/// excessive size, but are not treated as exact byte extents: native captures
/// can report a count that does not include the string's terminator.
///
/// Unlike [`validate_metadata_layout`], this function requires the SDK's full
/// C-string guarantee even for positive lengths. The declared-length validator
/// remains strict for descriptors promising only that many readable bytes.
///
/// # Safety
///
/// A non-null `p_data` must be readable byte-by-byte through its first NUL
/// terminator or the metadata cap, live and unchanged while `raw` is borrowed.
/// In particular, `length` readable bytes alone do not satisfy this contract.
pub(crate) unsafe fn validate_sender_metadata(
    raw: &NDIlib_metadata_frame_t,
) -> Result<Option<MetadataText<'_>>> {
    validate_metadata_prefix(raw)?;
    // SAFETY: the caller supplies the stronger SDK C-string contract regardless
    // of the reported count. The shared scan stops at the first NUL or the cap.
    unsafe { validate_frame_metadata(raw.p_data) }
}

/// Check descriptor fields before either bounded-byte or native C-string
/// validation reads a pointer. Zero retains the SDK's lengthless sentinel.
fn validate_metadata_prefix(raw: &NDIlib_metadata_frame_t) -> Result<usize> {
    if raw.length < 0 {
        return Err(Error::InvalidFrame(
            crate::FrameDefect::MetadataNegativeLength { length: raw.length },
        ));
    }

    if raw.length == 0 {
        return Ok(0);
    }

    if raw.p_data.is_null() {
        return Err(Error::InvalidFrame(crate::FrameDefect::MetadataNullData));
    }

    // Lossless: the signed SDK length is positive and usize is at least 32 bits.
    let len_with_nul = raw.length as usize;
    validate_metadata_len_with_nul(len_with_nul)?;
    Ok(len_with_nul)
}

fn validate_metadata_len_with_nul(len_with_nul: usize) -> Result<()> {
    debug_assert!(len_with_nul > 0, "metadata includes its terminator");

    if len_with_nul > MAX_METADATA_BYTES {
        return Err(Error::InvalidFrame(crate::FrameDefect::MetadataTooLarge {
            length: len_with_nul,
        }));
    }

    Ok(())
}

/// Why optional metadata was withheld while retaining otherwise valid media.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum MetadataWithheld {
    NotUtf8,
    Unterminated,
}

impl MetadataWithheld {
    const fn status(self) -> FrameMetadataStatus {
        match self {
            Self::NotUtf8 => FrameMetadataStatus::NotUtf8,
            Self::Unterminated => FrameMetadataStatus::Unterminated,
        }
    }
}

/// One metadata observation: readable text is carried by the state that says
/// it is readable, so consumers cannot pair `Text` with absent or withheld text.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FrameMetadataRef<'a> {
    Absent,
    Text(MetadataText<'a>),
    Withheld(MetadataWithheld),
}

impl<'a> FrameMetadataRef<'a> {
    pub(crate) fn from_text(text: Option<MetadataText<'a>>) -> Self {
        text.map_or(Self::Absent, Self::Text)
    }

    pub(crate) const fn status(self) -> FrameMetadataStatus {
        match self {
            Self::Absent => FrameMetadataStatus::Absent,
            Self::Text(_) => FrameMetadataStatus::Text,
            Self::Withheld(reason) => reason.status(),
        }
    }

    pub(crate) const fn text(self) -> Option<MetadataText<'a>> {
        match self {
            Self::Text(text) => Some(text),
            Self::Absent | Self::Withheld(_) => None,
        }
    }

    pub(crate) fn as_ptr(self) -> *const c_char {
        self.text().map_or(ptr::null(), MetadataText::as_ptr)
    }
}

/// Cached validation of one optional metadata pointer. Only readable text
/// carries a length; the other states cannot accidentally reborrow bytes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FrameMetadataState(MetadataScan);

#[derive(Debug, Clone, Copy)]
enum MetadataScan {
    Absent,
    Text { len_with_nul: usize },
    Withheld(MetadataWithheld),
}

impl FrameMetadataState {
    /// Scan once without allocation, retaining media when its side text is bad.
    ///
    /// # Safety
    ///
    /// A non-null pointer must be readable through its first NUL or the
    /// metadata cap, whichever comes first.
    pub(super) unsafe fn scan(pointer: *const c_char) -> Self {
        if pointer.is_null() {
            return Self(MetadataScan::Absent);
        }
        // SAFETY: the caller guarantees the bounded readable extent.
        let Some(bytes) = (unsafe { bounded_sdk_text(pointer, MAX_METADATA_BYTES) }) else {
            return Self(MetadataScan::Withheld(MetadataWithheld::Unterminated));
        };
        if str::from_utf8(bytes).is_ok() {
            Self(MetadataScan::Text {
                len_with_nul: bytes.len() + 1,
            })
        } else {
            Self(MetadataScan::Withheld(MetadataWithheld::NotUtf8))
        }
    }

    /// Cache text that was already validated, including standalone metadata.
    pub(crate) fn from_text(text: Option<MetadataText<'_>>) -> Self {
        Self(
            text.map_or(MetadataScan::Absent, |text| MetadataScan::Text {
                len_with_nul: text.len_with_nul(),
            }),
        )
    }

    /// Reborrow the one validated metadata observation without rescanning.
    ///
    /// # Safety
    ///
    /// `pointer` must name the same bytes used to construct this state, live
    /// and unchanged for `'a`, including the measured terminal NUL.
    pub(super) unsafe fn view<'a>(self, pointer: *const c_char) -> FrameMetadataRef<'a> {
        match self.0 {
            MetadataScan::Absent => FrameMetadataRef::Absent,
            MetadataScan::Withheld(reason) => FrameMetadataRef::Withheld(reason),
            MetadataScan::Text { len_with_nul } => {
                // SAFETY: construction validated UTF-8 and the terminator;
                // the caller retains those same bytes for the returned borrow.
                FrameMetadataRef::Text(unsafe { MetadataText::reborrow(pointer, len_with_nul) })
            }
        }
    }
}

/// Always-present owned UTF-8 text, including its sole terminal NUL. Unlike
/// optional frame metadata this type has no empty-buffer/absent state.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct TerminatedText {
    inner: String,
}

impl TerminatedText {
    pub(crate) fn new<S: Into<String>>(metadata: S) -> std::result::Result<Self, Rejected<String>> {
        let text = metadata.into();
        if let Err(error) = Self::validate(&text) {
            return Err(Rejected::new(error, text));
        }
        Ok(Self::terminate(text))
    }

    fn validate(text: &str) -> Result<()> {
        // A String's length is at most isize::MAX, so adding its terminator
        // cannot overflow usize on any supported target.
        let len_with_nul = text.len() + 1;
        validate_metadata_len_with_nul(len_with_nul)?;
        if let Some(position) = text.as_bytes().iter().position(|&byte| byte == 0) {
            #[expect(
                clippy::expect_used,
                reason = "the searched prefix includes its first NUL; NulError has no public constructor"
            )]
            return Err(Error::InvalidCString(
                CString::new(&text.as_bytes()[..=position])
                    .expect_err("the prefix includes its first NUL"),
            ));
        }
        Ok(())
    }

    // Only called with validated text owned by this module. Reserving exactly
    // one byte preserves the builder's allocation when it already has space.
    fn terminate(mut text: String) -> Self {
        text.reserve_exact(1);
        text.push('\0');
        Self { inner: text }
    }

    pub(crate) fn text(&self) -> MetadataText<'_> {
        MetadataText {
            with_nul: &self.inner,
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        self.text().as_str()
    }

    pub(crate) fn into_string(mut self) -> String {
        self.inner.pop();
        self.inner
    }

    /// Adopt retained storage without exposing a partially terminated value.
    fn adopt(storage: &mut String, text: MetadataText<'_>) -> Self {
        storage.reserve_exact(text.len_with_nul().saturating_sub(storage.len()));
        storage.clear();
        storage.push_str(text.with_nul);
        Self {
            inner: std::mem::take(storage),
        }
    }

    fn into_storage(self) -> String {
        self.inner
    }

    #[cfg(any(test, feature = "advanced"))]
    pub(super) fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    #[cfg(test)]
    pub(super) fn terminated_bytes(&self) -> &[u8] {
        self.inner.as_bytes()
    }

    pub(super) fn copied(text: MetadataText<'_>) -> Self {
        Self {
            inner: text.with_nul.to_owned(),
        }
    }

    pub(super) fn copy_text(&mut self, text: MetadataText<'_>) {
        self.inner
            .reserve_exact(text.len_with_nul().saturating_sub(self.inner.len()));
        self.inner.clear();
        self.inner.push_str(text.with_nul);
    }
}

impl fmt::Debug for TerminatedText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("TerminatedText")
            .field(&self.as_str())
            .finish()
    }
}

/// Owned optional metadata. Each state carries its storage, keeping readable
/// text terminated by construction while retaining spare capacity in absent
/// and withheld states for later copies.
#[derive(Clone, Debug)]
pub(crate) enum OwnedFrameMetadata {
    Absent(String),
    Text(TerminatedText),
    Withheld {
        reason: MetadataWithheld,
        storage: String,
    },
}

impl Default for OwnedFrameMetadata {
    fn default() -> Self {
        Self::Absent(String::new())
    }
}

/// The builder's untouched text after the bounded UTF-8 C string check.
pub(super) struct CheckedMetadata(Option<String>);

impl CheckedMetadata {
    /// On refusal, the caller still owns the original text and allocation.
    pub(super) fn take_from(metadata: &mut Option<String>) -> Result<Self> {
        if let Some(text) = metadata.as_deref() {
            TerminatedText::validate(text)?;
        }
        Ok(Self(metadata.take()))
    }

    pub(super) fn into_owned(self) -> OwnedFrameMetadata {
        self.0
            .map(TerminatedText::terminate)
            .map_or_else(OwnedFrameMetadata::default, OwnedFrameMetadata::Text)
    }
}

impl OwnedFrameMetadata {
    pub(crate) fn new(metadata: impl Into<String>) -> std::result::Result<Self, Rejected<String>> {
        Ok(Self::Text(TerminatedText::new(metadata)?))
    }

    pub(crate) fn view(&self) -> FrameMetadataRef<'_> {
        match self {
            Self::Absent(_) => FrameMetadataRef::Absent,
            Self::Text(text) => FrameMetadataRef::Text(text.text()),
            Self::Withheld { reason, .. } => FrameMetadataRef::Withheld(*reason),
        }
    }

    pub(crate) fn copied(view: FrameMetadataRef<'_>) -> Self {
        let mut metadata = Self::default();
        metadata.copy_from(view);
        metadata
    }

    /// Reserve before mutation, so a failed allocation leaves the original
    /// metadata complete. Once capacity suffices, every state transition is
    /// allocation-free, including through absent and withheld observations.
    pub(crate) fn copy_from(&mut self, view: FrameMetadataRef<'_>) {
        match view {
            FrameMetadataRef::Text(text) => match self {
                Self::Text(current) => current.copy_text(text),
                Self::Absent(storage) | Self::Withheld { storage, .. } => {
                    *self = Self::Text(TerminatedText::adopt(storage, text));
                }
            },
            FrameMetadataRef::Absent | FrameMetadataRef::Withheld(_) => {
                let mut storage = match std::mem::take(self) {
                    Self::Absent(storage) | Self::Withheld { storage, .. } => storage,
                    Self::Text(text) => text.into_storage(),
                };
                storage.clear();
                *self = if let FrameMetadataRef::Withheld(reason) = view {
                    Self::Withheld { reason, storage }
                } else {
                    Self::Absent(storage)
                };
            }
        }
    }

    pub(crate) fn set(
        &mut self,
        text: impl Into<String>,
    ) -> std::result::Result<(), Rejected<String>> {
        *self = Self::new(text)?;
        Ok(())
    }

    pub(crate) fn clear(&mut self) {
        self.copy_from(FrameMetadataRef::Absent);
    }

    #[cfg(any(test, feature = "advanced"))]
    pub(crate) fn retained_bytes(&self) -> usize {
        match self {
            Self::Absent(storage) | Self::Withheld { storage, .. } => storage.capacity(),
            Self::Text(text) => text.capacity(),
        }
    }
}

/// Validate a video/audio per-frame `p_metadata` pointer with a bounded scan.
///
/// The SDK exposes these pointers as optional lengthless C strings. A null
/// pointer is `None`; otherwise the result records the first terminator found
/// by the scan.
///
/// This continues to trust the NDI SDK that a non-null pointer is readable. The
/// validation does not make dangling or otherwise invalid pointers safe; it
/// only limits the scan to `MAX_METADATA_BYTES`, requires a terminator within
/// that cap, and validates UTF-8 before exposing safe text access.
///
/// # Safety
///
/// When non-null, `p_metadata` must be readable byte-by-byte until either the
/// first NUL terminator or `MAX_METADATA_BYTES` bytes have been read, and
/// the bytes it reads must stay live and unchanged for `'a`.
pub(crate) unsafe fn validate_frame_metadata<'a>(
    p_metadata: *const c_char,
) -> Result<Option<MetadataText<'a>>> {
    if p_metadata.is_null() {
        return Ok(None);
    }

    // SAFETY: forwarded to the caller's obligation for a non-null pointer.
    let Some(payload) = (unsafe { bounded_sdk_text(p_metadata, MAX_METADATA_BYTES) }) else {
        return Err(Error::InvalidFrame(
            crate::FrameDefect::MetadataUnterminated,
        ));
    };
    // SAFETY: the scan just read these bytes and the NUL after them, all
    // within the cap, from the non-null `p_metadata`, which the caller keeps
    // live and unchanged for `'a`; `u8` needs no alignment.
    let bytes = unsafe { slice::from_raw_parts(p_metadata.cast::<u8>(), payload.len() + 1) };
    MetadataText::from_measured_bytes(bytes).map(Some)
}
