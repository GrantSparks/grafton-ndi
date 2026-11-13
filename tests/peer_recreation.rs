//! Receivers connect to senders and receivers that are created and dropped in
//! quick succession, over loopback, as the runtime tests create them.
//!
//! On the hosted Windows runners some receivers never connected to a sender of
//! their own process (run 43342188690). Given `127.0.0.1:5961`, such a
//! receiver reported no connection for the whole 15 s a test waits, and its
//! sender none either, while a receiver opened 3 ms later to the same address
//! connected in 1 ms. Most followed, by about 65 ms, the teardown of the
//! previous test's sender on that port and of its receivers.
//!
//! This test recreates peers in the patterns the suites use, sends video and
//! asserts that every receiver connects and is given a frame. Its arms tell the
//! explanations apart (`ci/README.md`, "Receivers that never connect on
//! Windows", has the table):
//!
//! * `sender`: a receiver and its sender are dropped together, and the next
//!   sender is created after a gap of 0, 50, 250 or 1000 ms;
//! * `settled`: the receiver is dropped 500 ms before its sender, so that the
//!   next sender follows the teardown of a sender but not of a receiver;
//! * `receiver`: one sender stays up, with its first receiver connected
//!   throughout, and further receivers, at each bandwidth that carries video,
//!   are each created as soon as the one before is dropped.
//!
//! Every receiver is given the endpoint [`common::loopback_source`] returns,
//! as in the other suites. On Windows that names a loopback address no
//! receiver before it was given ([`common::loopback_host`]), and every arm runs
//! a second time as a control, with each receiver given the SDK's
//! `127.0.0.1:<port>`. The control's outcomes are reported and not asserted:
//! they measure, in the same run, what the distinct addresses change.
//!
//! Before the arms, with no receiver, a probe drops a sender, binds its port at
//! once with std's listener (which on Windows sets neither `SO_REUSEADDR` nor
//! `SO_EXCLUSIVEADDRUSE`), and creates another sender while the port is held.
//! A port that cannot be bound shows a socket outliving its sender. A second
//! sender given the held port shows that the SDK's listener can share a port
//! with another socket, which a lingering listener of an earlier sender would
//! need in order to take a receiver's connection.
//!
//! A receiver that has not connected within 2 s, where they connect within
//! milliseconds, is diagnosed: the Windows TCP table for the port
//! ([`common::socket_table`]) is taken, and a second receiver is opened to the
//! same address. The receiver is then waited for until the suites' 15 s have
//! passed, except a control receiver after the first, which shows the same
//! thing. The report has one line per receiver and is printed whether the test
//! passes or not.
mod common;

use grafton_ndi::{
    capabilities::RuntimeFeature, ConnectionStats, Ndi, Receiver, ReceiverBandwidth,
    ReceiverOptions, Sender, SenderOptions, Source, SourceAddress, VideoFrame,
};
use std::{
    fmt::{self, Write as _},
    io::Write as _,
    net::{Ipv4Addr, TcpListener},
    time::{Duration, Instant},
};

/// How long the suites wait for a receiver to deliver.
const PATIENCE: Duration = Duration::from_secs(15);

/// How long a receiver may take before it is diagnosed. Receivers that
/// connect do so within milliseconds.
const GRACE: Duration = Duration::from_secs(2);

/// One capture while waiting.
const SLICE: Duration = Duration::from_millis(10);

/// The gaps between dropping a cycle's peers and creating the next sender,
/// and the cycles run with each. In run 43342188690, 12 of 43 receivers opened
/// within 150 ms of another receiver's teardown never connected; at that rate
/// ten cycles with no gap would all connect with a chance of about 4%.
const GAPS: [(Duration, usize); 4] = [
    (Duration::ZERO, 10),
    (Duration::from_millis(50), 6),
    (Duration::from_millis(250), 6),
    (Duration::from_millis(1000), 4),
];

/// How long before its sender the `settled` arm drops a receiver: longer than
/// the 300 ms after a receiver's teardown from which every receiver of run
/// 43342188690 connected, except those that followed a teardown its SDK log
/// does not show.
const SETTLED: Duration = Duration::from_millis(500);

/// The cycles of the `settled` arm.
const SETTLED_CYCLES: usize = 8;

/// The rounds of the `receiver` arm, each with one receiver at every
/// bandwidth that carries video.
const RECEIVER_ROUNDS: usize = 3;

/// How a cycle's receivers are given their endpoint.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Route {
    /// From [`common::loopback_source`], as the other suites; asserted.
    Helper,
    /// As the SDK advertises it, at 127.0.0.1; reported only.
    Control,
}

impl Route {
    /// After this many receivers of the route that did not connect, its
    /// remaining cycles are skipped: each costs seconds, and the ones before
    /// show the failure.
    fn stuck_limit(self) -> usize {
        match self {
            Self::Helper => 3,
            Self::Control => 8,
        }
    }
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Helper => "helper",
            Self::Control => "control",
        })
    }
}

/// What one receiver did.
struct Cycle {
    arm: String,
    route: Route,
    /// The port of the sender, and of the sender created before it.
    port: u16,
    previous_port: Option<u16>,
    address: String,
    bandwidth: ReceiverBandwidth,
    /// Since the receiver was created: when it first reported a connection,
    /// and when it was given a frame.
    counted: Option<Duration>,
    framed: Option<Duration>,
    /// How long it was waited for.
    waited: Duration,
    stats: ConnectionStats,
    sender_connections: String,
    /// For a receiver that did not connect within [`GRACE`].
    diagnosis: Option<String>,
}

impl Cycle {
    fn connected(&self) -> bool {
        self.framed.is_some()
    }
}

/// When something happened, or that it never did.
fn at(time: Option<Duration>) -> String {
    time.map_or_else(|| "never".to_owned(), |time| format!("{time:.1?}"))
}

impl fmt::Display for Cycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let previous = self
            .previous_port
            .map_or_else(|| "-".to_owned(), |port| port.to_string());
        let bandwidth = format!("{:?}", self.bandwidth);
        write!(
            f,
            "{:<7} {:<18} port {} (before {previous:<4}) {:<16} {bandwidth:<16} counted {}, \
             frame {}, waited {:.1?}; receiver connections {}, video {}; {}",
            self.route.to_string(),
            self.arm,
            self.port,
            self.address,
            at(self.counted),
            at(self.framed),
            self.waited,
            self.stats.connections(),
            self.stats.video_frames_received(),
            self.sender_connections,
        )
    }
}

/// The receivers recorded so far, and what the arms need between cycles.
struct Run<'a> {
    ndi: &'a Ndi,
    video: VideoFrame,
    cycles: Vec<Cycle>,
    /// Cycles skipped, for each route.
    skipped: [usize; 2],
    /// Whether a control receiver that missed [`GRACE`] has been waited for
    /// until [`PATIENCE`].
    control_waited: bool,
    previous_port: Option<u16>,
    senders: usize,
}

impl<'a> Run<'a> {
    fn new(ndi: &'a Ndi) -> Self {
        Self {
            ndi,
            video: VideoFrame::builder().resolution(64, 32).build().unwrap(),
            cycles: Vec::new(),
            skipped: [0; 2],
            control_waited: false,
            previous_port: None,
            senders: 0,
        }
    }

    /// Whether `route` has had its share of receivers that did not connect;
    /// if so, the cycle that asks is counted as skipped.
    fn exhausted(&mut self, route: Route) -> bool {
        let stuck = self
            .cycles
            .iter()
            .filter(|cycle| cycle.route == route && !cycle.connected())
            .count();
        let exhausted = stuck >= route.stuck_limit();
        if exhausted {
            self.skipped[route as usize] += 1;
        }
        exhausted
    }

    /// A new unclocked sender.
    fn sender(&mut self) -> Sender {
        self.senders += 1;
        let name = common::unique_name(&format!("recreated {}", self.senders));
        Sender::new(
            self.ndi,
            &SenderOptions::new(name)
                .clock_video(false)
                .clock_audio(false),
        )
        .unwrap()
    }

    /// The endpoint of `sender` that `route` gives a receiver, and its port.
    /// Either way the sender is looked up once, with one finder, as the
    /// suites look up theirs.
    fn endpoint(&self, sender: &Sender, route: Route) -> (Source, u16) {
        let name = sender.source().unwrap().name().to_owned();
        match route {
            Route::Helper => {
                let source = common::loopback_endpoint(self.ndi, &name).unwrap();
                let port = common::port_of(&source).expect("the helper names an IP endpoint");
                (source, port)
            }
            Route::Control => {
                let (source, port) = common::advertised_endpoint(self.ndi, &name).unwrap();
                let loopback = SourceAddress::Ip(format!("127.0.0.1:{port}"));
                (source.with_address(loopback), port)
            }
        }
    }

    /// Open a receiver of `sender` at `source`, wait for it, and record what
    /// it did. The receiver is returned, for the caller to drop when the
    /// pattern it follows does.
    fn connect(
        &mut self,
        arm: &str,
        route: Route,
        sender: &Sender,
        (source, port): (Source, u16),
        bandwidth: ReceiverBandwidth,
    ) -> Receiver {
        let address = match source.address() {
            Some(SourceAddress::Ip(address)) => address.clone(),
            other => format!("{other:?}"),
        };
        let options = ReceiverOptions::new(source).bandwidth(bandwidth);
        let receiver = Receiver::new(self.ndi, &options).unwrap();
        let created = Instant::now();
        let mut counted = None;
        let mut framed = self.await_frame(sender, &receiver, created, GRACE, &mut counted);
        let mut diagnosis = None;
        if framed.is_none() {
            let mut text = common::socket_table(port)
                .unwrap_or_else(|| "  (the TCP table is collected on Windows only)\n".to_owned());
            // Whether the sender takes another receiver at the same address
            // while this one has not connected.
            let second = Receiver::new(self.ndi, &options).unwrap();
            let opened = Instant::now();
            let mut second_counted = None;
            let second_framed =
                self.await_frame(sender, &second, opened, GRACE, &mut second_counted);
            let _ = writeln!(
                text,
                "  a second receiver to {address}: counted {}, frame {}; it reports {:?}",
                at(second_counted),
                at(second_framed),
                second.connection_stats()
            );
            drop(second);
            if route == Route::Helper || !self.control_waited {
                self.control_waited |= route == Route::Control;
                framed = self.await_frame(sender, &receiver, created, PATIENCE, &mut counted);
            }
            diagnosis = Some(text);
        }
        let sender_connections = match sender.connection_count(Duration::ZERO) {
            Ok(count) => format!("sender connections {count}"),
            Err(error) => format!("sender connection count failed: {error}"),
        };
        self.cycles.push(Cycle {
            arm: arm.to_owned(),
            route,
            port,
            previous_port: self.previous_port,
            address,
            bandwidth,
            counted,
            framed,
            waited: created.elapsed(),
            stats: receiver.connection_stats(),
            sender_connections,
            diagnosis,
        });
        receiver
    }

    /// Send video until `receiver` is given a frame or `limit` has passed
    /// since `since`, noting when it first reports a connection.
    fn await_frame(
        &self,
        sender: &Sender,
        receiver: &Receiver,
        since: Instant,
        limit: Duration,
        counted: &mut Option<Duration>,
    ) -> Option<Duration> {
        while since.elapsed() < limit {
            sender.send_video(&self.video).unwrap();
            if counted.is_none() && receiver.connection_stats().connections() > 0 {
                *counted = Some(since.elapsed());
            }
            if receiver.video().capture(SLICE).unwrap().is_some() {
                return Some(since.elapsed());
            }
        }
        None
    }

    /// A receiver and its sender are dropped together, and the next sender is
    /// created after each gap.
    fn recreated_senders(&mut self, route: Route) {
        for (gap, cycles) in GAPS {
            let arm = format!("sender, gap {gap:?}");
            for _ in 0..cycles {
                if self.exhausted(route) {
                    continue;
                }
                std::thread::sleep(gap);
                let sender = self.sender();
                let endpoint = self.endpoint(&sender, route);
                let port = endpoint.1;
                let receiver =
                    self.connect(&arm, route, &sender, endpoint, ReceiverBandwidth::Highest);
                drop(receiver);
                drop(sender);
                self.previous_port = Some(port);
            }
        }
    }

    /// The receiver is dropped [`SETTLED`] before its sender, and the next
    /// sender is created at once.
    fn settled_receivers(&mut self, route: Route) {
        for _ in 0..SETTLED_CYCLES {
            if self.exhausted(route) {
                continue;
            }
            let sender = self.sender();
            let endpoint = self.endpoint(&sender, route);
            let port = endpoint.1;
            let receiver = self.connect(
                "settled",
                route,
                &sender,
                endpoint,
                ReceiverBandwidth::Highest,
            );
            drop(receiver);
            std::thread::sleep(SETTLED);
            drop(sender);
            self.previous_port = Some(port);
        }
    }

    /// One sender, with its first receiver connected throughout; every
    /// further receiver is created as soon as the one before is dropped.
    fn recreated_receivers(&mut self, route: Route, bandwidths: &[ReceiverBandwidth]) {
        if self.exhausted(route) {
            self.skipped[route as usize] += RECEIVER_ROUNDS * bandwidths.len();
            return;
        }
        let sender = self.sender();
        let endpoint = self.endpoint(&sender, route);
        let port = endpoint.1;
        let first = self.connect(
            "receiver, first",
            route,
            &sender,
            endpoint,
            ReceiverBandwidth::Highest,
        );
        self.previous_port = Some(port);
        for _ in 0..RECEIVER_ROUNDS {
            for &bandwidth in bandwidths {
                if self.exhausted(route) {
                    continue;
                }
                let endpoint = self.endpoint(&sender, route);
                drop(self.connect("receiver", route, &sender, endpoint, bandwidth));
            }
        }
        drop(first);
        drop(sender);
    }
}

/// Drop a sender, bind its port at once, and create a sender while the port is
/// held; see the module documentation.
fn port_sharing(ndi: &Ndi) -> String {
    let mut report = String::new();
    for attempt in 1..=3 {
        let name = common::unique_name(&format!("port probe {attempt}"));
        let first = Sender::new(ndi, &SenderOptions::new(name)).unwrap();
        let (_, port) = common::advertised_endpoint(ndi, first.source().unwrap().name()).unwrap();
        drop(first);
        let dropped = Instant::now();
        let mut refusals = Vec::new();
        let (holder, bound) = loop {
            match TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)) {
                Ok(holder) if refusals.is_empty() => break (Some(holder), "bound at once".into()),
                Ok(holder) => {
                    let after = dropped.elapsed();
                    break (
                        Some(holder),
                        format!("bound after {after:.1?}, refused before with {refusals:?}"),
                    );
                }
                Err(error) => {
                    if refusals.last() != Some(&error.kind()) {
                        refusals.push(error.kind());
                    }
                    if dropped.elapsed() >= Duration::from_secs(1) {
                        let refused = format!("not bound within 1 s, refused with {refusals:?}");
                        break (None, refused);
                    }
                    // Probing the port again, not waiting for time to pass.
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        };
        let name = common::unique_name(&format!("port probe {attempt} held"));
        // The SDK might refuse a sender whose port is taken rather than move
        // it; that is a finding too.
        let next = match Sender::new(ndi, &SenderOptions::new(name)) {
            Ok(second) => {
                let (_, next_port) =
                    common::advertised_endpoint(ndi, second.source().unwrap().name()).unwrap();
                let verdict = match (&holder, next_port == port) {
                    (Some(_), true) => "the SDK's listener shares a port another socket holds",
                    (Some(_), false) => "the SDK's listener does not share a held port",
                    (None, true) => {
                        "the SDK's listener shares the port with what outlived its sender"
                    }
                    (None, false) => "the SDK moved off the port that outlived its sender",
                };
                format!("the next sender got {next_port}: {verdict}")
            }
            Err(error) => format!("the next sender could not be created: {error}"),
        };
        let _ = writeln!(
            report,
            "  a sender on {port} dropped; 0.0.0.0:{port} {bound}; {next}"
        );
        drop(holder);
    }
    report
}

#[test]
fn receivers_connect_to_recreated_senders_and_as_recreated_receivers() {
    let Some(ndi) = common::ndi() else { return };
    let started = Instant::now();
    let mut bandwidths = vec![ReceiverBandwidth::Highest, ReceiverBandwidth::Lowest];
    if ndi
        .require_feature(RuntimeFeature::ReceiveVideoOnly)
        .is_ok()
    {
        bandwidths.extend([
            ReceiverBandwidth::VideoOnlyHighest,
            ReceiverBandwidth::VideoOnlyLowest,
        ]);
    }
    let mitigation = common::loopback_mitigation();
    let probe = port_sharing(&ndi);

    let mut run = Run::new(&ndi);
    // The control differs from the helper only where the helper gives each
    // endpoint an address of its own.
    let routes: &[Route] = if mitigation.is_ok() {
        &[Route::Helper, Route::Control]
    } else {
        &[Route::Helper]
    };
    for &route in routes {
        run.recreated_senders(route);
    }
    for &route in routes {
        run.settled_receivers(route);
    }
    for &route in routes {
        run.recreated_receivers(route, &bandwidths);
    }

    let info = ndi.info();
    let mut report = format!(
        "peer recreation on {} with NDI {} {:?}, in {:.1?}\n",
        std::env::consts::OS,
        info.version().map_or_else(
            || "(unrecognized)".to_owned(),
            |version| version.to_string()
        ),
        info.edition(),
        started.elapsed()
    );
    let _ = writeln!(
        report,
        "endpoints: {}",
        match &mitigation {
            Ok(what) => (*what).to_owned(),
            Err(why) => format!("127.0.0.1 only, because {why}"),
        }
    );
    let _ = write!(report, "port probe:\n{probe}");
    for &route in routes {
        let cycles: Vec<_> = run
            .cycles
            .iter()
            .filter(|cycle| cycle.route == route)
            .collect();
        let stuck = cycles.iter().filter(|cycle| !cycle.connected()).count();
        let late = cycles
            .iter()
            .filter(|cycle| cycle.framed.is_some_and(|framed| framed >= GRACE))
            .count();
        let slowest = cycles
            .iter()
            .filter_map(|cycle| cycle.framed)
            .max()
            .map_or_else(|| "none".to_owned(), |slowest| format!("{slowest:.1?}"));
        let _ = writeln!(
            report,
            "{route}: {} receivers, {stuck} not given a frame, {late} given one only after \
             {GRACE:?}, {} cycles skipped; slowest frame {slowest}",
            cycles.len(),
            run.skipped[route as usize]
        );
    }
    for cycle in &run.cycles {
        let _ = writeln!(report, "{cycle}");
        if let Some(diagnosis) = &cycle.diagnosis {
            let _ = write!(report, "{diagnosis}");
        }
    }

    let helper_failed = run
        .cycles
        .iter()
        .any(|cycle| cycle.route == Route::Helper && !cycle.connected());
    assert!(
        !helper_failed && run.skipped[Route::Helper as usize] == 0,
        "a receiver given its endpoint by the shared helper did not connect to a sender that \
         exists\n{report}"
    );
    // Written directly: the harness captures `eprintln!` of passing tests, and
    // the timings are what a passing run is for.
    let _ = write!(std::io::stderr(), "{report}");
}
