//! An AV-sync source retains the synchronizer borrow through frame release.
use super::*;

fn detached_sync(sync: AvSync) -> Guard<'static, AvSyncAudioFree> {
    let capture = sync.inner.capture();
    // Even an empty delivered descriptor cannot detach the native free owner.
    unsafe { Guard::new(capture.resource, Default::default()) }
}
