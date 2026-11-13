//! The vendor's Discovery Server on a loopback port, for tests that need one.
//!
//! The server program ships in the SDK beside the runtime library:
//! `<root>/bin/<target>/ndi-discovery-server` next to `<root>/lib/<target>/`.

use grafton_ndi::{
    discovery::{AdvertiserOptions, ListenerOptions},
    Ndi,
};
use std::{
    io::{BufRead, BufReader, Read},
    net::{Ipv4Addr, TcpListener, TcpStream},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// How long the registry, or a starting server, may take to reflect a change.
///
/// An advertiser or listener that finds no server tries again every 10
/// seconds. The SDK documents no interval; that is what 6.4.0.2 does. One
/// created before its server serves therefore connects up to 10 seconds
/// after it does, and this allows twice that.
pub const SETTLE: Duration = Duration::from_secs(20);

/// How many ports are tried before a server is declared unable to start.
const START_ATTEMPTS: usize = 5;

/// The server program of the SDK the selected runtime belongs to, or `None`
/// when that runtime has none beside it.
pub fn server_binary(ndi: &Ndi) -> Option<PathBuf> {
    let directory = ndi.info().library_path()?.parent()?;
    let binary = directory
        .parent()?
        .parent()?
        .join("bin")
        .join(directory.file_name()?)
        .join("ndi-discovery-server");
    binary.is_file().then_some(binary)
}

/// A loopback port nothing is listening on at the moment.
pub fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Everything a server has written, collected as it arrives so that its pipes
/// never fill and a failure can show what the server said.
#[derive(Default)]
pub struct Output {
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
pub struct Server {
    pub process: Child,
    pub port: u16,
    output: Output,
}

impl Server {
    /// Start a server on a free port. Another process can take a port between
    /// its selection and the server binding it, so a server that exits is
    /// tried again elsewhere.
    pub fn start(binary: &Path) -> Self {
        Self::start_on_one_of(
            binary,
            std::iter::repeat_with(free_port).take(START_ATTEMPTS),
        )
    }

    /// Start a server on the first of `ports` it can bind.
    pub fn start_on_one_of(binary: &Path, ports: impl IntoIterator<Item = u16>) -> Self {
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
    pub fn start_on(binary: &Path, port: u16) -> Result<Self, String> {
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

    pub fn endpoint(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    pub fn advertisers(&self) -> AdvertiserOptions {
        AdvertiserOptions::default().servers(self.endpoint())
    }

    pub fn listeners(&self) -> ListenerOptions {
        ListenerOptions::default().server(self.endpoint())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}
