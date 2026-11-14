//! Storage for the structures the File library fills in through a pointer.

use crate::{Error, Result};

/// Bytes kept after every structure the File library writes.
const GUARD: usize = 4096;

/// The value every guard byte holds while the guard is intact.
const SENTINEL: u8 = 0xA5;

/// A zeroed `T` for the File library to fill in, followed by a guard region.
///
/// The library writes these structures through a bare pointer, with the size
/// they had in the headers it was built from. It is located at run time and
/// reports no version, so that size cannot be compared with the layout this
/// crate was built with before the call. Should a later library have grown a
/// structure, the extra bytes land in the guard rather than past the end of the
/// storage, and [`OutParam::read`] reports the mismatch instead of returning a
/// value decoded with the wrong layout.
#[repr(C)]
pub(super) struct OutParam<T> {
    value: T,
    guard: [u8; GUARD],
}

impl<T: Copy + Default> OutParam<T> {
    /// Fresh storage holding `T::default()`, which is all-zero for SDK structures.
    pub(super) fn new() -> Self {
        Self {
            value: T::default(),
            guard: [SENTINEL; GUARD],
        }
    }

    /// The pointer to pass to the File library.
    ///
    /// It is derived from the whole storage, so a write that runs past `T`
    /// stays inside the allocation the pointer is valid for.
    pub(super) fn as_mut_ptr(&mut self) -> *mut T {
        std::ptr::from_mut(self).cast()
    }

    /// The structure the library wrote.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedSdkResult`] when the library wrote more than this crate's
    /// layout of `T`, in which case nothing it wrote can be trusted.
    pub(super) fn read(&self) -> Result<T> {
        if self.guard.iter().any(|&byte| byte != SENTINEL) {
            return Err(Error::UnexpectedSdkResult(format!(
                "the File library wrote past the {} bytes this crate's SDK headers give {}",
                size_of::<T>(),
                std::any::type_name::<T>()
            )));
        }
        Ok(self.value)
    }
}

/// Fill guarded storage, then honor the native call's availability result.
///
/// The closure receives a writable structure for the duration of the call;
/// its other native arguments must remain valid through that call. Check the
/// guard even on `false`: a library with a different layout cannot be trusted
/// merely because it reported no result.
pub(super) fn checked_fill<T: Copy + Default>(
    call: impl FnOnce(*mut T) -> bool,
) -> Result<Option<T>> {
    let mut out = OutParam::<T>::new();
    let available = call(out.as_mut_ptr());
    let value = out.read()?;
    Ok(available.then_some(value))
}

/// Capture a raw event without decoding its payload. Recorder callers must
/// observe it in the session ledger before any fallible payload conversion.
///
/// As for `checked_fill`, the closure receives guarded writable storage for
/// the call. A `none` return takes precedence over its stored event type, but
/// never over a damaged guard.
pub(super) fn capture_event<T: Copy + Default, K: PartialEq>(
    call: impl FnOnce(*mut T) -> K,
    event_type: impl FnOnce(&T) -> K,
    none: K,
    mismatch: &'static str,
) -> Result<Option<T>> {
    let mut out = OutParam::<T>::new();
    let kind = call(out.as_mut_ptr());
    let event = out.read()?;
    if kind == none {
        return Ok(None);
    }
    if kind != event_type(&event) {
        return Err(Error::UnexpectedSdkResult(mismatch.into()));
    }
    Ok(Some(event))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::{self, bindings::*};

    #[test]
    fn a_write_within_the_layout_is_returned() {
        let mut out = OutParam::<u64>::new();
        // SAFETY: the pointer is valid for one u64.
        unsafe { out.as_mut_ptr().write(7) };
        assert_eq!(out.read().unwrap(), 7);
    }

    #[test]
    fn a_library_that_writes_a_larger_structure_is_detected_not_trusted() {
        let mut out = OutParam::<NDIlib_file_recorder_event_t>::new();
        let larger = size_of::<NDIlib_file_recorder_event_t>() + 24;
        // SAFETY: the pointer is valid for the structure and its guard, and
        // `larger` stays within them.
        unsafe { out.as_mut_ptr().cast::<u8>().write_bytes(0, larger) };
        let error = out.read().unwrap_err();
        assert!(matches!(error, Error::UnexpectedSdkResult(_)), "{error}");
    }

    #[test]
    fn the_guard_starts_where_the_structure_ends() {
        let out = OutParam::<NDIlib_file_recorder_event_t>::new();
        let start = std::ptr::from_ref(&out) as usize;
        assert_eq!(
            out.guard.as_ptr() as usize - start,
            size_of::<NDIlib_file_recorder_event_t>()
        );
    }

    #[test]
    fn checked_fill_returns_available_storage_without_allocating() {
        let (result, allocations) = crate::test_alloc::allocation_count(|| {
            checked_fill(|out: *mut u64| {
                // SAFETY: the helper provides initialized, writable storage.
                unsafe { out.write(7) };
                true
            })
        });
        assert_eq!(result.unwrap(), Some(7));
        assert_eq!(allocations, 0);
        assert_eq!(checked_fill(|_: *mut u64| false).unwrap(), None);
    }

    /// Damage the first guard byte while staying within the helper's storage.
    ///
    /// # Safety
    ///
    /// `out` must point into an `OutParam<T>` supplied by a helper.
    unsafe fn damage_guard<T>(out: *mut T) {
        // SAFETY: the whole guarded allocation supplies the pointer's
        // provenance, and the first guard byte follows the T it contains.
        unsafe { out.cast::<u8>().add(size_of::<T>()).write(0) };
    }

    fn assert_guard_damage<T: std::fmt::Debug>(result: Result<T>) {
        assert!(
            matches!(result, Err(Error::UnexpectedSdkResult(message)) if message.contains("wrote past")),
            "a damaged guard must be checked before the native result"
        );
    }

    #[test]
    fn checked_fill_checks_the_guard_even_when_the_native_result_is_false() {
        for available in [false, true] {
            assert_guard_damage(checked_fill(|out: *mut u64| {
                // SAFETY: this pointer comes from the guarded fill helper.
                unsafe { damage_guard(out) };
                available
            }));
        }
    }

    #[test]
    fn capture_checks_the_guard_before_none_or_a_stored_type_mismatch() {
        fn check<T: Copy + Default + std::fmt::Debug>() {
            for kind in [0, 1] {
                assert_guard_damage(capture_event(
                    |out: *mut T| {
                        // SAFETY: this pointer comes from the guarded capture helper.
                        unsafe { damage_guard(out) };
                        kind
                    },
                    |_| panic!("a damaged event cannot be inspected"),
                    0,
                    "mismatched event types",
                ));
            }
        }
        check::<NDIlib_file_player_event_t>();
        check::<NDIlib_file_recorder_event_t>();
    }

    #[test]
    fn a_none_event_does_not_inspect_the_stored_event_type() {
        let result = capture_event(
            |out: *mut u32| {
                // SAFETY: the helper provides initialized, writable storage.
                unsafe { out.write(3) };
                0
            },
            |_| panic!("a none result does not expose an event"),
            0,
            "mismatched event types",
        );
        assert_eq!(result.unwrap(), None);
    }

    #[test]
    fn event_type_mismatch_is_diagnosed_for_both_native_handles() {
        use crate::file::{mock, FileApi, PlayerOptions, RecorderOptions, SourceSpec};

        unsafe extern "C" fn player_create(
            _: *const NDIlib_file_player_create_t,
        ) -> NDIlib_file_player_instance_t {
            std::ptr::NonNull::dangling().as_ptr()
        }
        unsafe extern "C" fn recorder_create(
            _: *const NDIlib_file_recorder_create_t,
        ) -> NDIlib_file_recorder_instance_t {
            std::ptr::NonNull::dangling().as_ptr()
        }
        unsafe extern "C" fn player_mismatch(
            _: NDIlib_file_player_instance_t,
            event: *mut NDIlib_file_player_event_t,
            _: u32,
        ) -> NDIlib_file_player_event_type_e {
            // SAFETY: the capture helper provides writable player-event storage.
            unsafe {
                event.write(NDIlib_file_player_event_t {
                    type_: NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_ended,
                    ..Default::default()
                })
            };
            NDIlib_file_player_event_type_e_NDIlib_file_player_event_type_file_opened
        }
        unsafe extern "C" fn recorder_mismatch(
            _: NDIlib_file_recorder_instance_t,
            event: *mut NDIlib_file_recorder_event_t,
            _: u32,
        ) -> NDIlib_file_recorder_event_type_e {
            // SAFETY: the capture helper provides writable recorder-event storage.
            unsafe { event.write(mock::recorder_event(mock::STARTED, "take.mkv")) };
            mock::FILE_CLOSED
        }
        let api = FileApi::with_tables(
            sys::FilePlayback {
                NDIlib_file_player_create: player_create,
                NDIlib_file_player_capture_event: player_mismatch,
                ..mock::playback()
            },
            sys::FileRecording {
                NDIlib_file_recorder_create: recorder_create,
                NDIlib_file_recorder_capture_event: recorder_mismatch,
                ..mock::recording()
            },
        );
        let mut player = api
            .create_player(&PlayerOptions::new("mock player"))
            .unwrap();
        let mut recorder = api
            .create_recorder(&RecorderOptions::new(
                SourceSpec::new("mock source").unwrap(),
            ))
            .unwrap();
        for (result, owner) in [
            (
                player.poll_event(std::time::Duration::ZERO).map(|_| ()),
                "player",
            ),
            (
                recorder.poll_event(std::time::Duration::ZERO).map(|_| ()),
                "recorder",
            ),
        ] {
            let Err(Error::UnexpectedSdkResult(message)) = result else {
                panic!("the {owner} must refuse mismatched event types: {result:?}");
            };
            assert_eq!(
                message,
                format!("the {owner} returned one event type and stored another")
            );
        }
    }
}
