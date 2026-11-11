//! Advertisers and listeners end to end, through the vendor's Discovery Server
//! on a loopback port.
//!
//! The server binary ships in the SDK next to the runtime library. The test
//! runs when the selected runtime supports the Discovery Server objects and
//! that binary is present, and is visibly skipped otherwise; automation that
//! provides both lists `discovery-server` in `GRAFTON_NDI_REQUIRE_RUNTIME`.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

mod common;

use grafton_ndi::{
    capabilities::RuntimeFeature,
    discovery::{
        AdvertisedId, AdvertiserOptions, ListenerOptions, ReceiverCommand, RecvAdvertisement,
        RecvAdvertiser, RecvListener, SendAdvertisement, SendAdvertiser, SendListener,
    },
    Receiver, ReceiverOptions, Sender, SenderOptions, Source, NDI,
};
use std::{
    net::{Ipv4Addr, TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

/// How long the registry may take to reflect a change.
const SETTLE: Duration = Duration::from_secs(20);

/// A Discovery Server on a loopback port, stopped when dropped.
struct Server {
    process: Child,
    endpoint: String,
}

impl Server {
    /// The server binary of the SDK the selected runtime library belongs to:
    /// `<root>/bin/<target>/ndi-discovery-server` beside `<root>/lib/<target>/`.
    fn binary(ndi: &NDI) -> Option<PathBuf> {
        let info = ndi.info();
        let library_directory = info.library_path()?.parent()?;
        let binary = library_directory
            .parent()?
            .parent()?
            .join("bin")
            .join(library_directory.file_name()?)
            .join("ndi-discovery-server");
        binary.is_file().then_some(binary)
    }

    fn start(binary: PathBuf) -> Self {
        // Reserve a free port, then hand it to the server.
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let process = Command::new(binary)
            .args(["-bind", "127.0.0.1", "-port", &port.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let server = Self {
            process,
            endpoint: format!("127.0.0.1:{port}"),
        };
        let deadline = Instant::now() + SETTLE;
        while TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err() {
            assert!(
                Instant::now() < deadline,
                "the Discovery Server did not start listening on {}",
                server.endpoint
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        server
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// Poll `observe` until it yields a value, failing the test after [`SETTLE`].
fn eventually<T>(what: &str, mut observe: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + SETTLE;
    loop {
        if let Some(value) = observe() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
    }
}

#[test]
fn advertised_senders_and_receivers_reach_listeners() {
    let Some(ndi) = common::ndi() else {
        return;
    };
    let unsupported = [
        RuntimeFeature::SenderAdvertising,
        RuntimeFeature::ReceiverAdvertising,
        RuntimeFeature::SenderDiscovery,
        RuntimeFeature::ReceiverDiscovery,
    ]
    .into_iter()
    .find_map(|feature| ndi.require_feature(feature).err());
    let binary = Server::binary(&ndi);
    let (None, Some(binary)) = (&unsupported, binary) else {
        let reason = unsupported.map_or_else(
            || "the SDK's ndi-discovery-server is not beside the selected runtime".to_owned(),
            |error| error.to_string(),
        );
        common::skip(common::Requirement::DiscoveryServer, &reason);
        return;
    };
    let server = Server::start(binary);
    let advertisers = AdvertiserOptions::default().servers(server.endpoint.as_str());
    let listeners = ListenerOptions::default()
        .server(server.endpoint.as_str())
        .unwrap();

    // A sender registered with one advertiser appears to a sender listener.
    let sender = Sender::new(
        &ndi,
        &SenderOptions::builder("grafton discovery loopback sender")
            .clock_video(false)
            .clock_audio(false)
            .build(),
    )
    .unwrap();
    let sender_name = sender.source().unwrap().name;
    let mut send_advertiser = SendAdvertiser::new(&ndi, &advertisers).unwrap();
    let sender_registration = send_advertiser
        .register(&sender, SendAdvertisement::default().allow_monitoring(true))
        .unwrap();
    let mut send_listener = SendListener::new(&ndi, &listeners).unwrap();
    let advertised_sender = eventually("the sender to be advertised", || {
        send_listener
            .wait_for_changes(Duration::from_millis(100))
            .unwrap();
        send_listener
            .snapshot()
            .unwrap()
            .into_iter()
            .find(|advertised| advertised.name() == Some(sender_name.as_str()))
    });
    assert!(send_listener.is_connected());
    let sender_id: AdvertisedId = advertised_sender.id().unwrap().clone();

    // A receiver registered as controllable appears to a receiver listener
    // with the Connect command, and accepts a connection request for it.
    let receiver = Receiver::new(
        &ndi,
        &ReceiverOptions::builder(Source::default())
            .name("grafton discovery loopback receiver")
            .build(),
    )
    .unwrap();
    let mut recv_advertiser = RecvAdvertiser::new(&ndi, &advertisers).unwrap();
    let receiver_registration = recv_advertiser
        .register(
            &receiver,
            RecvAdvertisement::default()
                .allow_monitoring(true)
                .allow_controlling(true),
        )
        .unwrap();
    let mut recv_listener = RecvListener::new(&ndi, &listeners).unwrap();
    let advertised_receiver = eventually("the receiver to be advertised", || {
        recv_listener
            .wait_for_changes(Duration::from_millis(100))
            .unwrap();
        recv_listener
            .snapshot()
            .unwrap()
            .into_iter()
            .find(|advertised| advertised.commands().contains(&ReceiverCommand::Connect))
    });
    let receiver_id = advertised_receiver.id().unwrap().clone();
    recv_listener.subscribe(&receiver_id);
    recv_listener
        .request_connect(&receiver_id, &sender_name)
        .unwrap();
    recv_listener.request_disconnect(&receiver_id).unwrap();

    // Removing a registration removes the peer from the registry.
    assert!(send_advertiser.unregister(sender_registration));
    eventually("the sender to leave the registry", || {
        send_listener
            .wait_for_changes(Duration::from_millis(100))
            .unwrap();
        send_listener
            .snapshot()
            .unwrap()
            .iter()
            .all(|advertised| advertised.id() != Some(&sender_id))
            .then_some(())
    });
    assert!(recv_advertiser.unregister(receiver_registration));
    eventually("the receiver to leave the registry", || {
        recv_listener
            .wait_for_changes(Duration::from_millis(100))
            .unwrap();
        recv_listener
            .snapshot()
            .unwrap()
            .iter()
            .all(|advertised| advertised.id() != Some(&receiver_id))
            .then_some(())
    });
}
