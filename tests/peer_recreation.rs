//! Receivers connect to a sender of their own process when each is created as
//! soon as the one before it was dropped.
//!
//! One sender sends throughout, and [`RECEIVERS`] receivers are created one
//! after another, each as soon as the one before was given a frame and
//! dropped. A receiver not given a frame within [`GRACE`], where those that
//! connect are given one in milliseconds, has stalled.
//!
//! Under the Windows test runner about half of such receivers stall, and with
//! the firewall off none does (`ci/README.md`, "Windows"). The report this
//! test prints is what that is diagnosed with. For the first [`DIAGNOSED`]
//! stalled receivers the Windows TCP table for the port is taken
//! ([`common::socket_table`]) and a second receiver is opened to the same
//! endpoint; the first is then left alone until the suites' [`PATIENCE`] has
//! passed, to show whether it connects by itself; after that, and for every
//! other one at once, [`Receiver::reconnect`] is called, to show whether
//! asking again revives it. After [`ENOUGH`] stalls the answer is in and the
//! run stops. The report names the configuration the SDK read, and is printed
//! whether the test passes or not.
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

/// How long a receiver may take before it has stalled. Under the runners
/// receivers that connect are given a frame within milliseconds; with the
/// Windows firewall off, within about 2 s.
const GRACE: Duration = Duration::from_secs(5);

/// The receivers created, unless [`ENOUGH`] stall first.
const RECEIVERS: usize = 40;

/// The stalls after which the run stops: each costs seconds, and these show
/// the failure.
const ENOUGH: usize = 6;

/// The stalled receivers that are diagnosed in full; the rest are only asked
/// to reconnect.
const DIAGNOSED: usize = 2;

/// One capture while waiting.
const SLICE: Duration = Duration::from_millis(10);

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

/// What one receiver did, and for one that stalled, what was tried.
struct Cycle {
    number: usize,
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
    fn stalled(&self) -> bool {
        self.first.framed.is_none()
    }
}

impl fmt::Display for Cycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "receiver {:>2}: {}", self.number, self.first)?;
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
        }
    }

    /// The receivers that stalled so far.
    fn stalled(&self) -> usize {
        self.cycles.iter().filter(|cycle| cycle.stalled()).count()
    }

    /// One receiver, created as soon as the one before was dropped, waited
    /// for, recorded and dropped.
    fn cycle(&mut self, number: usize) {
        let options = ReceiverOptions::new(self.source.clone());
        let receiver = Receiver::new(self.ndi, &options).unwrap();
        let first = self.await_frame(&receiver, GRACE);
        let mut cycle = Cycle {
            number,
            first,
            second: None,
            alone: None,
            reconnected: None,
            stats: receiver.connection_stats(),
            sender_connections: String::new(),
            table: None,
        };
        if first.framed.is_none() {
            let before = self.stalled();
            if before < DIAGNOSED {
                cycle.table = common::socket_table(self.port);
                let second = Receiver::new(self.ndi, &options).unwrap();
                cycle.second = Some(self.await_frame(&second, GRACE));
                drop(second);
            }
            if before == 0 {
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
    for number in 1..=RECEIVERS {
        run.cycle(number);
        if run.stalled() >= ENOUGH {
            break;
        }
    }

    let info = ndi.info();
    let mut report = format!(
        "receiver recreation on {} with NDI {} {:?}, in {:.1?}, to port {}, process {}\n",
        std::env::consts::OS,
        info.version().map_or_else(
            || "(unrecognized)".to_owned(),
            |version| version.to_string()
        ),
        info.edition(),
        started.elapsed(),
        run.port,
        std::process::id()
    );
    let _ = writeln!(report, "SDK configuration: {}", sdk_configuration());
    let stalled: Vec<&Cycle> = run.cycles.iter().filter(|cycle| cycle.stalled()).collect();
    let tally = |outcome: fn(&Cycle) -> Option<Outcome>| {
        let tried: Vec<_> = stalled.iter().filter_map(|&cycle| outcome(cycle)).collect();
        let framed = tried.iter().filter(|tried| tried.framed.is_some()).count();
        format!("{framed} of {}", tried.len())
    };
    let slowest = run
        .cycles
        .iter()
        .filter_map(|cycle| cycle.first.framed)
        .max()
        .map_or_else(|| "none".to_owned(), |slowest| format!("{slowest:.1?}"));
    let _ = writeln!(
        report,
        "{} of {} receivers were given no frame within {GRACE:?} ({} of them counted a \
         connection){}; a second receiver: {}; connected alone by {PATIENCE:?}: {}; after \
         reconnect(): {}; slowest frame {slowest}",
        stalled.len(),
        run.cycles.len(),
        stalled
            .iter()
            .filter(|cycle| cycle.first.counted.is_some())
            .count(),
        if run.cycles.len() < RECEIVERS {
            format!(", and the run stopped there, short of {RECEIVERS}")
        } else {
            String::new()
        },
        tally(|cycle| cycle.second),
        tally(|cycle| cycle.alone),
        tally(|cycle| cycle.reconnected),
    );
    for cycle in &stalled {
        let _ = writeln!(report, "{cycle}");
        if let Some(table) = &cycle.table {
            let _ = write!(report, "{table}");
        }
    }

    assert!(
        stalled.is_empty(),
        "receivers given no frame within {GRACE:?} by a sender of their own process: {}\n{report}",
        stalled.len()
    );
    // Written directly: the harness captures `eprintln!` of passing tests, and
    // the outcomes are what a passing run is for.
    let _ = write!(std::io::stderr(), "{report}");
}
