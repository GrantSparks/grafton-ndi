use super::*;
use crate::file::{EventText, RecorderEventKind};
use std::{ffi::c_char, mem::size_of};

fn indexed_path(index: usize, len: usize) -> EventText {
    // Keep the decimal-free prefix ordered so these long traces exercise
    // admission and matching rather than sorting a generated workload.
    let mut buffer = [b'x' as c_char; 4096];
    for (offset, slot) in buffer[..16].iter_mut().enumerate() {
        let nibble = (index as u64 >> ((15 - offset) * 4)) & 0xf;
        *slot = b"0123456789abcdef"[nibble as usize] as c_char;
    }
    buffer[len] = 0;
    EventText::from_buffer(&buffer)
}

fn path(index: usize) -> EventText {
    indexed_path(index, 24)
}

fn assert_storage_accounting(tracker: &FileTracker) {
    let (FileTracker::Complete(files) | FileTracker::Incomplete(files)) = tracker;
    let paths: usize = files
        .entries
        .iter()
        .map(|entry| entry.path.retained_bytes())
        .sum();
    assert_eq!(files.path_bytes, paths);
    assert_eq!(
        tracker.retained_bytes(),
        files.entries.capacity() * size_of::<OpenFile>() + paths
    );
    assert!(files.entries.len() <= MAX_FILES);
    assert!(tracker.retained_bytes() <= MAX_BYTES);
}

#[test]
fn complete_tracking_matches_out_of_order_paths_and_preserves_opening_order() {
    let mut tracker = FileTracker::new();
    assert!(tracker.is_complete());
    assert!(!tracker.cleanup_pending());
    tracker.open(RecorderEventKind::Started, path(8));
    tracker.open(RecorderEventKind::Rollover, path(2));
    tracker.open(RecorderEventKind::Rollover, path(5));
    assert_eq!(tracker.current(), Some(&path(5)));
    tracker.first_frame(&path(2));
    assert_eq!(tracker.close(&path(5)), FileClose::ConfirmedEmpty);
    // The current file follows opening order, not the path's sort order.
    assert_eq!(tracker.current(), Some(&path(2)));
    assert_eq!(tracker.close(&path(8)), FileClose::ConfirmedEmpty);
    assert_eq!(tracker.close(&path(2)), FileClose::HadMedia);
    assert!(tracker.is_complete());
    assert!(tracker.is_empty());
    assert!(!tracker.cleanup_pending());
    assert_eq!(tracker.current(), None);
    assert_storage_accounting(&tracker);
}

#[test]
fn malformed_or_missing_lifecycle_evidence_keeps_unknown_cleanup_after_known_closes() {
    for defect in 0..10 {
        let mut tracker = FileTracker::new();
        for index in 0..3 {
            tracker.open(
                if index == 0 {
                    RecorderEventKind::Started
                } else {
                    RecorderEventKind::Rollover
                },
                path(index),
            );
        }
        match defect {
            0 => tracker.mark_incomplete(),
            1 => tracker.open(RecorderEventKind::Rollover, path(1)),
            2 => tracker.open(RecorderEventKind::Started, path(4)),
            3 => tracker.open(RecorderEventKind::Rollover, EventText::default()),
            4 => tracker.open(
                RecorderEventKind::Rollover,
                EventText::from_buffer(&[b'u' as c_char; 32]),
            ),
            5 => tracker.open(
                RecorderEventKind::Rollover,
                EventText::from_buffer(&[b't' as c_char, 0]),
            ),
            6 => {
                assert_eq!(tracker.close(&path(99)), FileClose::Unconfirmed);
            }
            7 => {
                assert_eq!(tracker.close(&EventText::default()), FileClose::Unconfirmed);
            }
            8 => tracker.first_frame(&path(99)),
            9 => tracker.first_frame(&EventText::default()),
            _ => unreachable!(),
        }
        assert!(!tracker.is_complete(), "defect {defect}");
        let retained = tracker.files().len();
        let retained_bytes = tracker.retained_bytes();
        for index in 100..356 {
            tracker.open(RecorderEventKind::Rollover, path(index));
            assert_eq!(tracker.files().len(), retained);
            assert_eq!(tracker.retained_bytes(), retained_bytes);
        }
        for index in 0..3 {
            tracker.close(&path(index));
        }
        assert!(tracker.is_empty(), "defect {defect}");
        assert!(!tracker.is_complete(), "defect {defect}");
        assert!(tracker.cleanup_pending(), "defect {defect}");
        tracker.open(RecorderEventKind::Started, path(400));
        assert!(tracker.is_empty(), "storage admission must stay closed");
        assert_storage_accounting(&tracker);
    }
}

#[test]
fn generated_mixed_traces_preserve_known_media_and_irreversible_incompleteness() {
    // Deterministic traces reorder closes and media notifications around loss,
    // while the tiny independent model retains only witnessed open paths.
    for seed in 0..32_u64 {
        let mut tracker = FileTracker::new();
        let mut known = [false; 32];
        let mut media = [false; 32];
        for (index, present) in known.iter_mut().enumerate() {
            tracker.open(
                if index == 0 {
                    RecorderEventKind::Started
                } else {
                    RecorderEventKind::Rollover
                },
                path(index),
            );
            *present = true;
        }
        let mut random = seed + 1;
        let mut incomplete = false;
        for step in 0..512 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let index = (random >> 32) as usize % known.len();
            match step % 7 {
                0 | 1 if known[index] => {
                    tracker.first_frame(&path(index));
                    media[index] = true;
                }
                2 if known[index] => {
                    assert_eq!(
                        tracker.close(&path(index)),
                        if media[index] {
                            FileClose::HadMedia
                        } else if incomplete {
                            FileClose::Unconfirmed
                        } else {
                            FileClose::ConfirmedEmpty
                        }
                    );
                    known[index] = false;
                }
                3 => {
                    tracker.mark_incomplete();
                    incomplete = true;
                }
                4 => {
                    // These opens are hidden from bookkeeping after loss.
                    assert!(incomplete);
                    tracker.open(RecorderEventKind::Rollover, path(1000 + step));
                }
                5 => {
                    assert_eq!(tracker.close(&path(2000 + step)), FileClose::Unconfirmed);
                    incomplete = true;
                }
                _ => {}
            }
            assert_eq!(tracker.is_complete(), !incomplete);
            assert_eq!(
                tracker.files().len(),
                known.iter().filter(|&&present| present).count()
            );
            assert_storage_accounting(&tracker);
        }
        for (index, present) in known.into_iter().enumerate() {
            if present {
                assert_eq!(
                    tracker.close(&path(index)),
                    if media[index] {
                        FileClose::HadMedia
                    } else if incomplete {
                        FileClose::Unconfirmed
                    } else {
                        FileClose::ConfirmedEmpty
                    }
                );
            }
        }
        assert!(tracker.is_empty());
        assert!(tracker.cleanup_pending());
        assert!(!tracker.is_complete());
    }
}

#[test]
fn one_hundred_thousand_unmatched_opens_plateau_at_both_storage_limits() {
    // 4094 bytes is the longest exact path in the SDK's 4096-byte event field:
    // a terminator in the final byte would instead be possibly truncated.
    for len in [24, 4094] {
        let ((mut tracker, plateau), peak) = crate::test_alloc::peak_bytes(|| {
            let mut tracker = FileTracker::new();
            let mut plateau = None;
            for index in 0..100_000 {
                tracker.open(
                    if index == 0 {
                        RecorderEventKind::Started
                    } else {
                        RecorderEventKind::Rollover
                    },
                    indexed_path(index, len),
                );
                assert!(tracker.files().len() <= MAX_FILES);
                assert!(tracker.retained_bytes() <= MAX_BYTES);
                if !tracker.is_complete() {
                    let retained = (tracker.files().len(), tracker.retained_bytes());
                    assert_eq!(*plateau.get_or_insert(retained), retained);
                }
            }
            (
                tracker,
                plateau.expect("the workload must exhaust admission"),
            )
        });
        assert_storage_accounting(&tracker);
        eprintln!(
            "100000 unmatched opens: path_bytes={len}, retained_entries={}, retained_bytes={}, allocator_peak_bytes={peak}",
            plateau.0, plateau.1
        );
        // The event currently being delivered is separately allocated and is
        // released when admission refuses it. Bookkeeping itself stays capped.
        assert!(peak <= MAX_BYTES + len, "path length {len}, peak {peak}");
        if len == 24 {
            assert_eq!(plateau.0, MAX_FILES);
        } else {
            assert!(plateau.0 < MAX_FILES, "long paths must hit the byte limit");
        }
        let extra_events: Vec<_> = (100_000..100_256)
            .map(|index| indexed_path(index, len))
            .collect();
        let (_, allocations) = crate::test_alloc::allocation_count(|| {
            for event in extra_events {
                tracker.open(RecorderEventKind::Rollover, event);
            }
        });
        assert_eq!(allocations, 0, "incomplete bookkeeping must stop growing");
        assert_eq!((tracker.files().len(), tracker.retained_bytes()), plateau);
        while let Some(event) = tracker.files().last().map(|open| open.path.clone()) {
            tracker.close(&event);
        }
        assert!(tracker.is_empty());
        assert!(tracker.cleanup_pending());
        tracker.open(RecorderEventKind::Started, indexed_path(200_000, len));
        assert!(tracker.is_empty());
        assert_storage_accounting(&tracker);
    }
}

#[test]
fn failed_reservation_preserves_known_files_and_permanently_closes_admission() {
    // Reserve failures must be recoverable even before the first file was
    // retained and when a later growth would otherwise move existing entries.
    for initial in [0, 4, 12] {
        let mut tracker = FileTracker::new();
        for index in 0..initial {
            tracker.open(
                if index == 0 {
                    RecorderEventKind::Started
                } else {
                    RecorderEventKind::Rollover
                },
                path(index),
            );
        }
        // Fill the current vector capacity without asking it to grow.
        let (FileTracker::Complete(files) | FileTracker::Incomplete(files)) = &tracker;
        let capacity = files.entries.capacity();
        for index in initial..capacity {
            tracker.open(RecorderEventKind::Rollover, path(index));
        }
        let before = (tracker.files().len(), tracker.retained_bytes());
        let next = path(capacity);
        crate::test_alloc::fail_allocations_at_least(1, || {
            tracker.open(
                if capacity == 0 {
                    RecorderEventKind::Started
                } else {
                    RecorderEventKind::Rollover
                },
                next,
            );
        });
        assert!(!tracker.is_complete());
        assert_eq!((tracker.files().len(), tracker.retained_bytes()), before);
        tracker.open(RecorderEventKind::Rollover, path(capacity + 1));
        assert_eq!(tracker.files().len(), before.0);
        for index in 0..capacity {
            assert_eq!(tracker.close(&path(index)), FileClose::Unconfirmed);
        }
        assert!(tracker.is_empty());
        assert!(tracker.cleanup_pending());
        assert_storage_accounting(&tracker);
    }
}

#[test]
fn an_exhausted_open_order_marks_tracking_incomplete_without_retaining_another_file() {
    let mut tracker = FileTracker::new();
    tracker.open(RecorderEventKind::Started, path(1));
    tracker.data_mut().next_order = u64::MAX;
    tracker.open(RecorderEventKind::Rollover, path(2));
    assert!(!tracker.is_complete());
    assert_eq!(tracker.files().len(), 1);
    assert_eq!(tracker.current(), Some(&path(1)));
    assert!(tracker.cleanup_pending());
    assert_storage_accounting(&tracker);
}
