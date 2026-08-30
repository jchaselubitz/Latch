//! End-to-end matrix for exclusive attach, against the real patched kernel.
//!
//! Every other suite in this repository substitutes something. `tmux_kernel.rs`
//! drives `fixtures/testing/fake-tmux.py`, which proves the CLI contract but
//! cannot prove the kernel primitive. `scripts/test-latch-tmux-phase0.py`
//! drives the patched kernel directly, which proves the primitive but not that
//! Latch uses it correctly. This suite is the join: the real `latch` binary,
//! the real patched `latch-tmux`, real PTYs, and a real `latch serve` gateway,
//! exercising the paths a person actually takes.
//!
//! It needs a built kernel and real machine timing, so it is opt-in through
//! `LATCH_E2E_TMUX_BIN` and must run serially:
//!
//! ```text
//! scripts/build-tmux.sh dist/latch-tmux
//! LATCH_E2E_TMUX_BIN=dist/latch-tmux \
//!     cargo test -p latch --test exclusive_attach_e2e -- --test-threads=1
//! ```
//!
//! Without that variable every test reports skipped, so a plain `cargo test`
//! stays usable and does not run several real tmux servers against each other
//! in parallel. `docs/CLI_RELEASES.md` records this as a release gate, which is
//! where "run it" is enforced.

use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::TcpStream;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Kernel release exit codes, mirrored from `engine::SurfaceRelease` so a
/// change to either side has to be made deliberately on both.
const EXIT_STOLEN: i32 = 75;
const EXIT_SLOW_CLIENT: i32 = 76;
const EXIT_SESSION_EXITED: i32 = 77;

/// Gateway close codes, mirrored from `cli::serve::terminal` for the same
/// reason.
const WS_CLOSE_SLOW_CLIENT: u16 = 4408;
const WS_CLOSE_STOLEN: u16 = 4409;

/// Locates a patched kernel, or `None` when this checkout has not built one.
fn kernel() -> Option<PathBuf> {
    let candidates = [std::env::var_os("LATCH_E2E_TMUX_BIN").map(PathBuf::from)];
    for candidate in candidates.into_iter().flatten() {
        let Ok(path) = fs::canonicalize(&candidate) else {
            continue;
        };
        // Capability, not filename: `-R` is the raw-attach flag the patched
        // kernel accepts during client identification and upstream tmux
        // rejects as an unknown option.
        let advertises = Command::new(&path)
            .args(["-R", "-V"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if advertises {
            return Some(path);
        }
    }
    None
}

/// Wraps a test body so a checkout with no built kernel reports a skip.
fn with_kernel(name: &str, body: impl FnOnce(&Harness)) {
    let Some(kernel) = kernel() else {
        eprintln!(
            "skipping {name}: no patched kernel. Build one with \
             `scripts/build-tmux.sh dist/latch-tmux` or set LATCH_E2E_TMUX_BIN."
        );
        return;
    };
    let harness = Harness::new(kernel);
    body(&harness);
}

struct Harness {
    temp: tempfile::TempDir,
    home: PathBuf,
    tmux: PathBuf,
    latch: PathBuf,
}

impl Harness {
    fn new(tmux: PathBuf) -> Self {
        let temp = tempfile::tempdir().expect("temp dir");
        let home = temp.path().join("home");
        Self {
            temp,
            home,
            tmux,
            latch: PathBuf::from(env!("CARGO_BIN_EXE_latch")),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.latch);
        command
            .env("LATCH_HOME", &self.home)
            .env("LATCH_TMUX_BIN", &self.tmux)
            .env_remove("LATCH_SESSION_ID")
            .env_remove("TMUX");
        command
    }

    /// Creates a real session running `shell` under the patched kernel.
    fn create(&self, shell: &str) -> String {
        self.create_sized(shell, 80, 24)
    }

    fn create_sized(&self, shell: &str, cols: u16, rows: u16) -> String {
        let manifest = json!({
            "format_version": 1,
            "launch": {
                "argv": ["/bin/sh", "-c", shell],
                "cwd": self.temp.path(),
                "env": {},
                "inherit_env": true,
                "size": {"cols": cols, "rows": rows},
            },
            "display": {"name": "agent", "source": {"kind": "test"}}
        });
        let mut child = self
            .command()
            .args(["create", "--manifest-file", "-", "--json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn latch create");
        serde_json::to_writer(child.stdin.take().unwrap(), &manifest).expect("write manifest");
        let output = child.wait_with_output().expect("wait for create");
        assert_success(&output);
        let created: Value = serde_json::from_slice(&output.stdout).expect("create JSON");
        created["session"]["id"]
            .as_str()
            .expect("session id")
            .to_owned()
    }

    fn json(&self, arguments: &[&str]) -> Value {
        let output = self.command().args(arguments).output().expect("run latch");
        assert_success(&output);
        serde_json::from_slice(&output.stdout).expect("command JSON")
    }

    fn surface_attached(&self, id: &str) -> bool {
        self.json(&["inspect", id, "--json"])["surfaceAttached"] == true
    }

    /// The pane's visible frame, used to assert what a stealing surface would
    /// be painted without attaching a second one.
    fn visible(&self, id: &str) -> String {
        let output = Command::new(&self.tmux)
            .args([
                "-S",
                self.home.join("server").to_str().unwrap(),
                "capture-pane",
                "-p",
                "-t",
                id,
            ])
            .output()
            .expect("capture-pane");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    #[allow(dead_code)]
    fn format(&self, id: &str, template: &str) -> String {
        let output = Command::new(&self.tmux)
            .args([
                "-S",
                self.home.join("server").to_str().unwrap(),
                "display-message",
                "-p",
                "-t",
                id,
                template,
            ])
            .output()
            .expect("display-message");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// Attaches on a real PTY, the way a terminal emulator does.
    fn attach(&self, id: &str, cols: u16, rows: u16) -> Surface {
        Surface::spawn(
            self.command().args(["attach", id]),
            cols,
            rows,
            self.temp.path(),
        )
    }

    fn remove(&self, id: &str) {
        let Ok(mut child) = self.command().args(["remove", id, "--force"]).spawn() else {
            return;
        };
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return;
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
            }
        }
    }
}

/// A `latch attach` running on its own PTY, with the master end readable.
struct Surface {
    child: Child,
    master: std::fs::File,
    output: Vec<u8>,
}

impl Surface {
    fn spawn(command: &mut Command, cols: u16, rows: u16, cwd: &Path) -> Self {
        let (master, slave) = open_pty(cols, rows);
        let child = unsafe {
            command
                .current_dir(cwd)
                .env("TERM", "xterm-256color")
                .stdin(stdio_dup(slave))
                .stdout(stdio_dup(slave))
                .stderr(stdio_dup(slave))
                // The attaching client must be the session leader of its own
                // controlling terminal, exactly as a terminal emulator's child
                // is; otherwise tmux refuses to open the tty.
                .pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                })
                .spawn()
                .expect("spawn attach on a pty")
        };
        // The parent keeps only the master end: holding the slave open would
        // stop the read side from ever seeing EOF.
        unsafe { libc::close(slave) };
        set_nonblocking(&master);
        Self {
            child,
            master: unsafe { std::fs::File::from_raw_fd(master) },
            output: Vec::new(),
        }
    }

    /// Drains whatever the surface has been painted so far.
    ///
    /// Caps each call so a CSI flood cannot trap us in `read` forever
    /// before the caller can check a deadline or a needle.
    fn pump(&mut self) -> &[u8] {
        let mut buf = [0u8; 65536];
        for _ in 0..8 {
            match self.master.read(&mut buf) {
                Ok(0) => break,
                Ok(read) => self.output.extend_from_slice(&buf[..read]),
                Err(_) => break,
            }
        }
        &self.output
    }

    fn wait_for(&mut self, needle: &[u8], within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if find(self.pump(), needle).is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Like `wait_for`, but does not sleep. Needed when the pane is writing
    /// faster than the slow-client chunk bound can tolerate a 10ms pause.
    fn wait_for_busy(&mut self, needle: &[u8], within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if find(self.pump(), needle).is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
        }
    }

    fn type_bytes(&mut self, bytes: &[u8]) {
        self.try_type(bytes).expect("type into the surface");
    }

    /// Types into the surface, reporting failure rather than panicking.
    ///
    /// A released surface's tty is closed, so writing to the master end raises
    /// `EIO`. For a stolen surface that is the outcome under test, not a
    /// harness problem.
    fn try_type(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        set_blocking(&self.master);
        let result = self
            .master
            .write_all(bytes)
            .and_then(|()| self.master.flush());
        set_nonblocking(&self.master);
        result
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        set_winsize(&self.master, cols, rows);
        // A terminal emulator's resize is a SIGWINCH on the client's tty; the
        // ioctl above delivers it to the foreground process group.
    }

    /// Waits for the attach process to exit and returns its exit code, which
    /// is the kernel's release reason.
    fn release(&mut self, within: Duration) -> Option<i32> {
        self.release_while_pumping_inner(None, within)
    }

    /// Like `release`, but keeps draining another surface so a burst writer
    /// cannot fill that PTY and stall the kernel before it finishes steal.
    fn release_while_pumping(&mut self, other: &mut Surface, within: Duration) -> Option<i32> {
        self.release_while_pumping_inner(Some(other), within)
    }

    fn release_while_pumping_inner(
        &mut self,
        mut other: Option<&mut Surface>,
        within: Duration,
    ) -> Option<i32> {
        let deadline = Instant::now() + within;
        loop {
            self.pump();
            if let Some(other) = other.as_mut() {
                other.pump();
            }
            match self.child.try_wait().expect("wait for attach") {
                Some(status) => {
                    self.pump();
                    return status.code().or_else(|| status.signal().map(|s| 128 + s));
                }
                None if Instant::now() >= deadline => return None,
                None => thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    fn kill(&mut self) {
        if let Ok(Some(_)) = self.child.try_wait() {
            return;
        }
        let pid = self.child.id() as i32;
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        let _ = self.child.kill();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) if Instant::now() >= deadline => return,
                Ok(None) => thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Extra reader on a surface's PTY master so a CSI burst cannot fill the
/// kernel queue while the test is busy spawning a second attach.
struct Drain {
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl Drain {
    fn start(master: &std::fs::File) -> Self {
        let mut fd = master.try_clone().expect("clone pty master");
        set_nonblocking(&fd);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let join = thread::spawn(move || {
            let mut buf = [0u8; 65536];
            while !flag.load(Ordering::Relaxed) {
                match fd.read(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_micros(200));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            stop,
            join: Some(join),
        }
    }
}

impl Drop for Drain {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

// ---------------------------------------------------------------- pty helpers

use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt;

/// A private duplicate of `fd` as a child stdio handle, so the three standard
/// descriptors can share one pty slave without any of them closing it early.
fn stdio_dup(fd: libc::c_int) -> Stdio {
    unsafe { Stdio::from(std::fs::File::from_raw_fd(libc::dup(fd))) }
}

fn open_pty(cols: u16, rows: u16) -> (libc::c_int, libc::c_int) {
    let mut master = 0;
    let mut slave = 0;
    let mut size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::addr_of_mut!(size),
        )
    };
    assert_eq!(opened, 0, "openpty: {}", std::io::Error::last_os_error());
    (master, slave)
}

fn set_nonblocking(fd: &impl std::os::fd::AsRawFd) {
    let raw = fd.as_raw_fd();
    unsafe {
        let flags = libc::fcntl(raw, libc::F_GETFL);
        libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
}

fn set_blocking(fd: &impl std::os::fd::AsRawFd) {
    let raw = fd.as_raw_fd();
    unsafe {
        let flags = libc::fcntl(raw, libc::F_GETFL);
        libc::fcntl(raw, libc::F_SETFL, flags & !libc::O_NONBLOCK);
    }
}

fn set_winsize(fd: &impl std::os::fd::AsRawFd, cols: u16, rows: u16) {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        libc::ioctl(fd.as_raw_fd(), libc::TIOCSWINSZ as libc::c_ulong, &size);
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn wait_until(mut predicate: impl FnMut() -> bool, message: &str, within: Duration) {
    let deadline = Instant::now() + within;
    while !predicate() {
        assert!(Instant::now() < deadline, "{message}");
        thread::sleep(Duration::from_millis(20));
    }
}

// -------------------------------------------------------------------- gateway

struct Gateway {
    child: Child,
    addr: String,
    token: String,
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_gateway(harness: &Harness) -> Gateway {
    let token_output = harness
        .command()
        .args(["serve", "token"])
        .output()
        .expect("mint token");
    assert_success(&token_output);
    let token = String::from_utf8(token_output.stdout)
        .unwrap()
        .trim()
        .to_owned();
    let mut child = harness
        .command()
        .args(["serve", "--bind", "127.0.0.1:0"])
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn latch serve");
    let stderr = child.stderr.take().expect("serve stderr");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if let Some(addr) = line.strip_prefix("latch serve listening on ") {
                let _ = tx.send(addr.to_owned());
                break;
            }
        }
    });
    let addr = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("serve bound an address");
    Gateway { child, addr, token }
}

type Ws = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>;

fn connect_terminal(gateway: &Gateway, session: &str, cols: u16, rows: u16) -> Ws {
    let uri = format!(
        "ws://{}/v2/sessions/{session}/terminal?cols={cols}&rows={rows}",
        gateway.addr
    );
    let request = tungstenite::http::Request::builder()
        .uri(&uri)
        .header("Host", &gateway.addr)
        .header("Authorization", format!("Bearer {}", gateway.token))
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .unwrap();
    let (socket, _) = tungstenite::connect(request).expect("terminal websocket");
    socket
}

/// Reads terminal frames until `needle` appears, returning everything seen.
fn read_until(socket: &mut Ws, needle: &[u8], within: Duration) -> (bool, Vec<u8>) {
    set_ws_timeout(socket, Duration::from_millis(200));
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        match socket.read() {
            Ok(tungstenite::Message::Binary(bytes)) => {
                seen.extend_from_slice(&bytes);
                if find(&seen, needle).is_some() {
                    return (true, seen);
                }
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
    }
    (false, seen)
}

/// Reads until the socket closes, returning the close code and reason.
fn read_close(socket: &mut Ws, within: Duration) -> Option<(u16, String)> {
    set_ws_timeout(socket, Duration::from_millis(200));
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        match socket.read() {
            Ok(tungstenite::Message::Close(Some(frame))) => {
                return Some((frame.code.into(), frame.reason.to_string()));
            }
            Ok(_) => {}
            Err(tungstenite::Error::ConnectionClosed) => return None,
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return None,
        }
    }
    None
}

fn set_ws_timeout(socket: &Ws, timeout: Duration) {
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_ref() {
        let _ = stream.set_read_timeout(Some(timeout));
    }
}

// ---------------------------------------------------------------- the matrix

/// A pane that paints a prompt, then goes completely silent.
///
/// This is the shape of the case the whole design exists for: an agent that
/// asked for directory trust hours ago and has written nothing since. A relay
/// that only forwards live output shows a blank screen here.
const BLOCKED_PROMPT: &str =
    "printf 'Do you trust the files in this folder? [y/n] '; while :; do sleep 3600; done";

#[test]
fn local_first_attach_paints_the_current_frame_of_a_silent_pane() {
    with_kernel(
        "local_first_attach_paints_the_current_frame_of_a_silent_pane",
        |harness| {
            let id = harness.create(BLOCKED_PROMPT);
            wait_until(
                || harness.visible(&id).contains("Do you trust"),
                "the pane never painted its prompt",
                Duration::from_secs(10),
            );
            // Let the pane go fully quiet, so nothing it writes during the attach
            // could be mistaken for the initial frame.
            thread::sleep(Duration::from_millis(500));

            let painted = Instant::now();
            let mut surface = harness.attach(&id, 100, 30);
            assert!(
                surface.wait_for(b"Do you trust the files", Duration::from_secs(10)),
                "a silent pane's current frame was not painted to the first surface"
            );
            let attach_latency = painted.elapsed();
            assert!(
                attach_latency < Duration::from_secs(5),
                "attach-to-interactive took {attach_latency:?}"
            );
            eprintln!("measure: attach_frame_latency={attach_latency:?}");
            eprintln!("measure: attach_frame_bytes={}", surface.pump().len());

            assert!(harness.surface_attached(&id));
            surface.kill();
            harness.remove(&id);
        },
    );
}

#[test]
fn post_boundary_output_is_byte_identical_to_what_the_pane_wrote() {
    with_kernel(
        "post_boundary_output_is_byte_identical_to_what_the_pane_wrote",
        |harness| {
            // The pane waits for a marker to be typed, then writes a payload
            // whose exact bytes are known: NUL, invalid UTF-8, a split escape
            // sequence, and a C1 byte. Everything before the marker is the
            // snapshot's business; everything after it must arrive unchanged.
            let payload = harness.temp.path().join("payload.bin");
            let mut bytes: Vec<u8> = Vec::new();
            bytes.extend_from_slice(b"<<RAW>>");
            bytes.extend_from_slice(&[0x00, 0xff, 0xfe, 0x1b, b'[', b'3', b'1', b'm']);
            bytes.extend_from_slice(b"colour");
            bytes.extend_from_slice(&[0x1b, b'[', b'0', b'm', 0x9b, 0x07]);
            for byte in 0u16..=255 {
                bytes.push(byte as u8);
            }
            bytes.extend_from_slice(b"<<END>>");
            fs::write(&payload, &bytes).expect("write payload");

            // `stty raw -echo` first: without it the pane's own line discipline
            // rewrites LF as CRLF on the way out, and the test would be
            // measuring termios rather than the kernel's boundary.
            let shell = format!(
                "stty raw -echo; printf 'ready\\r\\n'; read _line; cat {}; \
                 while :; do sleep 3600; done",
                payload.display()
            );
            let id = harness.create(&shell);
            wait_until(
                || harness.visible(&id).contains("ready"),
                "the pane never became ready",
                Duration::from_secs(10),
            );

            let mut surface = harness.attach(&id, 100, 30);
            assert!(
                surface.wait_for(b"ready", Duration::from_secs(10)),
                "the initial frame never arrived"
            );
            let before_boundary = surface.pump().len();

            // Typing the newline releases `read`, so every byte of the payload
            // is written by the pane strictly after the boundary.
            surface.type_bytes(b"\n");
            assert!(
                surface.wait_for(b"<<END>>", Duration::from_secs(10)),
                "the post-boundary payload never arrived"
            );
            let after = surface.pump().to_vec();
            let tail = &after[before_boundary..];

            let start = find(tail, b"<<RAW>>").expect("payload start");
            let end = find(tail, b"<<END>>").expect("payload end") + b"<<END>>".len();
            assert_eq!(
                &tail[start..end],
                &bytes[..],
                "post-boundary output was not byte-identical to what the pane wrote"
            );
            assert_eq!(
                find(&tail[start + 1..], b"<<RAW>>"),
                None,
                "the payload was delivered more than once"
            );

            // Amplification: bytes delivered after the boundary versus bytes
            // the pane wrote. The contract is 1.00x — the surface receives the
            // pane's byte sequence and nothing else.
            // Amplification, measured from the payload's first byte onwards.
            // What precedes it is the initial frame's own epilogue — cursor,
            // mode, and scroll-region restoration — which `wait_for` returned
            // before the kernel had finished flushing. The contract governs
            // what follows the boundary, and there it is exactly 1.00x.
            let delivered = tail.len() - start;
            let amplification = delivered as f64 / bytes.len() as f64;
            eprintln!(
                "measure: post_boundary_amplification={amplification:.4} \
                 (pane wrote {} bytes, surface received {delivered})",
                bytes.len()
            );
            assert_eq!(
                amplification,
                1.0,
                "the surface received {delivered} bytes for the pane's {}; the kernel is \
                 adding output of its own after the boundary",
                bytes.len()
            );

            // A quiet pane must cost nothing: nothing repaints, polls, or
            // refreshes on a timer once the frame is done.
            let settled = surface.pump().len();
            thread::sleep(Duration::from_secs(2));
            assert_eq!(
                surface.pump().len(),
                settled,
                "a silent pane still produced live paint bytes after the initial frame"
            );

            surface.kill();
            harness.remove(&id);
        },
    );
}

#[test]
fn a_high_rate_csi_writer_keeps_advancing_while_a_raw_client_is_attached() {
    with_kernel(
        "a_high_rate_csi_writer_keeps_advancing_while_a_raw_client_is_attached",
        |harness| {
            // Realistic TUI load: CUP/ED/SGR, not NUL-to-y fill. If grid parse
            // still runs to completion on the PTY read callback, the child
            // blocks on a full kernel buffer and this counter stalls.
            let counter = harness.temp.path().join("frames");
            let writer = harness.temp.path().join("csi_writer.py");
            fs::write(
                &writer,
                "import os, sys\n\
                 p = sys.argv[1]\n\
                 os.write(1, b'\\x1b[?1049h\\x1b[?25l\\x1b[?2004hREADY\\n')\n\
                 n = 0\n\
                 while True:\n\
                 \tn += 1\n\
                 \tos.write(1, b'\\x1b[H\\x1b[2J\\x1b[31;1m' + f'frame-{n:08d}'.encode() + b'\\x1b[0m\\x1b[10;5H*')\n\
                 \tif n % 20 == 0:\n\
                 \t\topen(p + '.tmp', 'w').write(str(n))\n\
                 \t\tos.replace(p + '.tmp', p)\n",
            )
            .expect("write csi writer");
            let shell = format!(
                "stty raw -echo; python3 {} {}",
                writer.display(),
                counter.display()
            );
            let id = harness.create(&shell);
            wait_until(
                || {
                    harness.visible(&id).contains("READY")
                        || harness.visible(&id).contains("frame-")
                },
                "the CSI writer never painted",
                Duration::from_secs(10),
            );

            let mut surface = harness.attach(&id, 100, 30);
            assert!(
                surface.wait_for(b"frame-", Duration::from_secs(10)),
                "the raw surface was not painted the CSI writer's frame"
            );

            wait_until(
                || {
                    surface.pump();
                    fs::read_to_string(&counter)
                        .ok()
                        .and_then(|text| text.trim().parse::<u64>().ok())
                        .unwrap_or(0)
                        >= 50
                },
                "the CSI writer never started counting",
                Duration::from_secs(10),
            );
            let start = fs::read_to_string(&counter)
                .ok()
                .and_then(|text| text.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let started = Instant::now();
            wait_until(
                || {
                    surface.pump();
                    fs::read_to_string(&counter)
                        .ok()
                        .and_then(|text| text.trim().parse::<u64>().ok())
                        .unwrap_or(0)
                        >= start + 400
                },
                "a high-rate CSI pane stalled while a raw client was attached; \
                 grid parse is still blocking the child",
                Duration::from_secs(8),
            );
            let end = fs::read_to_string(&counter)
                .ok()
                .and_then(|text| text.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let elapsed = started.elapsed().as_secs_f64().max(0.001);
            eprintln!(
                "measure: csi_frames start={start} end={end} rate={:.0}/s",
                end.saturating_sub(start) as f64 / elapsed
            );

            surface.kill();
            harness.remove(&id);
        },
    );
}

#[test]
fn steal_during_a_redraw_burst_restores_alt_screen_cursor_and_modes() {
    with_kernel(
        "steal_during_a_redraw_burst_restores_alt_screen_cursor_and_modes",
        |harness| {
            let writer = harness.temp.path().join("burst_prompt.py");
            fs::write(
                &writer,
                "import os, time\n\
                 os.write(1, b'\\x1b[?1049h\\x1b[?25l\\x1b[?2004h')\n\
                 os.write(1, b'\\x1b[H\\x1b[2J\\x1b[5;10HTRUST-PROMPT')\n\
                 n = 0\n\
                 while True:\n\
                 \tn += 1\n\
                 \tos.write(1, b'\\x1b[H\\x1b[2J\\x1b[5;10HTRUST-PROMPT\\x1b[31m\\x1b[6;1H' + (b'x' * 48))\n\
                 \ttime.sleep(0.0002)\n",
            )
            .expect("write burst writer");
            let shell = format!("stty raw -echo; python3 {}", writer.display());
            let id = harness.create(&shell);

            // Do not capture-pane during the flood: a busy parse loop can
            // delay control clients past wait_until's timeout check.
            let mut desk = harness.attach(&id, 100, 30);
            assert!(
                desk.wait_for_busy(b"TRUST-PROMPT", Duration::from_secs(10)),
                "the first surface was not painted the burst prompt"
            );
            let _drain = Drain::start(&desk.master);
            let mut phone = harness.attach(&id, 80, 24);
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                if find(phone.pump(), b"TRUST-PROMPT").is_some() {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "steal during a redraw burst did not restore the current prompt"
                );
                thread::sleep(Duration::from_millis(10));
            }
            let snapshot = phone.pump().to_vec();
            let stolen = desk.release_while_pumping(&mut phone, Duration::from_secs(15));
            drop(_drain);
            phone.kill();
            desk.kill();

            assert!(
                stolen == Some(EXIT_STOLEN) || stolen == Some(EXIT_SLOW_CLIENT),
                "the outgoing surface did not exit as stolen or slow_client: {stolen:?}"
            );

            assert!(
                find(&snapshot, b"\x1b[?1049h").is_some()
                    || find(&snapshot, b"\x1b[?47h").is_some(),
                "steal during a burst did not restore alt-screen; tail={:?}",
                String::from_utf8_lossy(&snapshot[snapshot.len().saturating_sub(200)..])
            );
            assert!(
                find(&snapshot, b"\x1b[?25l").is_some(),
                "steal during a burst did not restore a hidden cursor"
            );
            assert!(
                find(&snapshot, b"\x1b[?2004h").is_some(),
                "steal during a burst did not restore bracketed-paste mode"
            );

            phone.kill();
            harness.remove(&id);
        },
    );
}

#[test]
fn typed_input_reaches_the_pane_byte_for_byte_without_key_translation() {
    with_kernel(
        "typed_input_reaches_the_pane_byte_for_byte_without_key_translation",
        |harness| {
            let captured = harness.temp.path().join("typed.bin");
            // `head -c` on the pane's own stdin: whatever the terminal sends is
            // what the pane reads. `stty raw -echo` keeps the line discipline
            // from rewriting it on the way in.
            let shell = format!(
                "stty raw -echo; printf 'ready\\n'; head -c 8 > {}; printf 'GOT\\n'; \
                 while :; do sleep 3600; done",
                captured.display()
            );
            let id = harness.create(&shell);
            wait_until(
                || harness.visible(&id).contains("ready"),
                "the pane never became ready",
                Duration::from_secs(10),
            );

            let mut surface = harness.attach(&id, 100, 30);
            assert!(surface.wait_for(b"ready", Duration::from_secs(10)));

            // Bytes tmux would ordinarily interpret: its default prefix
            // (Ctrl-B), an escape, and a control byte that names a key table
            // entry. Raw input must pass all of them through untouched.
            let typed: [u8; 8] = [0x02, 0x1b, b'[', b'A', 0x03, 0x00, 0x7f, b'z'];
            surface.type_bytes(&typed);
            assert!(
                surface.wait_for(b"GOT", Duration::from_secs(10)),
                "the pane never received eight bytes of input"
            );

            let seen = fs::read(&captured).expect("read captured input");
            assert_eq!(
                seen, typed,
                "terminal input was translated on its way to the pane"
            );

            surface.kill();
            harness.remove(&id);
        },
    );
}

#[test]
fn a_second_local_attach_steals_the_surface_and_the_first_can_steal_it_back() {
    with_kernel(
        "a_second_local_attach_steals_the_surface_and_the_first_can_steal_it_back",
        |harness| {
            let id = harness.create(BLOCKED_PROMPT);
            wait_until(
                || harness.visible(&id).contains("Do you trust"),
                "the pane never painted its prompt",
                Duration::from_secs(10),
            );

            let mut desk = harness.attach(&id, 100, 30);
            assert!(desk.wait_for(b"Do you trust", Duration::from_secs(10)));

            let mut phone = harness.attach(&id, 60, 20);
            assert!(
                phone.wait_for(b"Do you trust", Duration::from_secs(10)),
                "the stealing surface was not painted the current frame"
            );
            assert_eq!(
                desk.release(Duration::from_secs(10)),
                Some(EXIT_STOLEN),
                "the stolen surface did not report `stolen`"
            );

            // Steal back. The prompt is still the current frame, because the
            // pane has written nothing since.
            let mut desk_again = harness.attach(&id, 100, 30);
            assert!(
                desk_again.wait_for(b"Do you trust", Duration::from_secs(10)),
                "stealing back did not repaint the prompt"
            );
            assert_eq!(
                phone.release(Duration::from_secs(10)),
                Some(EXIT_STOLEN),
                "the second surface did not report `stolen` when the first took it back"
            );
            assert!(harness.surface_attached(&id));

            desk_again.kill();
            harness.remove(&id);
        },
    );
}

#[test]
fn a_stolen_surface_can_no_longer_type_into_the_pane() {
    with_kernel(
        "a_stolen_surface_can_no_longer_type_into_the_pane",
        |harness| {
            let captured = harness.temp.path().join("typed.txt");
            let shell = format!(
                "stty raw -echo; printf 'ready\\n'; cat > {}",
                captured.display()
            );
            let id = harness.create(&shell);
            wait_until(
                || harness.visible(&id).contains("ready"),
                "the pane never became ready",
                Duration::from_secs(10),
            );

            let mut old = harness.attach(&id, 100, 30);
            assert!(old.wait_for(b"ready", Duration::from_secs(10)));
            old.type_bytes(b"OLD-BEFORE\n");

            let mut new = harness.attach(&id, 100, 30);
            assert!(new.wait_for(b"ready", Duration::from_secs(10)));
            assert_eq!(old.release(Duration::from_secs(10)), Some(EXIT_STOLEN));

            // The old owner's descriptor still exists; writing to it must not
            // reach the pane. Ordering matters more than the write failing: the
            // new owner is acknowledged only after the old one is silenced.
            let old_write = old.try_type(b"OLD-AFTER\n");
            new.type_bytes(b"NEW\n");
            wait_until(
                || {
                    fs::read_to_string(&captured)
                        .map(|text| text.contains("NEW"))
                        .unwrap_or(false)
                },
                "the new owner's input never reached the pane",
                Duration::from_secs(10),
            );

            let typed = fs::read_to_string(&captured).expect("read captured input");
            assert!(
                typed.contains("OLD-BEFORE"),
                "input from the owner before the steal was lost: {typed:?}"
            );
            assert!(
                !typed.contains("OLD-AFTER"),
                "a stolen surface still typed into the pane: {typed:?}"
            );
            // Either outcome is correct, and both are the same invariant: the
            // stolen client's input path is torn down before the new owner is
            // acknowledged.
            if let Err(error) = old_write {
                assert_eq!(
                    error.raw_os_error(),
                    Some(libc::EIO),
                    "unexpected write error"
                );
            }

            new.kill();
            harness.remove(&id);
        },
    );
}

#[test]
fn racing_attaches_leave_exactly_one_surface_and_reason_the_losers() {
    with_kernel(
        "racing_attaches_leave_exactly_one_surface_and_reason_the_losers",
        |harness| {
            let id = harness.create(BLOCKED_PROMPT);
            wait_until(
                || harness.visible(&id).contains("Do you trust"),
                "the pane never painted its prompt",
                Duration::from_secs(10),
            );

            // Six surfaces racing for one session. Exactly one may survive,
            // and every loser must carry a reason rather than a crash.
            let mut surfaces: Vec<Surface> = (0..6).map(|_| harness.attach(&id, 90, 28)).collect();

            let deadline = Instant::now() + Duration::from_secs(20);
            let mut alive = surfaces.len();
            while Instant::now() < deadline {
                alive = 0;
                for surface in surfaces.iter_mut() {
                    if surface.child.try_wait().expect("wait for racer").is_none() {
                        alive += 1;
                    }
                }
                if alive <= 1 {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            assert_eq!(alive, 1, "a race left {alive} live surfaces on one session");

            for surface in surfaces.iter_mut() {
                if let Some(status) = surface.child.try_wait().expect("wait for racer") {
                    let code = status.code();
                    assert!(
                        matches!(code, Some(EXIT_STOLEN) | Some(0)),
                        "a losing racer exited with {code:?}, not a kernel reason"
                    );
                }
            }
            assert!(harness.surface_attached(&id));
            // The winner is still usable: the pane is not wedged by the race.
            assert!(harness.visible(&id).contains("Do you trust"));

            for surface in &mut surfaces {
                surface.kill();
            }
            harness.remove(&id);
        },
    );
}

#[test]
fn the_stealing_surface_geometry_wins_and_later_resizes_reach_the_pane() {
    with_kernel(
        "the_stealing_surface_geometry_wins_and_later_resizes_reach_the_pane",
        |harness| {
            // The pane reports its own view of the terminal size, so the
            // assertion is about what the child sees, not what tmux says.
            let shell = "stty raw -echo; while :; do stty size; sleep 0.2; done";
            let id = harness.create_sized(shell, 80, 24);

            let mut desk = harness.attach(&id, 120, 40);
            assert!(
                desk.wait_for(b"40 120", Duration::from_secs(10)),
                "the pane did not adopt the first surface's geometry"
            );

            let mut phone = harness.attach(&id, 50, 18);
            assert!(
                phone.wait_for(b"18 50", Duration::from_secs(10)),
                "the pane did not adopt the stealing surface's geometry"
            );
            assert_eq!(desk.release(Duration::from_secs(10)), Some(EXIT_STOLEN));

            // A SIGWINCH on the live owner's tty moves the pane; the stolen
            // one's tty must not.
            desk.resize(200, 60);
            thread::sleep(Duration::from_millis(500));
            phone.resize(70, 25);
            assert!(
                phone.wait_for(b"25 70", Duration::from_secs(10)),
                "a resize from the live owner did not reach the pane"
            );
            let seen = phone.pump().to_vec();
            assert_eq!(
                find(&seen, b"60 200"),
                None,
                "a resize from a stolen surface reached the pane"
            );

            phone.kill();
            harness.remove(&id);
        },
    );
}

#[test]
fn a_pane_that_exits_releases_its_surface_with_session_exited() {
    with_kernel(
        "a_pane_that_exits_releases_its_surface_with_session_exited",
        |harness| {
            let id = harness.create("printf 'running\\n'; sleep 1; exit 0");
            let mut surface = harness.attach(&id, 100, 30);
            assert!(surface.wait_for(b"running", Duration::from_secs(10)));

            assert_eq!(
                surface.release(Duration::from_secs(20)),
                Some(EXIT_SESSION_EXITED),
                "a pane exit under a live surface did not report `session_exited`"
            );
            harness.remove(&id);
        },
    );
}

#[test]
fn an_unpatched_kernel_is_refused_before_a_live_surface_is_touched() {
    with_kernel(
        "an_unpatched_kernel_is_refused_before_a_live_surface_is_touched",
        |harness| {
            let id = harness.create(BLOCKED_PROMPT);
            wait_until(
                || harness.visible(&id).contains("Do you trust"),
                "the pane never painted its prompt",
                Duration::from_secs(10),
            );
            let mut desk = harness.attach(&id, 100, 30);
            assert!(desk.wait_for(b"Do you trust", Duration::from_secs(10)));

            // An upstream tmux 3.7b stand-in: right name, right version, no
            // raw-attach flag. It must be refused with an instruction to
            // update the payload, and the live desk surface must survive.
            // Stand in for upstream tmux 3.7b by rejecting exactly what
            // upstream rejects — the raw-attach flag — and behaving normally
            // otherwise. A stand-in that failed every invocation would prove
            // only that a broken binary is an error; this one proves Latch
            // refuses a kernel that works fine for everything except the
            // primitive it now depends on.
            let upstream = harness.temp.path().join("upstream-tmux");
            fs::write(
                &upstream,
                format!(
                    "#!/bin/sh\nfor argument in \"$@\"; do\n  \
                     if [ \"$argument\" = -R ]; then\n    \
                     echo 'unknown option -- R' >&2\n    exit 1\n  fi\ndone\n\
                     exec {} \"$@\"\n",
                    harness.tmux.display()
                ),
            )
            .expect("write upstream stand-in");
            fs::set_permissions(&upstream, fs::Permissions::from_mode(0o755)).expect("chmod");

            let refused = harness
                .command()
                .env("LATCH_TMUX_BIN", &upstream)
                .args(["attach", &id])
                .output()
                .expect("run attach with an upstream kernel");
            assert!(!refused.status.success());
            let diagnostic = String::from_utf8_lossy(&refused.stderr).into_owned();
            assert!(
                diagnostic.contains("complete Latch payload"),
                "an upstream kernel was not refused with an update instruction: {diagnostic}"
            );

            assert!(
                desk.child.try_wait().expect("wait").is_none(),
                "a refused attach evicted the healthy surface it never replaced"
            );
            assert!(harness.surface_attached(&id));

            desk.kill();
            harness.remove(&id);
        },
    );
}

#[test]
fn the_gateway_steals_the_desk_surface_and_the_desk_steals_it_back() {
    with_kernel(
        "the_gateway_steals_the_desk_surface_and_the_desk_steals_it_back",
        |harness| {
            let id = harness.create(BLOCKED_PROMPT);
            wait_until(
                || harness.visible(&id).contains("Do you trust"),
                "the pane never painted its prompt",
                Duration::from_secs(10),
            );
            let gateway = start_gateway(harness);

            let mut desk = harness.attach(&id, 120, 40);
            assert!(desk.wait_for(b"Do you trust", Duration::from_secs(10)));

            // The phone connects and takes the surface. It must be painted the
            // prompt even though the pane has written nothing for a while.
            let mut phone = connect_terminal(&gateway, &id, 60, 20);
            let (saw, _) = read_until(&mut phone, b"Do you trust", Duration::from_secs(15));
            assert!(saw, "the gateway surface was not painted the current frame");
            assert_eq!(
                desk.release(Duration::from_secs(15)),
                Some(EXIT_STOLEN),
                "a websocket steal did not release the desk surface as `stolen`"
            );

            // The desk steals it back; the socket closes with the stolen code.
            let mut desk_again = harness.attach(&id, 120, 40);
            assert!(
                desk_again.wait_for(b"Do you trust", Duration::from_secs(15)),
                "stealing back from the gateway did not repaint the prompt"
            );
            let close = read_close(&mut phone, Duration::from_secs(15));
            assert_eq!(
                close
                    .as_ref()
                    .map(|(code, reason)| (*code, reason.as_str())),
                Some((WS_CLOSE_STOLEN, "stolen")),
                "the stolen websocket did not close with the stolen reason: {close:?}"
            );

            desk_again.kill();
            harness.remove(&id);
        },
    );
}

#[test]
fn a_non_reading_gateway_peer_is_evicted_while_the_pane_keeps_running() {
    with_kernel(
        "a_non_reading_gateway_peer_is_evicted_while_the_pane_keeps_running",
        |harness| {
            let counter = harness.temp.path().join("ticks");
            // A pane that both writes hard (to fill any queue) and records
            // progress on disk, so "did the pane keep going" is answerable
            // after the client is gone.
            let shell = format!(
                "stty raw -echo; i=0; while :; do i=$((i+1)); echo $i > {}; \
                 head -c 4096 /dev/zero | tr '\\0' 'x'; done",
                counter.display()
            );
            let id = harness.create(&shell);
            let gateway = start_gateway(harness);

            let mut peer = connect_terminal(&gateway, &id, 80, 24);
            let (saw, _) = read_until(&mut peer, b"xxxx", Duration::from_secs(15));
            assert!(saw, "the gateway peer never received pane output");

            // Stop reading entirely. The socket's receive buffer fills, the
            // gateway's bounded write deadline expires, and the peer is
            // evicted rather than the pane being blocked behind it.
            let close = read_close_after_stalling(&mut peer, Duration::from_secs(60));
            assert!(
                matches!(close, Some((WS_CLOSE_SLOW_CLIENT, _)) | None),
                "a non-reading peer was not evicted: {close:?}"
            );

            let before = fs::read_to_string(&counter).unwrap_or_default();
            wait_until(
                || fs::read_to_string(&counter).unwrap_or_default() != before,
                "the pane stopped making progress after its client was evicted",
                Duration::from_secs(20),
            );

            // No attach process may survive its socket.
            wait_until(
                || !harness.surface_attached(&id),
                "an evicted peer left a ghost surface attached",
                Duration::from_secs(20),
            );
            drop(gateway);
            harness.remove(&id);
        },
    );
}

#[test]
fn a_dropped_gateway_peer_leaves_no_surface_and_the_session_survives() {
    with_kernel(
        "a_dropped_gateway_peer_leaves_no_surface_and_the_session_survives",
        |harness| {
            let id = harness.create(BLOCKED_PROMPT);
            wait_until(
                || harness.visible(&id).contains("Do you trust"),
                "the pane never painted its prompt",
                Duration::from_secs(10),
            );
            let gateway = start_gateway(harness);

            let mut peer = connect_terminal(&gateway, &id, 80, 24);
            let (saw, _) = read_until(&mut peer, b"Do you trust", Duration::from_secs(15));
            assert!(saw);
            wait_until(
                || harness.surface_attached(&id),
                "the gateway peer never became the surface",
                Duration::from_secs(10),
            );

            // Drop the socket the way a phone losing signal does: no close
            // frame, just a vanished peer.
            drop(peer);
            wait_until(
                || !harness.surface_attached(&id),
                "a dropped peer left a ghost surface attached",
                Duration::from_secs(20),
            );

            // The session is still there, still headless, still showing its
            // prompt, and a desk terminal can take it.
            assert!(harness.visible(&id).contains("Do you trust"));
            let mut desk = harness.attach(&id, 100, 30);
            assert!(
                desk.wait_for(b"Do you trust", Duration::from_secs(15)),
                "the session was not reattachable after its peer vanished"
            );

            desk.kill();
            drop(gateway);
            harness.remove(&id);
        },
    );
}

/// Stops reading the socket and waits for the gateway to give up on it.
///
/// `tungstenite` reads eagerly, so "stop reading" has to mean stop calling
/// `read` at all for a while, then drain whatever the close handshake left.
fn read_close_after_stalling(socket: &mut Ws, within: Duration) -> Option<(u16, String)> {
    thread::sleep(Duration::from_secs(5));
    read_close(socket, within)
}

#[test]
fn a_terminal_query_is_answered_once_headless_and_once_by_the_live_terminal() {
    with_kernel(
        "a_terminal_query_is_answered_once_headless_and_once_by_the_live_terminal",
        |harness| {
            // The pane issues a Device Attributes query twice: once with no
            // surface, once with a live one. It reports each reply as hex,
            // because printing the reply itself would feed an escape sequence
            // straight back into the terminal that is meant to display it.
            //
            // `min 0 time 5` makes every read return after half a second with
            // whatever arrived. A fixed byte count would hang the moment a
            // reply were one byte shorter than expected, and would turn "no
            // reply at all" — the failure this test exists to catch — into a
            // timeout rather than an assertion.
            let shell = "stty raw -echo min 0 time 5; \
                 printf 'ARMED\\r\\n'; \
                 printf '\\033[c'; \
                 one=$(dd bs=64 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'); \
                 printf 'ONE[%s]\\r\\n' \"$one\"; \
                 while :; do \
                   gate=$(dd bs=64 count=1 2>/dev/null); \
                   [ -n \"$gate\" ] && break; \
                 done; \
                 printf '\\033[c'; \
                 two=$(dd bs=64 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'); \
                 printf 'TWO[%s]\\r\\n' \"$two\"; \
                 while :; do sleep 3600; done";
            let id = harness.create(shell);

            // Headless: tmux is the virtual terminal, so it answers on the
            // pane's behalf — exactly once.
            wait_until(
                || harness.visible(&id).contains("ONE["),
                "a headless query went unanswered: tmux is not the query owner",
                Duration::from_secs(15),
            );
            let headless = harness.visible(&id);
            let one = between(&headless, "ONE[", "]").expect("headless reply");
            assert!(
                one.starts_with("1b5b3f"),
                "the headless reply was not a device-attributes response: {one:?}"
            );
            assert_eq!(
                headless.matches("ONE[").count(),
                1,
                "a headless query was answered more than once: {headless}"
            );

            // Live: the real terminal owns the reply. tmux must forward the
            // query to the surface and suppress its own answer.
            let mut surface = harness.attach(&id, 100, 30);
            assert!(surface.wait_for(b"ARMED", Duration::from_secs(15)));
            let before = surface.pump().len();
            surface.type_bytes(b"g");
            assert!(
                surface.wait_for(b"\x1b[c", Duration::from_secs(15)),
                "a live query was not forwarded to the real terminal"
            );
            let forwarded = surface.pump()[before..].to_vec();
            assert_eq!(
                forwarded
                    .windows(3)
                    .filter(|window| *window == b"\x1b[c")
                    .count(),
                1,
                "the query reached the live terminal more than once"
            );

            // Answer the way a terminal would. The pane must receive exactly
            // these bytes: our answer, and not also tmux's.
            surface.type_bytes(b"\x1b[?62;c");
            wait_until(
                || harness.visible(&id).contains("TWO["),
                "the terminal's answer never reached the pane",
                Duration::from_secs(15),
            );
            let live = harness.visible(&id);
            let two = between(&live, "TWO[", "]").expect("live reply");
            assert_eq!(
                two, "1b5b3f36323b63",
                "the pane did not receive exactly the real terminal's one answer \
                 (got {two:?}); tmux answered too, or answered instead"
            );
            assert_eq!(
                live.matches("ONE[").count() + live.matches("TWO[").count(),
                2,
                "the pane saw more replies than queries: {live}"
            );

            surface.kill();
            harness.remove(&id);
        },
    );
}

/// The text between the first `open` and the next `close` after it.
fn between<'a>(haystack: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = haystack.find(open)? + open.len();
    let rest = &haystack[start..];
    Some(&rest[..rest.find(close)?])
}

#[test]
fn local_input_latency_stays_close_to_a_direct_pty_baseline() {
    with_kernel(
        "local_input_latency_stays_close_to_a_direct_pty_baseline",
        |harness| {
            const ROUNDS: usize = 200;

            // Baseline: the same echo loop on a bare PTY, with no Latch and no
            // kernel in the path. Measuring both on this machine, in this run,
            // is the only comparison that means anything.
            let baseline = {
                let (master, slave) = open_pty(100, 30);
                let mut child = unsafe {
                    Command::new("/bin/sh")
                        .args([
                            "-c",
                            "stty raw -echo; while read -r _l; do printf 'P\\r\\n'; done",
                        ])
                        .stdin(stdio_dup(slave))
                        .stdout(stdio_dup(slave))
                        .stderr(stdio_dup(slave))
                        .pre_exec(|| {
                            if libc::setsid() < 0 {
                                return Err(std::io::Error::last_os_error());
                            }
                            Ok(())
                        })
                        .spawn()
                        .expect("spawn baseline shell")
                };
                unsafe { libc::close(slave) };
                let mut file = unsafe { std::fs::File::from_raw_fd(master) };
                let samples = round_trip(&mut file, ROUNDS);
                let _ = child.kill();
                let _ = child.wait();
                samples
            };

            let id = harness.create(
                "stty raw -echo; printf 'ready\\r\\n'; while read -r _l; do printf 'P\\r\\n'; done",
            );
            wait_until(
                || harness.visible(&id).contains("ready"),
                "the pane never became ready",
                Duration::from_secs(10),
            );
            let mut surface = harness.attach(&id, 100, 30);
            assert!(surface.wait_for(b"ready", Duration::from_secs(10)));
            set_blocking(&surface.master);
            let latched = round_trip(&mut surface.master, ROUNDS);
            set_nonblocking(&surface.master);

            let baseline_p95 = percentile(&baseline, 0.95);
            let latched_p95 = percentile(&latched, 0.95);
            eprintln!(
                "measure: input_latency_p95 baseline={baseline_p95:?} latched={latched_p95:?} \
                 overhead={:?}",
                latched_p95.saturating_sub(baseline_p95)
            );
            assert!(
                latched_p95 <= baseline_p95 + Duration::from_millis(2),
                "local echo p95 was {latched_p95:?} against a {baseline_p95:?} direct-PTY \
                 baseline, more than the 2ms the design allows"
            );

            surface.kill();
            harness.remove(&id);
        },
    );
}

/// Types a newline and waits for the echo, `rounds` times, returning each
/// round trip. Both sides of the comparison use this same loop.
fn round_trip(file: &mut std::fs::File, rounds: usize) -> Vec<Duration> {
    let mut samples = Vec::with_capacity(rounds);
    let mut scratch = [0u8; 4096];
    for _ in 0..rounds {
        let sent = Instant::now();
        if file.write_all(b"\n").and_then(|()| file.flush()).is_err() {
            break;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match file.read(&mut scratch) {
                Ok(read) if find(&scratch[..read], b"P").is_some() => {
                    samples.push(sent.elapsed());
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
            if Instant::now() >= deadline {
                break;
            }
        }
    }
    samples
}

fn percentile(samples: &[Duration], quantile: f64) -> Duration {
    assert!(!samples.is_empty(), "no latency samples were collected");
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() as f64 - 1.0) * quantile).round() as usize;
    sorted[index]
}

#[test]
fn a_hard_writing_pane_keeps_the_kernel_bounded_and_the_session_progressing() {
    with_kernel(
        "a_hard_writing_pane_keeps_the_kernel_bounded_and_the_session_progressing",
        |harness| {
            let counter = harness.temp.path().join("ticks");
            let shell = format!(
                "stty raw -echo; i=0; while :; do i=$((i+1)); echo $i > {}; \
                 head -c 65536 /dev/zero | tr '\\0' 'y'; done",
                counter.display()
            );
            let id = harness.create(&shell);

            // A surface that reads nothing at all. The kernel's queue bound is
            // what stands between this and unbounded memory growth.
            let mut stalled = harness.attach(&id, 100, 30);
            // Let the pane's grid and scrollback reach their limits before
            // sampling. A sample taken right after attach measures warm-up,
            // and on a loaded machine that warm-up is still in progress —
            // which reads as drift that is not there.
            let warm = settled_rss(harness);
            thread::sleep(Duration::from_secs(15));
            let after = kernel_rss(harness);
            eprintln!(
                "measure: kernel_rss warm={warm}KiB after_stall={after}KiB \
                 growth={}KiB",
                after.saturating_sub(warm)
            );

            // The bound is 1 MiB of queued output. Allowing 8 MiB of growth
            // leaves generous room for allocator behaviour while still
            // failing loudly if the queue is actually unbounded: 15 seconds of
            // this pane is hundreds of megabytes.
            assert!(
                after.saturating_sub(warm) < 8 * 1024,
                "kernel RSS grew {}KiB while a client read nothing; the output queue is \
                 not bounded",
                after.saturating_sub(warm)
            );

            assert_eq!(
                stalled.release(Duration::from_secs(20)),
                Some(EXIT_SLOW_CLIENT),
                "a client that read nothing was not evicted with `slow_client`"
            );

            // The point of eviction is that the session outlives it. Both
            // halves matter and both have failed here before: the pane must
            // still be running, and the kernel must still be answering — a
            // server stuck writing to a full terminal does neither.
            let before = fs::read_to_string(&counter).unwrap_or_default();
            wait_until(
                || fs::read_to_string(&counter).unwrap_or_default() != before,
                "the pane stopped making progress behind a client that would not read",
                Duration::from_secs(20),
            );
            assert!(
                !harness.surface_attached(&id),
                "the evicted client is still counted as the session's surface"
            );

            let mut next = harness.attach(&id, 100, 30);
            assert!(
                next.wait_for(b"y", Duration::from_secs(10)),
                "the next attach after slow-client eviction did not get a current frame"
            );
            let after_attach = fs::read_to_string(&counter).unwrap_or_default();
            wait_until(
                || {
                    next.pump();
                    fs::read_to_string(&counter).unwrap_or_default() != after_attach
                },
                "the pane stalled after the next attach took over",
                Duration::from_secs(20),
            );

            next.kill();
            stalled.kill();
            harness.remove(&id);
        },
    );
}

/// Kernel RSS once it has stopped climbing, so a later sample measures drift
/// rather than the tail of warm-up. Gives up and returns the last reading if it
/// never settles, which the caller's bound then judges on its own.
fn settled_rss(harness: &Harness) -> u64 {
    let mut previous = kernel_rss(harness);
    for _ in 0..20 {
        thread::sleep(Duration::from_secs(1));
        let current = kernel_rss(harness);
        if current <= previous + 64 {
            return current.max(previous);
        }
        previous = current;
    }
    previous
}

/// Resident size of the session kernel serving `harness`, in KiB.
fn kernel_rss(harness: &Harness) -> u64 {
    let output = Command::new("/bin/ps")
        .args(["-Ao", "rss=,command="])
        .output()
        .expect("ps");
    let socket = harness.home.join("server");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(socket.to_str().unwrap()) && line.contains("server"))
        .filter_map(|line| line.split_whitespace().next()?.parse::<u64>().ok())
        .max()
        .unwrap_or(0)
}

#[test]
fn a_running_upstream_server_is_refused_with_a_restart_instruction() {
    with_kernel(
        "a_running_upstream_server_is_refused_with_a_restart_instruction",
        |harness| {
            // Installing the payload does not restart a tmux server that is
            // already up, so a patched binary says nothing about the server on
            // the other end of the socket. An upstream server accepts an
            // ordinary attach and silently ignores the raw-attach identify
            // flag, which hands the user tmux's own renderer with no steal and
            // no warning.
            let id = harness.create(BLOCKED_PROMPT);
            wait_until(
                || harness.visible(&id).contains("Do you trust"),
                "the pane never painted its prompt",
                Duration::from_secs(10),
            );

            // The control: a patched server names itself to any client that
            // asks, which is what makes the check above possible at all.
            let answer = Command::new(&harness.tmux)
                .args([
                    "-S",
                    harness.home.join("server").to_str().unwrap(),
                    "display-message",
                    "-p",
                    "#{latch_raw_kernel}",
                ])
                .output()
                .expect("probe the running server");
            assert_eq!(
                String::from_utf8_lossy(&answer.stdout).trim(),
                "latch-raw-attach-v1",
                "the patched kernel does not identify itself to a client"
            );

            // Upstream tmux resolves that unknown format to nothing. Stand in
            // for one by answering the probe the way it would and delegating
            // everything else, so the session stays real and only the server's
            // identity is in question.
            let stock = harness.temp.path().join("stock-tmux");
            let script = format!(
                "#!/bin/sh\n\
                 for argument in \"$@\"; do\n\
                 \x20 if [ \"$argument\" = '#{{latch_raw_kernel}}' ]; then\n\
                 \x20   echo\n\
                 \x20   exit 0\n\
                 \x20 fi\n\
                 done\n\
                 exec {} \"$@\"\n",
                harness.tmux.display()
            );
            fs::write(&stock, script).expect("write upstream-server stand-in");
            fs::set_permissions(&stock, fs::Permissions::from_mode(0o755)).expect("chmod");

            let refused = harness
                .command()
                .env("LATCH_TMUX_BIN", &stock)
                .args(["attach", &id])
                .output()
                .expect("attach against an upstream server");
            assert!(!refused.status.success());
            let diagnostic = String::from_utf8_lossy(&refused.stderr).into_owned();
            assert!(
                diagnostic.contains("predates this release"),
                "an upstream server was not refused with a restart instruction: {diagnostic}"
            );
            assert!(
                harness.visible(&id).contains("Do you trust"),
                "a refused attach disturbed the session it never took"
            );

            harness.remove(&id);
        },
    );
}
