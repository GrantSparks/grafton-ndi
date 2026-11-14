//! A routing source through the real runtime.

#[macro_use]
#[path = "common/mod.rs"]
mod helpers;
common_helpers!(runtime, name);

use grafton_ndi::{
    routing::{Router, RouterOptions},
    Sender, SenderOptions,
};
use std::time::Duration;

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
    assert_eq!(router.connection_count(Duration::ZERO).unwrap(), 0);

    let sender = Sender::new(
        &ndi,
        &SenderOptions::new(common::unique_name("grafton routing target"))
            .clock_video(false)
            .clock_audio(false),
    )
    .unwrap();
    let target = sender.source().unwrap();
    router.change(&target).unwrap();
    router.clear().unwrap();

    // A router may be used and destroyed on a thread that did not create it.
    std::thread::spawn(move || {
        router.change(&target).unwrap();
        assert_eq!(router.connection_count(Duration::ZERO).unwrap(), 0);
    })
    .join()
    .unwrap();
}
