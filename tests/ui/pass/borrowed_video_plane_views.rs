use grafton_ndi::{BorrowedVideoFrame, PixelFormat, VideoPlane, VideoPlanes};

// A padded borrowed frame lends its active rows through plane views that
// borrow the frame, without copying; scoped workers can read them.
fn main() {
    // Two BGRA rows of two pixels, each padded from 8 to 12 bytes.
    let pixels = [7; 24];
    let frame = BorrowedVideoFrame::builder(&pixels, 2, 2, PixelFormat::Bgra)
        .line_stride_bytes(12)
        .build()
        .unwrap();
    let plane: VideoPlane<'_> = frame.plane(0).unwrap();
    let planes: VideoPlanes<'_> = frame.planes();
    assert_eq!(planes.len(), 1);
    assert!(frame.plane(1).is_none());
    assert!(plane.data().is_none());
    assert!(plane.rows().all(|row| row == [7; 8]));
    std::thread::scope(|scope| {
        scope.spawn(move || plane.rows().count());
        scope.spawn(move || planes.flat_map(|plane| plane.rows()).count());
    });
    let owned = frame.to_owned();
    assert!(frame
        .planes()
        .zip(owned.planes())
        .all(|(borrowed, owned)| borrowed.rows().eq(owned.rows())));
}
