//! Advertisers and listeners end to end, through the vendor's Discovery Server
//! on a loopback port.
//!
//! The server binary ships in the SDK next to the runtime library. The tests
//! run when the selected runtime supports the Discovery Server objects and
//! that binary is present, and are visibly skipped otherwise; automation that
//! provides both lists `discovery-server` in `GRAFTON_NDI_REQUIRE_RUNTIME`.
#![cfg(all(target_os = "linux", not(ndi_link_static)))]

mod common;

use grafton_ndi::{
    capabilities::RuntimeFeature,
    discovery::{
        AdvertisedReceiver, AdvertiserOptions, ListenerOptions, ReceiverCommand, RecvAdvertisement,
        RecvAdvertiser, RecvListener, SendAdvertisement, SendAdvertiser, SendListener,
    },
    Error, Receiver, ReceiverOptions, Sender, SenderOptions, Source, NDI,
};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Ipv4Addr, TcpListener, TcpStream},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// How long the registry, or a starting server, may take to reflect a change.
const SETTLE: Duration = Duration::from_secs(20);

/// How many ports are tried before a server is declared unable to start.
const START_ATTEMPTS: usize = 5;

/// A runtime whose advertisers and listeners are usable, or `None` after
/// reporting the skip.
fn discovery_runtime() -> Option<NDI> {
    let ndi = common::ndi()?;
    let unsupported = [
        RuntimeFeature::SenderAdvertising,
        RuntimeFeature::ReceiverAdvertising,
        RuntimeFeature::SenderDiscovery,
        RuntimeFeature::ReceiverDiscovery,
    ]
    .into_iter()
    .find_map(|feature| ndi.require_feature(feature).err());
    match unsupported {
        None => Some(ndi),
        Some(error) => {
            common::skip(common::Requirement::DiscoveryServer, &error.to_string());
            None
        }
    }
}

/// That runtime and the server binary of the SDK it belongs to
/// (`<root>/bin/<target>/ndi-discovery-server` beside `<root>/lib/<target>/`),
/// or `None` after reporting the skip.
fn discovery_server() -> Option<(NDI, PathBuf)> {
    let ndi = discovery_runtime()?;
    let binary = ndi.info().library_path().and_then(|library| {
        let directory = library.parent()?;
        let binary = directory
            .parent()?
            .parent()?
            .join("bin")
            .join(directory.file_name()?)
            .join("ndi-discovery-server");
        binary.is_file().then_some(binary)
    });
    match binary {
        Some(binary) => Some((ndi, binary)),
        None => {
            common::skip(
                common::Requirement::DiscoveryServer,
                "the SDK's ndi-discovery-server is not beside the selected runtime",
            );
            None
        }
    }
}

/// A loopback port nothing is listening on at the moment.
fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Everything a server has written, collected as it arrives so that its pipes
/// never fill and a failure can show what the server said.
#[derive(Default)]
struct Output {
    text: Arc<Mutex<String>>,
    readers: Vec<std::thread::JoinHandle<()>>,
}

impl Output {
    fn collect(&mut self, stream: impl Read + Send + 'static) {
        let text = Arc::clone(&self.text);
        self.readers.push(std::thread::spawn(move || {
            for line in BufReader::new(stream).split(b'\n').map_while(Result::ok) {
                let mut text = text.lock().unwrap();
                text.push_str(&String::from_utf8_lossy(&line));
                text.push('\n');
            }
        }));
    }

    /// What the server has written so far.
    fn text(&self) -> String {
        self.text.lock().unwrap().clone()
    }

    /// Everything a server that has exited wrote.
    fn finish(&mut self) -> String {
        for reader in self.readers.drain(..) {
            reader.join().unwrap();
        }
        self.text()
    }
}

/// A Discovery Server on a loopback port. It stops when dropped, and when the
/// thread that started it ends for any other reason, such as the test process
/// being killed.
struct Server {
    process: Child,
    port: u16,
    output: Output,
}

impl Server {
    /// Start a server on a free port. Another process can take a port between
    /// its selection and the server binding it, so a server that exits is
    /// tried again elsewhere.
    fn start(binary: &Path) -> Self {
        Self::start_on_one_of(
            binary,
            std::iter::repeat_with(free_port).take(START_ATTEMPTS),
        )
    }

    /// Start a server on the first of `ports` it can bind.
    fn start_on_one_of(binary: &Path, ports: impl IntoIterator<Item = u16>) -> Self {
        let mut failures = Vec::new();
        for port in ports {
            match Self::start_on(binary, port) {
                Ok(server) => return server,
                Err(failure) => failures.push(failure),
            }
        }
        panic!(
            "the Discovery Server did not start:\n{}",
            failures.join("\n")
        );
    }

    /// Start a server on `port` and wait until it serves there.
    ///
    /// # Errors
    ///
    /// Describes, with the server's own output, a server that exited instead.
    ///
    /// # Panics
    ///
    /// Panics, with the server's output, when a running server does not start
    /// serving in time.
    fn start_on(binary: &Path, port: u16) -> Result<Self, String> {
        let test_process = std::process::id();
        let mut command = Command::new(binary);
        command
            .args(["-bind", "127.0.0.1", "-port", &port.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // SAFETY: the closure runs in the forked child before it executes the
        // server, and calls only async-signal-safe functions without
        // allocating.
        unsafe {
            command.pre_exec(move || {
                // The kernel kills the server when the thread that started it
                // ends, so a test process that dies cannot leave it behind.
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // That thread may have ended before the request took effect.
                if libc::getppid() as u32 != test_process {
                    return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
                }
                Ok(())
            });
        }
        let mut process = command.spawn().unwrap();
        let mut output = Output::default();
        output.collect(process.stdout.take().unwrap());
        output.collect(process.stderr.take().unwrap());
        let mut server = Self {
            process,
            port,
            output,
        };

        // The server says so once it has bound the port. Only then can a
        // connection to the port be known to reach it rather than some other
        // process that took the port first.
        let serving = format!("Running server on port: {port}");
        let deadline = Instant::now() + SETTLE;
        loop {
            if let Some(status) = server.process.try_wait().unwrap() {
                return Err(format!(
                    "on port {port} it exited with {status}:\n{}",
                    server.output.finish()
                ));
            }
            if server.output.text().contains(&serving)
                && TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok()
            {
                return Ok(server);
            }
            assert!(
                Instant::now() < deadline,
                "the Discovery Server did not serve port {port} within {SETTLE:?}:\n{}",
                server.output.text()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn endpoint(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    fn advertisers(&self) -> AdvertiserOptions {
        AdvertiserOptions::default().servers(self.endpoint())
    }

    fn listeners(&self) -> ListenerOptions {
        ListenerOptions::default().server(self.endpoint()).unwrap()
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

/// A sender named `label`, made unique to this process: the runtime takes a
/// lock named after each sender and each named receiver.
fn sender(ndi: &NDI, label: &str) -> Sender {
    let options = SenderOptions::builder(common::unique_name(label))
        .clock_video(false)
        .clock_audio(false);
    Sender::new(ndi, &options.build()).unwrap()
}

/// A receiver named `label`, made unique to this process like a [`sender`].
fn receiver(ndi: &NDI, label: &str) -> Receiver {
    let options = ReceiverOptions::builder(Source::default()).name(common::unique_name(label));
    Receiver::new(ndi, &options.build()).unwrap()
}

/// The advertised receiver that [`receiver`] created for `label`: the registry
/// prefixes the machine name.
fn named(receivers: &[AdvertisedReceiver], label: &str) -> Option<AdvertisedReceiver> {
    let suffix = format!("({})", common::unique_name(label));
    receivers
        .iter()
        .find(|receiver| receiver.name().is_some_and(|name| name.ends_with(&suffix)))
        .cloned()
}

#[test]
fn advertised_senders_and_receivers_reach_listeners() {
    let Some((ndi, binary)) = discovery_server() else {
        return;
    };
    let server = Server::start(&binary);

    // A sender registered with one advertiser appears to a sender listener.
    let sender = sender(&ndi, "grafton discovery loopback sender");
    let sender_name = sender.source().unwrap().name;
    let mut send_advertiser = SendAdvertiser::new(&ndi, &server.advertisers()).unwrap();
    let sender_registration = send_advertiser
        .register(&sender, SendAdvertisement::default().allow_monitoring(true))
        .unwrap();
    let mut send_listener = SendListener::new(&ndi, &server.listeners()).unwrap();
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
    let sender_id = advertised_sender.id().unwrap().clone();
    send_listener.subscribe(&sender_id);

    // Receivers appear to a receiver listener as they were advertised: one
    // that may be controlled, one that may only be monitored, and one more in
    // the second one's input group.
    const GROUP: &str = "grafton discovery loopback group";
    let controllable_receiver = receiver(&ndi, "grafton discovery loopback controllable");
    let monitored_receiver = receiver(&ndi, "grafton discovery loopback monitored");
    let grouped_receiver = receiver(&ndi, "grafton discovery loopback grouped");
    let mut recv_advertiser = RecvAdvertiser::new(&ndi, &server.advertisers()).unwrap();
    let controllable_registration = recv_advertiser
        .register(
            &controllable_receiver,
            RecvAdvertisement::default()
                .allow_monitoring(true)
                .allow_controlling(true),
        )
        .unwrap();
    recv_advertiser
        .register(
            &monitored_receiver,
            RecvAdvertisement::default()
                .allow_monitoring(true)
                .input_group(GROUP),
        )
        .unwrap();
    recv_advertiser
        .register(
            &grouped_receiver,
            RecvAdvertisement::default()
                .allow_controlling(true)
                .input_group(GROUP),
        )
        .unwrap();
    let mut recv_listener = RecvListener::new(&ndi, &server.listeners()).unwrap();
    let [controllable, monitored, grouped] = eventually("the receivers to be advertised", || {
        recv_listener
            .wait_for_changes(Duration::from_millis(100))
            .unwrap();
        let receivers = recv_listener.snapshot().unwrap();
        Some([
            named(&receivers, "grafton discovery loopback controllable")?,
            named(&receivers, "grafton discovery loopback monitored")?,
            named(&receivers, "grafton discovery loopback grouped")?,
        ])
    });
    assert!(recv_listener.is_connected());

    // Control was granted to the receivers it was set for, and to no other.
    assert!(controllable.commands().contains(&ReceiverCommand::Connect));
    assert!(grouped.commands().contains(&ReceiverCommand::Connect));
    assert!(!monitored.commands().contains(&ReceiverCommand::Connect));

    // The input-group name arrives as given, and names one group.
    assert_eq!(monitored.input_name(), Some(GROUP));
    assert_eq!(grouped.input_name(), Some(GROUP));
    assert_eq!(controllable.input_name(), Some(""));
    assert_eq!(monitored.input_id().unwrap(), grouped.input_id().unwrap());
    assert_ne!(
        monitored.input_id().unwrap(),
        controllable.input_id().unwrap()
    );

    // A receiver that accepts connection requests can be sent one. One that
    // does not is refused before anything is sent.
    let controllable_id = controllable.id().unwrap().clone();
    recv_listener.subscribe(&controllable_id);
    recv_listener
        .request_connect(&controllable_id, &sender_name)
        .unwrap();
    recv_listener.request_disconnect(&controllable_id).unwrap();
    assert!(matches!(
        recv_listener.request_connect(monitored.id().unwrap(), &sender_name),
        Err(Error::UnsupportedCapability { .. })
    ));

    // Removing a registration removes the peer from the registry.
    assert!(send_advertiser.unregister(sender_registration).unwrap());
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
    assert!(recv_advertiser
        .unregister(controllable_registration)
        .unwrap());
    eventually("the receiver to leave the registry", || {
        recv_listener
            .wait_for_changes(Duration::from_millis(100))
            .unwrap();
        recv_listener
            .snapshot()
            .unwrap()
            .iter()
            .all(|advertised| advertised.id() != Some(&controllable_id))
            .then_some(())
    });
}

#[test]
fn creation_does_not_wait_for_the_server() {
    let Some(ndi) = discovery_runtime() else {
        return;
    };
    // Nothing listens on this port, and no server started by another test can
    // be given it. Every object is created all the same, accepts work, and
    // reports that it is not connected.
    let endpoint = "127.0.0.1:1";
    let advertisers = AdvertiserOptions::default().servers(endpoint);
    let listeners = ListenerOptions::default().server(endpoint).unwrap();
    let sender = sender(&ndi, "grafton discovery unreachable sender");
    let receiver = receiver(&ndi, "grafton discovery unreachable receiver");
    let mut send_advertiser = SendAdvertiser::new(&ndi, &advertisers).unwrap();
    let mut recv_advertiser = RecvAdvertiser::new(&ndi, &advertisers).unwrap();
    send_advertiser
        .register(&sender, SendAdvertisement::default())
        .unwrap();
    recv_advertiser
        .register(&receiver, RecvAdvertisement::default())
        .unwrap();
    let mut send_listener = SendListener::new(&ndi, &listeners).unwrap();
    let mut recv_listener = RecvListener::new(&ndi, &listeners).unwrap();
    assert!(!send_listener.is_connected() && !recv_listener.is_connected());
    assert_eq!(send_listener.server_url().as_deref(), Some(endpoint));
    assert!(send_listener.snapshot().unwrap().is_empty());
    assert!(recv_listener.snapshot().unwrap().is_empty());

    // An address the SDK cannot parse is refused at creation, and the error
    // names the address instead of blaming the network.
    for address in ["", "not a host:xyz", "127.0.0.1:99999"] {
        let errors = [
            SendAdvertiser::new(&ndi, &AdvertiserOptions::default().servers(address)).err(),
            SendListener::new(&ndi, &ListenerOptions::default().server(address).unwrap()).err(),
        ];
        for error in errors {
            let Some(Error::InitializationFailed(reason)) = error else {
                panic!("{address:?}: expected a creation failure, got {error:?}");
            };
            assert!(reason.contains(&format!("{address:?}")), "{reason}");
        }
    }
}

#[test]
fn objects_connect_when_their_server_appears() {
    let Some((ndi, binary)) = discovery_server() else {
        return;
    };
    // The sender is registered, and the listener created, before any server
    // exists at the address.
    let port = free_port();
    let endpoint = format!("127.0.0.1:{port}");
    let sender = sender(&ndi, "grafton discovery late server sender");
    let sender_name = sender.source().unwrap().name;
    let mut advertiser = SendAdvertiser::new(
        &ndi,
        &AdvertiserOptions::default().servers(endpoint.as_str()),
    )
    .unwrap();
    advertiser
        .register(&sender, SendAdvertisement::default())
        .unwrap();
    let mut listener = SendListener::new(
        &ndi,
        &ListenerOptions::default()
            .server(endpoint.as_str())
            .unwrap(),
    )
    .unwrap();
    assert!(!listener.is_connected());

    // Once a server appears there, both connect on their own and the
    // registration made earlier arrives.
    let _server = Server::start_on_one_of(&binary, [port]);
    eventually("the earlier registration to arrive", || {
        listener
            .wait_for_changes(Duration::from_millis(100))
            .unwrap();
        listener
            .snapshot()
            .unwrap()
            .iter()
            .any(|advertised| advertised.name() == Some(sender_name.as_str()))
            .then_some(())
    });
    assert!(listener.is_connected());
}

#[test]
fn a_server_that_cannot_bind_its_port_is_reported_and_started_elsewhere() {
    let Some((_ndi, binary)) = discovery_server() else {
        return;
    };
    // The port is taken, as it would be if another process won the race for
    // a port that was free a moment ago.
    let taken = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let taken_port = taken.local_addr().unwrap().port();

    // The failure is the server's exit, with what it wrote, not a timeout.
    let started = Instant::now();
    let failure = Server::start_on(&binary, taken_port)
        .err()
        .expect("no server can bind a port that is taken");
    assert!(started.elapsed() < SETTLE / 2, "{failure}");
    assert!(failure.contains("exited with"), "{failure}");
    assert!(failure.contains("NDI Discovery Service"), "{failure}");

    // A start that is given another port to fall back on succeeds there.
    let free = free_port();
    let server = Server::start_on_one_of(&binary, [taken_port, free]);
    assert_eq!(server.port, free);
}

/// Whether a process still runs: a killed process that nothing has reaped yet
/// no longer does.
fn is_running(process: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{process}/stat")).is_ok_and(|stat| {
        // The state follows the parenthesised command name.
        let state = stat.rsplit(')').next().unwrap_or_default().trim_start();
        !state.starts_with('Z') && !state.starts_with('X')
    })
}

#[test]
fn the_server_does_not_outlive_a_killed_test_process() {
    const HOLDER: &str = "GRAFTON_DISCOVERY_SERVER_HOLDER";
    if let Some(binary) = std::env::var_os(HOLDER) {
        // The holder: start a server, say which process it is, and wait to be
        // killed without the chance to stop it.
        let server = Server::start(Path::new(&binary));
        println!("server {}", server.process.id());
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }
    let Some((_ndi, binary)) = discovery_server() else {
        return;
    };
    let mut holder = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "the_server_does_not_outlive_a_killed_test_process",
            "--nocapture",
        ])
        .env(HOLDER, &binary)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let server = BufReader::new(holder.stdout.take().unwrap())
        .lines()
        .map_while(Result::ok)
        .find_map(|line| line.strip_prefix("server ")?.parse::<u32>().ok())
        .expect("the holder reports its server");
    assert!(is_running(server));

    holder.kill().unwrap();
    holder.wait().unwrap();
    // The server must go with the holder, not at some later moment of its
    // own: left alone it would only stop once it next wrote to the pipes the
    // holder held, which it does every few seconds.
    let deadline = Instant::now() + Duration::from_secs(2);
    while is_running(server) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    if is_running(server) {
        // SAFETY: the signal goes to the server the holder reported, which
        // nothing else would stop.
        unsafe { libc::kill(server as libc::pid_t, libc::SIGKILL) };
        panic!("the server outlived the process that started it");
    }
}
