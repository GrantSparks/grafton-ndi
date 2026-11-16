//! A routing source through the real runtime.

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name, loopback, endpoint);

use grafton_ndi::{
    routing::{Router, RouterOptions},
    PixelFormat, Receiver, ReceiverOptions, Sender, SenderOptions, Timecode, VideoFrame,
};
use std::time::{Duration, Instant};

#[test]
fn a_router_advertises_itself_and_accepts_route_changes() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let name = common::unique_name("grafton routing validation");
    let mut router = Router::new(&ndi, &RouterOptions::new(name.as_str())).unwrap();
    let advertised = router.source().unwrap();
    assert!(
        advertised.name().ends_with(&format!("({name})")),
        "{advertised}"
    );
    assert_eq!(router.connection_count().unwrap(), 0);
    assert!(router
        .wait_for_connection(Duration::ZERO)
        .unwrap()
        .is_none());

    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("grafton routing target"))
            .clock_video(false)
            .clock_audio(false),
    )
    .unwrap();
    let target = sender.source().unwrap();
    router.set_route(&target).unwrap();
    router.clear_route().unwrap();

    // A router may be used and destroyed on a thread that did not create it.
    std::thread::spawn(move || {
        router.set_route(&target).unwrap();
        assert_eq!(router.connection_count().unwrap(), 0);
    })
    .join()
    .unwrap();
}

/// The SDK may read the route only after a later receiver connects.
#[test]
fn a_receiver_connects_after_the_route_input_is_dropped() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("retained routing target")).clock_video(false),
    )
    .unwrap();
    let mut router = Router::new(
        &ndi,
        &RouterOptions::new(common::unique_name("retained route")),
    )
    .unwrap();
    {
        let target = common::loopback_source(&ndi, &sender).unwrap();
        router.set_route(&target).unwrap();
    }
    let route = common::loopback_endpoint(&ndi, router.source().unwrap().name()).unwrap();
    let receiver = Receiver::new(&ndi, &ReceiverOptions::new(route)).unwrap();
    let mut frame = VideoFrame::builder()
        .resolution(64, 32)
        .pixel_format(PixelFormat::Bgra)
        .timecode(Timecode::from_ticks(72))
        .build()
        .unwrap();
    frame.data_mut().fill(72);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        sender.send_video(&frame).unwrap();
        if let Some(received) = receiver
            .capture_video_ref(Duration::from_millis(100))
            .unwrap()
        {
            assert_eq!((received.width(), received.height()), (64, 32));
            assert_eq!(received.timecode(), Timecode::from_ticks(72));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the receiver never received the retained route"
        );
    }
    router.clear_route().unwrap();
}
