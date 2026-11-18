//! Compiled inside finder.rs so privacy cannot explain a rejected guard.

#![allow(dead_code)]

use super::*;

// Positive control: the actual acquisition path and snapshot kernel agree.
fn a_finder_issued_guard_is_accepted(finder: &Finder) -> Result<Vec<Source>> {
    finder
        .lock_snapshots(crate::timeout::NeverStop)
        .unwrap()
        .snapshot(&SelectingSnapshot {
            core: &finder.sdk,
            select: |_: &BorrowedSource<'_>| Ok(true),
        })
}

// The old MutexGuard<()> protocol accepted this unrelated lock. The actual
// SnapshotGuard now requires the native instance protected by its own mutex.
#[cfg(ndi_test_foreign_snapshot_guard)]
fn a_foreign_mutex_guard_is_rejected(
    foreign: &parking_lot::Mutex<()>,
    snapshot: &sys::Core,
) -> Result<Vec<Source>> {
    let instance = foreign.lock();
    SnapshotGuard { instance }.snapshot(&SelectingSnapshot {
        core: snapshot,
        select: |_: &BorrowedSource<'_>| Ok(true),
    })
}
