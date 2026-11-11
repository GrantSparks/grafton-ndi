//! Timestamps read from the selected NDI runtime's clock.

use crate::{capabilities::RuntimeFeature, Error, Result, NDI};

/// An SDK clock reading in signed 100-nanosecond ticks since the Unix epoch.
///
/// This is distinct from synthesized frame timecodes. Existing frame timestamp
/// setters accept [`Self::ticks_100ns`]; no application clock is substituted when
/// the native timestamp helper is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SdkTimestamp(i64);

impl SdkTimestamp {
    /// Return the timestamp in the native SDK's 100-nanosecond units.
    #[must_use]
    pub const fn ticks_100ns(self) -> i64 {
        self.0
    }
}

impl NDI {
    /// Read the selected SDK's current timestamp, retaining its clock semantics.
    ///
    /// Requires the NDI 6.4 timestamp helper. Missing capability is an error;
    /// this never falls back to `SystemTime` or `Instant`.
    pub fn timestamp(&self) -> Result<SdkTimestamp> {
        self.require_feature(RuntimeFeature::SdkTimestamp)?;
        type GetTimestamp = unsafe extern "C" fn() -> i64;
        // SAFETY: This exact signature is declared by Processing.NDI.utilities.h.
        let get = unsafe { self.resolve_optional::<GetTimestamp>(b"NDIlib_util_get_timestamp\0")? }
            .ok_or_else(|| Error::UnsupportedCapability {
                capability: "SDK timestamp".into(),
                reason: "NDIlib_util_get_timestamp is unavailable in the selected runtime".into(),
            })?;
        // SAFETY: The selected runtime remains initialized through this NDI borrow.
        Ok(SdkTimestamp(unsafe { get() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "Requires real NDI SDK and reviewed network confinement"]
    fn native_timestamp_has_utc_100ns_units_or_reports_missing_capability() {
        let ndi = NDI::new().unwrap();
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
        let ticks = i128::from(ndi.timestamp().unwrap().ticks_100ns());
        let after = clock_ticks() as i128;
        // Allow one second for native/system clock sampling differences. This
        // catches unit or epoch mistakes without relying on subsecond equality.
        assert!((before - 10_000_000..=after + 10_000_000).contains(&ticks));
    }
}
