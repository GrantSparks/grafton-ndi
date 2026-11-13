//! Receivers connect to a sender of their own process, also when each is
//! created as soon as the one before it was dropped.
//!
//! On the hosted Windows runners receivers of the runtime suites sometimes
//! never connected to a sender of their own process at `127.0.0.1:<port>`.
//! Two causes have been told apart so far (`ci/README.md`, "Receivers that
//! never connect on Windows", has the runs):
//!
//! * The runner's firewall rules blocked multicast DNS, and with it some
//!   receivers' connections, until the runner gave the SDK a loopback discovery
//!   address, as the macOS runner does: in run 43366775596, 43 to 54 of 100
//!   receivers stalled without that address and none with it.
//! * With the address given, a receiver still stalled where it was created a
//!   few milliseconds after another receiver of the same sender, and of the
//!   same name, was dropped: the video-only test's second video-only receiver,
//!   and the image colour tests' second receiver.
//!
//! This test measures the second. One sender sends throughout, and receivers
//! are created one after another, each as soon as the one before was given a
//! frame and dropped. The arms take turns, receiver by receiver:
//!
//! * `same name`: the receiver takes the default name, which the receiver just
//!   dropped had too;
//! * `own name`: the receiver has a name no receiver had before;
//! * `rested`: the default name, with the receiver created [`REST`] after the
//!   one before was dropped.
//!
//! A receiver not given a frame within [`GRACE`], where those that connect are
//! given one in milliseconds, is diagnosed. For the first [`DIAGNOSED`] of each
//! arm: the Windows TCP table for the port is taken ([`common::socket_table`]),
//! and a second receiver, of a name of its own, is opened to the same
//! endpoint. The first of each arm is then left alone until the suites'
//! [`PATIENCE`] has passed, to show whether it connects by itself; after that,
//! and for every other one at once, [`Receiver::reconnect`] is called, to show
//! whether asking again revives it. The report names the configuration the SDK
//! read, has a summary line per arm and a line for each receiver that missed
//! [`GRACE`], and is printed whether the test passes or not.
mod common;

use grafton_ndi::{
    ConnectionStats, Ndi, Receiver, ReceiverOptions, Sender, SenderOptions, Source, VideoFrame,
};
use std::{
    fmt::{self, Write as _},
    io::Write as _,
    time::{Duration, Instant},
};

/// How long the suites wait for a receiver to deliver.
const PATIENCE: Duration = Duration::from_secs(15);

/// How long a receiver may take before it is diagnosed. Under the runners
/// receivers that connect are given a frame within milliseconds; with the
/// Windows firewall off, within about 2 s.
const GRACE: Duration = Duration::from_secs(5);

/// How long the `rested` arm waits between dropping a receiver and creating
/// the next.
const REST: Duration = Duration::from_millis(250);

/// The receivers of each arm.
const ROUNDS: usize = 25;

/// The receivers of each arm that missed [`GRACE`] and are diagnosed in full;
/// the rest are only recorded and asked to reconnect, which bounds the test's
/// time where many stall.
const DIAGNOSED: usize = 3;

/// One capture while waiting.
const SLICE: Duration = Duration::from_millis(10);

/// How a receiver is named and when it is created.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arm {
    SameName,
    OwnName,
    Rested,
}

impl Arm {
    /// The arms, in the order they take turns: each `same name` receiver
    /// follows a `rested` one, which has the default name.
    const ALL: [Self; 3] = [Self::SameName, Self::OwnName, Self::Rested];
}

impl fmt::Display for Arm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SameName => "same name",
            Self::OwnName => "own name",
            Self::Rested => "rested",
        })
    }
}

/// When something happened, since a receiver was created or asked again, or
/// that it never did.
fn at(time: Option<Duration>) -> String {
    time.map_or_else(|| "never".to_owned(), |time| format!("{time:.1?}"))
}

/// When a receiver first reported a connection, and when it was given a frame.
#[derive(Clone, Copy, Default)]
struct Outcome {
    counted: Option<Duration>,
    framed: Option<Duration>,
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "counted {}, frame {}", at(self.counted), at(self.framed))
    }
}

/// What one receiver did, and for one that missed [`GRACE`], what was tried.
struct Cycle {
    arm: Arm,
    round: usize,
    first: Outcome,
    /// A second receiver to the same endpoint, opened after [`GRACE`].
    second: Option<Outcome>,
    /// Whether the receiver connected by itself before [`PATIENCE`], when it
    /// was left alone that long.
    alone: Option<Outcome>,
    /// The receiver after [`Receiver::reconnect`], since that call.
    reconnected: Option<Outcome>,
    stats: ConnectionStats,
    sender_connections: String,
    table: Option<String>,
}

impl Cycle {
    fn prompt(&self) -> bool {
        self.first.framed.is_some()
    }
}

impl fmt::Display for Cycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:<9} round {:>2}: {}",
            self.arm.to_string(),
            self.round,
            self.first
        )?;
        if let Some(second) = self.second {
            write!(f, "; a second receiver: {second}")?;
        }
        if let Some(alone) = self.alone {
            write!(f, "; left alone to {PATIENCE:?}: {alone}")?;
        }
        if let Some(reconnected) = self.reconnected {
            write!(f, "; after reconnect(): {reconnected}")?;
        }
        write!(
            f,
            "; receiver connections {}, video {}; {}",
            self.stats.connections(),
            self.stats.video_frames_received(),
            self.sender_connections
        )
    }
}

/// The one sender, the endpoint every receiver is given, and the receivers
/// recorded so far.
struct Run<'a> {
    ndi: &'a Ndi,
    sender: Sender,
    source: Source,
    port: u16,
    video: VideoFrame,
    cycles: Vec<Cycle>,
    names: usize,
}

impl<'a> Run<'a> {
    fn new(ndi: &'a Ndi) -> Self {
        let sender = Sender::new(
            ndi,
            &SenderOptions::new(common::unique_name("recreated receivers"))
                .clock_video(false)
                .clock_audio(false),
        )
        .unwrap();
        let source = common::loopback_source(ndi, &sender).unwrap();
        let port = common::port_of(&source).expect("the helper names an IP endpoint");
        Self {
            ndi,
            sender,
            source,
            port,
            video: VideoFrame::builder().resolution(64, 32).build().unwrap(),
            cycles: Vec::new(),
            names: 0,
        }
    }

    /// Options for a receiver of a name no receiver had before.
    fn own_name(&mut self) -> ReceiverOptions {
        self.names += 1;
        ReceiverOptions::new(self.source.clone())
            .name(common::unique_name(&format!("receiver {}", self.names)))
    }

    /// The receivers of `arm` that missed [`GRACE`] so far.
    fn late(&self, arm: Arm) -> usize {
        self.cycles
            .iter()
            .filter(|cycle| cycle.arm == arm && !cycle.prompt())
            .count()
    }

    /// One receiver of `arm`, created as `arm` says after the one before was
    /// dropped, waited for, recorded and dropped.
    fn cycle(&mut self, arm: Arm, round: usize) {
        let options = match arm {
            Arm::SameName => ReceiverOptions::new(self.source.clone()),
            Arm::OwnName => self.own_name(),
            Arm::Rested => {
                std::thread::sleep(REST);
                ReceiverOptions::new(self.source.clone())
            }
        };
        let receiver = Receiver::new(self.ndi, &options).unwrap();
        let first = self.await_frame(&receiver, GRACE);
        let mut cycle = Cycle {
            arm,
            round,
            first,
            second: None,
            alone: None,
            reconnected: None,
            stats: receiver.connection_stats(),
            sender_connections: String::new(),
            table: None,
        };
        if first.framed.is_none() {
            let late = self.late(arm);
            if late < DIAGNOSED {
                cycle.table = common::socket_table(self.port);
                let second = Receiver::new(self.ndi, &self.own_name()).unwrap();
                cycle.second = Some(self.await_frame(&second, GRACE));
                drop(second);
            }
            if late == 0 {
                // The time already spent counts towards the suites' patience.
                let left = PATIENCE.saturating_sub(GRACE * 2);
                cycle.alone = Some(self.await_frame(&receiver, left));
            }
            if cycle.alone.is_none_or(|alone| alone.framed.is_none()) {
                receiver.reconnect().unwrap();
                cycle.reconnected = Some(self.await_frame(&receiver, GRACE));
            }
            cycle.stats = receiver.connection_stats();
        }
        cycle.sender_connections = match self.sender.connection_count(Duration::ZERO) {
            Ok(count) => format!("sender connections {count}"),
            Err(error) => format!("sender connection count failed: {error}"),
        };
        self.cycles.push(cycle);
        drop(receiver);
    }

    /// Send video until `receiver` is given a frame or `limit` has passed,
    /// noting when it first reports a connection.
    fn await_frame(&self, receiver: &Receiver, limit: Duration) -> Outcome {
        let since = Instant::now();
        let mut outcome = Outcome::default();
        while since.elapsed() < limit {
            self.sender.send_video(&self.video).unwrap();
            if outcome.counted.is_none() && receiver.connection_stats().connections() > 0 {
                outcome.counted = Some(since.elapsed());
            }
            if receiver.video().capture(SLICE).unwrap().is_some() {
                outcome.framed = Some(since.elapsed());
                break;
            }
        }
        outcome
    }
}

/// The configuration file the SDK reads, through `NDI_CONFIG_DIR`, or that
/// there is none: the CI runners give the SDK one, and what it says decides
/// which networks the SDK tries.
fn sdk_configuration() -> String {
    let Some(directory) = std::env::var_os("NDI_CONFIG_DIR") else {
        return "none (NDI_CONFIG_DIR is not set)".to_owned();
    };
    let file = std::path::Path::new(&directory).join("ndi-config.v1.json");
    match std::fs::read_to_string(&file) {
        Ok(text) => text.split_whitespace().collect::<Vec<_>>().join(" "),
        Err(error) => format!("{} could not be read: {error}", file.display()),
    }
}

#[test]
fn receivers_connect_when_each_follows_the_last() {
    let Some(ndi) = common::ndi() else { return };
    let started = Instant::now();
    let mut run = Run::new(&ndi);
    for round in 1..=ROUNDS {
        for arm in Arm::ALL {
            run.cycle(arm, round);
        }
    }

    let info = ndi.info();
    let mut report = format!(
        "receiver recreation on {} with NDI {} {:?}, in {:.1?}, to port {}\n",
        std::env::consts::OS,
        info.version().map_or_else(
            || "(unrecognized)".to_owned(),
            |version| version.to_string()
        ),
        info.edition(),
        started.elapsed(),
        run.port
    );
    let _ = writeln!(report, "SDK configuration: {}", sdk_configuration());
    for arm in Arm::ALL {
        let cycles: Vec<&Cycle> = run.cycles.iter().filter(|cycle| cycle.arm == arm).collect();
        let late: Vec<&Cycle> = cycles
            .iter()
            .copied()
            .filter(|cycle| !cycle.prompt())
            .collect();
        let counted = late.iter().filter(|cycle| cycle.first.counted.is_some());
        let tally = |outcome: fn(&Cycle) -> Option<Outcome>| {
            let tried: Vec<_> = late.iter().filter_map(|&cycle| outcome(cycle)).collect();
            let framed = tried.iter().filter(|tried| tried.framed.is_some()).count();
            format!("{framed} of {}", tried.len())
        };
        let slowest = cycles
            .iter()
            .filter_map(|cycle| cycle.first.framed)
            .max()
            .map_or_else(|| "none".to_owned(), |slowest| format!("{slowest:.1?}"));
        let _ = writeln!(
            report,
            "{arm}: {} receivers, {} given no frame within {GRACE:?} ({} of them counted a \
             connection); a second receiver: {}; connected alone by {PATIENCE:?}: {}; after \
             reconnect(): {}; slowest frame {slowest}",
            cycles.len(),
            late.len(),
            counted.count(),
            tally(|cycle| cycle.second),
            tally(|cycle| cycle.alone),
            tally(|cycle| cycle.reconnected),
        );
    }
    for cycle in run.cycles.iter().filter(|cycle| !cycle.prompt()) {
        let _ = writeln!(report, "{cycle}");
        if let Some(table) = &cycle.table {
            let _ = write!(report, "{table}");
        }
    }

    let late = Arm::ALL.iter().map(|&arm| run.late(arm)).sum::<usize>();
    assert!(
        late == 0,
        "receivers given no frame within {GRACE:?} by a sender of their own process: \
         {late}\n{report}"
    );
    // Written directly: the harness captures `eprintln!` of passing tests, and
    // the outcomes are what a passing run is for.
    let _ = write!(std::io::stderr(), "{report}");
}
