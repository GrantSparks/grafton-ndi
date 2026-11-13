//! Receivers connect to a sender of their own process, however its endpoint
//! was looked up and whatever was dropped just before.
//!
//! On the hosted Windows runners about one receiver in eight of the runtime
//! suites never connected to a sender of its own process, in run 43342188690
//! and again in 43352460772: given `127.0.0.1:5961`, it reported no
//! connection for the whole 15 s a test waits, the sender counted none, and
//! the TCP table held no connection of it, while a receiver opened later to the
//! same address connected in a millisecond. Linux and macOS have shown none.
//! In 43352460772 the runtime suites' receivers that stalled and those that
//! connected had opened alike: about 4 ms after their sender started, and
//! 65 ms after the previous test's receiver closed. Giving each receiver a
//! loopback address of its own, 127.0.0.2 and on, changed nothing.
//!
//! What every one of those receivers did follow, by a millisecond or two, is
//! the destruction of the finder that had just looked its sender up
//! ([`common::loopback_endpoint`]). Each cycle of this test creates a sender,
//! looks it up in one of the ways below, opens a receiver to
//! `127.0.0.1:<port>`, sends video until the receiver is given a frame, and
//! drops both. The arms take turns cycle by cycle, so each follows the
//! teardown of the cycle before as the suites' receivers do, and all of them
//! share the job's conditions:
//!
//! * `dropped`: [`common::loopback_endpoint`], as the suites look senders up: a
//!   finder is created, finds the sender and is dropped, and the receiver is
//!   created at once;
//! * `rested`: the same, with the receiver created [`REST`] after the finder
//!   was dropped;
//! * `held`: the finder that found the sender lives until the cycle ends;
//! * `unfound`, on Windows only: no finder at all. The port is the one this
//!   process listens on besides 5960, from the TCP table.
//!
//! `ci/README.md`, "Receivers that never connect on Windows", reads the
//! outcomes.
//!
//! A receiver not given a frame within [`GRACE`], where those that connect are
//! given one in milliseconds, is diagnosed. For the first [`DIAGNOSED`] of each
//! arm: the Windows TCP table for the port is taken ([`common::socket_table`]),
//! and a second receiver is opened to the same endpoint. The first of each arm
//! is then left alone until the suites' [`PATIENCE`] has passed, to show
//! whether it connects by itself; after that, and for every other one at once,
//! [`Receiver::reconnect`] is called, to show whether asking again revives it.
//! The report has a summary line per arm and a line for each receiver that
//! missed [`GRACE`], and is printed whether the test passes or not.
mod common;

use grafton_ndi::{
    ConnectionStats, Finder, Ndi, Receiver, ReceiverOptions, Sender, SenderOptions, Source,
    SourceAddress, VideoFrame,
};
use std::{
    fmt::{self, Write as _},
    io::Write as _,
    time::{Duration, Instant},
};

/// How long the suites wait for a receiver to deliver.
const PATIENCE: Duration = Duration::from_secs(15);

/// How long a receiver may take before it is diagnosed. Receivers that
/// connect are given a frame within milliseconds.
const GRACE: Duration = Duration::from_secs(2);

/// How long the `rested` arm waits between dropping the finder and creating
/// the receiver.
const REST: Duration = Duration::from_millis(250);

/// The cycles of each arm. At one receiver in eight, all 25 of an arm connect
/// with a chance of about 4%.
const ROUNDS: usize = 25;

/// The receivers of each arm that missed [`GRACE`] and are diagnosed in full;
/// the rest are only recorded and asked to reconnect, which bounds the test's
/// time where many stall.
const DIAGNOSED: usize = 3;

/// One capture while waiting.
const SLICE: Duration = Duration::from_millis(10);

/// How a cycle looks its sender up before its receiver is created.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lookup {
    Dropped,
    Rested,
    Held,
    Unfound,
}

impl Lookup {
    /// The arms this platform runs, in the order they take turns.
    fn arms() -> &'static [Self] {
        if cfg!(windows) {
            &[Self::Dropped, Self::Rested, Self::Held, Self::Unfound]
        } else {
            &[Self::Dropped, Self::Rested, Self::Held]
        }
    }
}

impl fmt::Display for Lookup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Dropped => "dropped",
            Self::Rested => "rested",
            Self::Held => "held",
            Self::Unfound => "unfound",
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

/// What one cycle's receiver did, and for one that missed [`GRACE`], what
/// was tried.
struct Cycle {
    lookup: Lookup,
    round: usize,
    port: u16,
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
            "{:<7} round {:>2} port {}: {}",
            self.lookup.to_string(),
            self.round,
            self.port,
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

/// The receivers recorded so far.
struct Run<'a> {
    ndi: &'a Ndi,
    video: VideoFrame,
    cycles: Vec<Cycle>,
    /// Cycles of the `unfound` arm that found no single listening port, with
    /// what they found.
    unresolved: Vec<String>,
    senders: usize,
}

impl<'a> Run<'a> {
    fn new(ndi: &'a Ndi) -> Self {
        Self {
            ndi,
            video: VideoFrame::builder().resolution(64, 32).build().unwrap(),
            cycles: Vec::new(),
            unresolved: Vec::new(),
            senders: 0,
        }
    }

    /// The receivers of `lookup` that missed [`GRACE`] so far.
    fn late(&self, lookup: Lookup) -> usize {
        self.cycles
            .iter()
            .filter(|cycle| cycle.lookup == lookup && !cycle.prompt())
            .count()
    }

    /// One cycle of `lookup`.
    fn cycle(&mut self, lookup: Lookup, round: usize) {
        self.senders += 1;
        let sender = Sender::new(
            self.ndi,
            &SenderOptions::new(common::unique_name(&format!("recreated {}", self.senders)))
                .clock_video(false)
                .clock_audio(false),
        )
        .unwrap();
        // The name the sender is advertised as, `HOST (name)`.
        let name = sender.source().unwrap().name().to_owned();
        // Kept, for `held`, until the cycle's receivers are dropped.
        let mut finder: Option<Finder> = None;
        let (source, port) = match lookup {
            Lookup::Dropped => endpoint(common::loopback_endpoint(self.ndi, &name).unwrap()),
            Lookup::Rested => {
                let found = common::loopback_endpoint(self.ndi, &name).unwrap();
                std::thread::sleep(REST);
                endpoint(found)
            }
            Lookup::Held => {
                let held = finder.insert(common::loopback_finder(self.ndi).unwrap());
                let (source, port) = common::advertised_endpoint_by(held, &name).unwrap();
                let loopback = SourceAddress::Ip(format!("127.0.0.1:{port}"));
                (source.with_address(loopback), port)
            }
            Lookup::Unfound => match listening_port() {
                Ok(port) => {
                    let address = SourceAddress::Ip(format!("127.0.0.1:{port}"));
                    (Source::new(&name).with_address(address), port)
                }
                Err(found) => {
                    self.unresolved.push(format!("round {round}: {found}"));
                    return;
                }
            },
        };
        let options = ReceiverOptions::new(source);
        let receiver = Receiver::new(self.ndi, &options).unwrap();
        let first = self.await_frame(&sender, &receiver, GRACE);
        let mut cycle = Cycle {
            lookup,
            round,
            port,
            first,
            second: None,
            alone: None,
            reconnected: None,
            stats: receiver.connection_stats(),
            sender_connections: String::new(),
            table: None,
        };
        if first.framed.is_none() {
            let late = self.late(lookup);
            if late < DIAGNOSED {
                cycle.table = common::socket_table(port);
                let second = Receiver::new(self.ndi, &options).unwrap();
                cycle.second = Some(self.await_frame(&sender, &second, GRACE));
                drop(second);
            }
            if late == 0 {
                // The time already spent counts towards the suites' patience.
                let left = PATIENCE.saturating_sub(GRACE * 2);
                cycle.alone = Some(self.await_frame(&sender, &receiver, left));
            }
            if cycle.alone.is_none_or(|alone| alone.framed.is_none()) {
                receiver.reconnect().unwrap();
                cycle.reconnected = Some(self.await_frame(&sender, &receiver, GRACE));
            }
            cycle.stats = receiver.connection_stats();
        }
        cycle.sender_connections = match sender.connection_count(Duration::ZERO) {
            Ok(count) => format!("sender connections {count}"),
            Err(error) => format!("sender connection count failed: {error}"),
        };
        self.cycles.push(cycle);
        drop(receiver);
        drop(finder);
        drop(sender);
    }

    /// Send video until `receiver` is given a frame or `limit` has passed,
    /// noting when it first reports a connection.
    fn await_frame(&self, sender: &Sender, receiver: &Receiver, limit: Duration) -> Outcome {
        let since = Instant::now();
        let mut outcome = Outcome::default();
        while since.elapsed() < limit {
            sender.send_video(&self.video).unwrap();
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

/// `found`, with its port.
fn endpoint(found: Source) -> (Source, u16) {
    let port = common::port_of(&found).expect("the helper names an IP endpoint");
    (found, port)
}

/// The one TCP port this process listens on besides 5960, the port of the
/// machine's source listing; or what the table held instead. Only the
/// `unfound` arm, which runs on Windows, asks.
fn listening_port() -> Result<u16, String> {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("netstat")
            .args(["-ano", "-p", "TCP"])
            .output()
            .map_err(|error| format!("netstat could not be run: {error}"))?;
        let pid = std::process::id().to_string();
        let ports: Vec<u16> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                // Proto, local address, foreign address, state, process id.
                match line.split_whitespace().collect::<Vec<_>>()[..] {
                    ["TCP", local, _, "LISTENING", owner] if owner == pid => {
                        local.rsplit(':').next()?.parse().ok()
                    }
                    _ => None,
                }
            })
            .filter(|&port| port != 5960)
            .collect();
        match ports[..] {
            [port] => Ok(port),
            _ => Err(format!("this process listens on {ports:?} besides 5960")),
        }
    }
    #[cfg(not(windows))]
    {
        Err("the TCP table is read on Windows only".to_owned())
    }
}

#[test]
fn receivers_connect_however_their_sender_was_looked_up() {
    let Some(ndi) = common::ndi() else { return };
    let started = Instant::now();
    let mut run = Run::new(&ndi);
    for round in 1..=ROUNDS {
        for &lookup in Lookup::arms() {
            run.cycle(lookup, round);
        }
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
    for &lookup in Lookup::arms() {
        let cycles: Vec<_> = run
            .cycles
            .iter()
            .filter(|cycle| cycle.lookup == lookup)
            .collect();
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
            "{lookup}: {} receivers, {} given no frame within {GRACE:?} ({} of them counted a \
             connection); connected alone by {PATIENCE:?}: {}; after reconnect(): {}; a second \
             receiver: {}; slowest frame {slowest}",
            cycles.len(),
            late.len(),
            counted.count(),
            tally(|cycle| cycle.alone),
            tally(|cycle| cycle.reconnected),
            tally(|cycle| cycle.second),
        );
    }
    for unresolved in &run.unresolved {
        let _ = writeln!(report, "unfound, skipped: {unresolved}");
    }
    for cycle in run.cycles.iter().filter(|cycle| !cycle.prompt()) {
        let _ = writeln!(report, "{cycle}");
        if let Some(table) = &cycle.table {
            let _ = write!(report, "{table}");
        }
    }

    let late = Lookup::arms()
        .iter()
        .map(|&lookup| run.late(lookup))
        .sum::<usize>();
    assert!(
        late == 0 && run.unresolved.is_empty(),
        "receivers given no frame within {GRACE:?} by a sender of their own process: \
         {late}\n{report}"
    );
    // Written directly: the harness captures `eprintln!` of passing tests, and
    // the outcomes are what a passing run is for.
    let _ = write!(std::io::stderr(), "{report}");
}
