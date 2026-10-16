#![allow(unused)]
use grafton_ndi::{FrameSyncVideoRef, VideoFrameRef, VideoPlane, VideoPlanes, VideoRows};
fn assert_send_sync<T: Send + Sync>() {}
fn views(plane: VideoPlane<'_>, rows: VideoRows<'_>, planes: VideoPlanes<'_>) {
    std::thread::scope(|scope| {
        let shared_plane = &plane;
        let shared_rows = &rows;
        let shared_planes = &planes;
        scope.spawn(move || {
            let _ = shared_plane.row(0);
            let _ = shared_rows.clone().count();
            let _ = shared_planes.clone().count();
        });
    });
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let _ = plane.rows().count();
        });
        scope.spawn(move || {
            let _ = rows.rev().count();
        });
        scope.spawn(move || {
            let _ = planes.flat_map(|p| p.rows()).count();
        });
    });
}
fn receiver(frame: VideoFrameRef<'_>) {
    views(
        frame.plane(0).unwrap(),
        frame.plane(0).unwrap().rows(),
        frame.planes(),
    );
    drop(frame);
}
fn framesync(frame: FrameSyncVideoRef<'_>) {
    views(
        frame.plane(0).unwrap(),
        frame.plane(0).unwrap().rows(),
        frame.planes(),
    );
    drop(frame);
}
fn main() {
    assert_send_sync::<VideoPlane<'static>>();
    assert_send_sync::<VideoRows<'static>>();
    assert_send_sync::<VideoPlanes<'static>>();
}
