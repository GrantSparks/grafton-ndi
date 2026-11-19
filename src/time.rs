//! Time values carried by frames and read from the selected runtime's clock.
//!
//! These values count 100-nanosecond ticks, the SDK's unit for time.

use crate::{
    sys::bindings::{NDIlib_recv_timestamp_undefined, NDIlib_send_timecode_synthesize},
    Ndi, Result,
};

/// The editorial timecode a frame carries, in 100-nanosecond ticks.
///
/// Every sendable frame starts with [`Timecode::SYNTHESIZE`], which asks the
/// SDK to generate a timecode when the frame is sent. Any other value,
/// including zero, is sent unchanged. A received frame carries the timecode
/// its sender gave it or the SDK generated.
///
/// # Examples
///
/// ```
/// use grafton_ndi::Timecode;
///
/// let one_second = Timecode::from_ticks(10_000_000);
/// assert_eq!(one_second.ticks(), 10_000_000);
/// assert_eq!(Timecode::default(), Timecode::SYNTHESIZE);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timecode(i64);

impl Timecode {
    /// The SDK's request to generate a timecode when the frame is sent
    /// (`NDIlib_send_timecode_synthesize`, `i64::MAX` ticks).
    pub const SYNTHESIZE: Self = Self(NDIlib_send_timecode_synthesize);

    /// A timecode of `ticks` 100-nanosecond intervals.
    #[must_use]
    pub const fn from_ticks(ticks: i64) -> Self {
        Self(ticks)
    }

    /// The timecode in 100-nanosecond ticks.
    #[must_use]
    pub const fn ticks(self) -> i64 {
        self.0
    }
}

impl Default for Timecode {
    /// [`Timecode::SYNTHESIZE`].
    fn default() -> Self {
        Self::SYNTHESIZE
    }
}

/// A reading of the SDK's clock, in 100-nanosecond ticks since the Unix epoch.
///
/// [`Ndi::timestamp`] reads the clock. Media accessors return
/// [`ReceivedTimestamp`], whose tick origin depends on the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SdkTimestamp(i64);

impl SdkTimestamp {
    /// A timestamp of `ticks` 100-nanosecond intervals since the Unix epoch.
    #[must_use]
    pub const fn from_ticks(ticks: i64) -> Self {
        Self(ticks)
    }

    /// The timestamp in 100-nanosecond ticks since the Unix epoch.
    #[must_use]
    pub const fn ticks(self) -> i64 {
        self.0
    }
}

/// A received media timestamp in signed 100-nanosecond ticks.
///
/// The source and capture path determine the tick origin. For example, on
/// Linux x86-64 with NDI Advanced and the File library 6.4.0.2, seeking a
/// single-track file produced a video frame whose timestamp matched the
/// file-relative PTS in the player's `FirstFrame` event.
///
/// An undefined timestamp is represented as `None` by media accessors.
/// FrameSync and AV-sync audio also treat zero as undefined.
///
/// Order these values only when they share a tick origin. Use [`Self::ticks`]
/// and interpret the ticks for the specific source before comparing them
/// with an [`SdkTimestamp`].
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReceivedTimestamp(i64);

impl ReceivedTimestamp {
    /// The raw signed timestamp in 100-nanosecond ticks from the source.
    #[must_use]
    pub const fn ticks(self) -> i64 {
        self.0
    }

    /// The timestamp field of a frame the SDK delivered: `None` when it holds
    /// the SDK's undefined sentinel (`NDIlib_recv_timestamp_undefined`).
    pub(crate) fn from_sdk(ticks: i64) -> Option<Self> {
        (ticks != NDIlib_recv_timestamp_undefined).then_some(Self(ticks))
    }

    /// The SDK's timestamp field for `timestamp`, the inverse of
    /// [`Self::from_sdk`].
    pub(crate) fn to_sdk(timestamp: Option<Self>) -> i64 {
        timestamp.map_or(NDIlib_recv_timestamp_undefined, Self::ticks)
    }
}

impl Ndi {
    /// Read the selected SDK's current timestamp, retaining its clock semantics.
    ///
    /// Requires the NDI 6.4 timestamp helper. Missing capability is an error;
    /// this never falls back to `SystemTime` or `Instant`.
    #[doc(alias = "NDIlib_util_get_timestamp")]
    pub fn timestamp(&self) -> Result<SdkTimestamp> {
        let timestamp = self.table::<crate::sys::Timestamp>()?;
        // SAFETY: the query takes no arguments, and this handle keeps the
        // selected runtime initialized for the duration of the call.
        Ok(SdkTimestamp(unsafe {
            timestamp.NDIlib_util_get_timestamp()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{capabilities::RuntimeFeature, Error};

    #[test]
    fn received_timestamps_map_the_undefined_sentinel_to_none() {
        assert_eq!(
            ReceivedTimestamp::from_sdk(NDIlib_recv_timestamp_undefined),
            None
        );
        for ticks in [i64::MIN, -1, 0, 1, i64::MAX - 1] {
            let timestamp = ReceivedTimestamp::from_sdk(ticks);
            assert_eq!(timestamp.map(ReceivedTimestamp::ticks), Some(ticks));
            assert_eq!(ReceivedTimestamp::to_sdk(timestamp), ticks);
        }
        assert_eq!(
            ReceivedTimestamp::to_sdk(None),
            NDIlib_recv_timestamp_undefined
        );
        assert_eq!(
            std::mem::size_of::<ReceivedTimestamp>(),
            std::mem::size_of::<SdkTimestamp>()
        );
    }

    #[test]
    fn editorial_timecode_defaults_to_synthesize() {
        assert_eq!(Timecode::SYNTHESIZE.ticks(), i64::MAX);
        assert_eq!(Timecode::default(), Timecode::SYNTHESIZE);
    }

    #[test]
    fn native_timestamp_has_utc_100ns_units_or_reports_missing_capability() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        if ndi.require_feature(RuntimeFeature::SdkTimestamp).is_err() {
            assert!(matches!(
                ndi.timestamp(),
                Err(Error::UnsupportedCapability { .. })
            ));
            return;
        }
        let clock_ticks = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
                / 100
        };
        let before = clock_ticks() as i128;
        let ticks = i128::from(ndi.timestamp().unwrap().ticks());
        let after = clock_ticks() as i128;
        // Allow one second for native/system clock sampling differences. This
        // catches unit or epoch mistakes without relying on subsecond equality.
        assert!((before - 10_000_000..=after + 10_000_000).contains(&ticks));
    }
}
