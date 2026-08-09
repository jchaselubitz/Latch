//! Shared machinery for the worker suite.
//!
//! Two kinds of test live in `crates/latch/tests/`, and this module serves both.
//!
//! The state machine — registry, resize authority, bounded queues — is exercised
//! directly, because "a steal raced a detach" is a statement about ordering and
//! deserves to be asserted as one rather than inferred from bytes that happened
//! to arrive helpfully.
//!
//! Everything about *processes* is exercised against the real binary. Detachment,
//! signals, socket lifetime, and hostile input are properties of a running
//! program; a test that stubs the process out would pass on a build that could
//! not survive its terminal closing.
//!
//! The client here is deliberately hand-rolled over
//! [`latch_protocol::frame`] rather than borrowing whatever the worker uses
//! internally. If the worker's own framing helper is wrong, a test built on it
//! agrees with the bug.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Once;
use std::time::{Duration, Instant};

use latch_protocol::control::{self, AttachMode, ClientInfo, ControlMessage, SessionState, Size};
use latch_protocol::frame::{self, FrameDecoder, FrameType};
use latch_protocol::PROTOCOL_VERSION;

use latch::worker::manifest::{LaunchManifest, TerminalSize, WorkerTuning};
use latch::worker::paths::{LatchHome, SessionId, SessionPaths};
use latch::worker::socket::OwnedFrame;
use latch::worker::spawn::{self, SpawnRequest, SpawnedWorker};

/// A generous but bounded wait for something the worker must do promptly.
///
/// Long enough that a loaded machine does not produce a false failure, short
/// enough that a genuine hang is reported as one rather than as a test that
/// never finishes.
pub const SETTLE: Duration = Duration::from_secs(5);

/// A short wait, for asserting that something does *not* happen.
pub const BRIEF: Duration = Duration::from_millis(300);

/// Clears the process umask so mode assertions measure what the code set.
///
/// Modes are asserted rather than trusted (`docs/ARCHITECTURE_RULES.md`), and a
/// test that only passes under the developer's umask asserts nothing. Set once
/// per process and always to the same value, so tests running in parallel
/// threads cannot disagree about it.
pub fn relax_umask() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: `umask` is a process-global setter with no preconditions.
        unsafe { libc::umask(0) };
    });
}

/// The shortest temporary base directory this platform offers.
///
/// See [`Harness::new`] for why a few dozen bytes of path decide whether the
/// suite runs at all.
fn short_temp_base() -> PathBuf {
    let short = PathBuf::from("/tmp");
    if short.is_dir() {
        short
    } else {
        std::env::temp_dir()
    }
}

/// The permission bits of a path.
pub fn mode_of(path: &Path) -> u32 {
    std::fs::symlink_metadata(path)
        .unwrap_or_else(|err| panic!("cannot stat {}: {err}", path.display()))
        .permissions()
        .mode()
        & 0o777
}

/// Whether a process exists and is signallable by this user.
pub fn is_alive(pid: i32) -> bool {
    // SAFETY: `kill` with signal 0 performs the permission and existence check
    // without delivering anything.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Sends a signal to one process.
pub fn signal(pid: i32, sig: i32) {
    // SAFETY: `kill` is safe to call with any pid; failure is reported, not UB.
    let rc = unsafe { libc::kill(pid, sig) };
    assert_eq!(rc, 0, "cannot signal pid {pid} with {sig}");
}

/// Sends a signal to a whole process group.
pub fn signal_group(pgid: i32, sig: i32) {
    // SAFETY: as `signal`; a negative pid addresses a process group.
    let rc = unsafe { libc::kill(-pgid, sig) };
    assert_eq!(rc, 0, "cannot signal process group {pgid} with {sig}");
}

/// The process group a process belongs to.
pub fn group_of(pid: i32) -> i32 {
    // SAFETY: `getpgid` reads kernel state and returns -1 on failure.
    let pgid = unsafe { libc::getpgid(pid) };
    assert!(pgid > 0, "cannot read the process group of pid {pid}");
    pgid
}

/// The session a process belongs to.
pub fn session_of(pid: i32) -> i32 {
    // SAFETY: `getsid` reads kernel state and returns -1 on failure.
    let sid = unsafe { libc::getsid(pid) };
    assert!(sid > 0, "cannot read the session of pid {pid}");
    sid
}

/// This process's effective uid.
pub fn current_uid() -> u32 {
    // SAFETY: `geteuid` cannot fail.
    unsafe { libc::geteuid() }
}

/// Polls `condition` until it holds or `timeout` elapses.
pub fn wait_for(what: &str, timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out after {timeout:?} waiting for {what}");
}

/// Polls until `condition` holds, returning whether it ever did.
pub fn settled(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

/// The `latch` binary this test run built.
pub fn worker_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_latch"))
}

/// A throwaway `~/.latch`.
///
/// Every test gets its own root, so nothing depends on the order tests run in
/// and nothing touches the developer's real sessions.
pub struct Harness {
    tempdir: tempfile::TempDir,
    home: LatchHome,
    next_id: AtomicU32,
}

impl Harness {
    /// Creates a root under a temporary directory.
    ///
    /// The root is kept deliberately short. A session's control socket lives at
    /// `<root>/latch/sessions/<id>/control.sock`, and `sun_path` holds 104 bytes
    /// on Darwin against 108 on Linux. macOS's default `TMPDIR` is a ~49-byte
    /// path under `/var/folders`, which puts that socket at exactly 104 — one
    /// byte over, so every worker in the suite fails to bind. `/tmp` is four
    /// bytes on both platforms and leaves the limit irrelevant.
    pub fn new() -> Self {
        relax_umask();
        let tempdir = tempfile::Builder::new()
            .prefix("latch")
            .tempdir_in(short_temp_base())
            .expect("cannot create a temporary Latch root");
        let home = LatchHome::new(tempdir.path().join("latch"));
        Self {
            tempdir,
            home,
            next_id: AtomicU32::new(1),
        }
    }

    /// The root.
    pub fn home(&self) -> &LatchHome {
        &self.home
    }

    /// The temporary directory containing the root.
    pub fn tempdir(&self) -> &Path {
        self.tempdir.path()
    }

    /// A fresh session identifier, deterministic within one harness.
    ///
    /// Deterministic rather than generated so a failure names the same session
    /// on a re-run, and so the suite does not depend on id generation — which is
    /// the worker's business, not the test's.
    pub fn next_session_id(&self) -> SessionId {
        let n = self.next_id.fetch_add(1, Ordering::Relaxed);
        SessionId::parse(&format!("ses_test{n:08}")).expect("test ids are well formed")
    }

    /// Starts a session and waits for its socket to answer.
    pub fn spawn(&self, manifest: LaunchManifest) -> Session {
        let id = self.next_session_id();
        let paths = self.home.session(&id);
        let spawned = spawn::spawn_detached(SpawnRequest::new(
            paths.clone(),
            id,
            worker_binary(),
            manifest,
        ))
        .expect("the worker should start");
        Session { spawned }
    }

    /// Starts a session running a shell command.
    pub fn spawn_sh(&self, script: &str) -> Session {
        self.spawn(sh_manifest(script, TerminalSize::new(200, 50)))
    }
}

impl Default for Harness {
    fn default() -> Self {
        Self::new()
    }
}

/// A manifest that runs `script` under `/bin/sh`.
///
/// `sh` rather than a purpose-built helper because a session is a shell in the
/// ordinary case, and because a test child that can be described in one line of
/// shell is a test whose intent is readable from the assertion.
pub fn sh_manifest(script: &str, size: TerminalSize) -> LaunchManifest {
    LaunchManifest::new(
        vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
        std::env::temp_dir(),
        size,
    )
}

/// A manifest with adjusted worker bounds.
pub fn tuned(mut manifest: LaunchManifest, tuning: WorkerTuning) -> LaunchManifest {
    manifest.tuning = tuning;
    manifest
}

/// A live session under test.
pub struct Session {
    spawned: SpawnedWorker,
}

impl Session {
    /// Where the session's files are.
    pub fn paths(&self) -> &SessionPaths {
        &self.spawned.paths
    }

    /// The session's identifier.
    pub fn id(&self) -> &SessionId {
        &self.spawned.id
    }

    /// Opens a connection to the control socket without attaching.
    pub fn connect(&self) -> std::io::Result<Client> {
        Client::connect(self.paths())
    }

    /// Opens a connection and attaches, expecting acceptance.
    pub fn attach(&self, mode: AttachMode, steal: bool, size: Size) -> Client {
        let mut client = self.connect().expect("the control socket should accept");
        let reply = client.attach(mode, steal, size);
        assert!(
            matches!(reply, ControlMessage::Attached { .. }),
            "expected `attached`, got {reply:?}"
        );
        client
    }

    /// Whether the socket still answers.
    pub fn is_live(&self) -> bool {
        UnixStream::connect(self.paths().socket()).is_ok()
    }

    /// Asks the worker to stop, the way `latch stop` does.
    ///
    /// `session.update { state: stopping }` and nothing else. There is no ninth
    /// control message for stopping and no PID anywhere in this path.
    pub fn request_stop(&self) {
        let mut client = self.attach(AttachMode::Watch, false, Size { cols: 80, rows: 24 });
        client
            .send_control(&ControlMessage::SessionUpdate {
                state: Some(SessionState::Stopping),
                attachments: None,
                title: None,
            })
            .expect("the stop request should be delivered");
        // Held open until the worker acts on it: a client that closes
        // immediately would make this a test of stop-racing-a-detach instead.
        std::thread::sleep(Duration::from_millis(50));
        drop(client);
    }

    /// Waits until the socket is gone.
    pub fn wait_until_gone(&self, timeout: Duration) {
        wait_for("the session socket to disappear", timeout, || {
            !self.is_live()
        });
    }
}

impl Drop for Session {
    /// Best-effort cleanup, so a suite run does not leave workers behind.
    ///
    /// Deliberately not asserted on: a test that already killed the session,
    /// or that is failing for its own reasons, must not have a second failure
    /// reported from here.
    fn drop(&mut self) {
        if !self.is_live() {
            return;
        }
        let Ok(mut client) = self.connect() else {
            return;
        };
        let _ = client.send_control(&ControlMessage::Attach {
            protocol: PROTOCOL_VERSION,
            mode: AttachMode::Watch,
            steal: false,
            client: ClientInfo {
                kind: "cli".to_owned(),
                name: "test-cleanup".to_owned(),
            },
            size: Size { cols: 80, rows: 24 },
        });
        let _ = client.send_control(&ControlMessage::SessionUpdate {
            state: Some(SessionState::Stopping),
            attachments: None,
            title: None,
        });
        let _ = settled(SETTLE, || !self.is_live());
    }
}

/// A test client speaking the wire protocol directly.
pub struct Client {
    stream: UnixStream,
    decoder: FrameDecoder,
    pending: VecDeque<OwnedFrame>,
}

impl Client {
    /// Connects to a session's control socket.
    pub fn connect(paths: &SessionPaths) -> std::io::Result<Self> {
        let stream = UnixStream::connect(paths.socket())?;
        stream.set_read_timeout(Some(Duration::from_millis(50)))?;
        Ok(Self {
            stream,
            decoder: FrameDecoder::new(),
            pending: VecDeque::new(),
        })
    }

    /// The underlying socket, for tests that need to abuse it.
    pub fn stream(&self) -> &UnixStream {
        &self.stream
    }

    /// Writes raw bytes, bypassing framing entirely.
    pub fn send_raw(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(bytes)?;
        self.stream.flush()
    }

    /// Writes one frame.
    pub fn send_frame(&mut self, frame_type: FrameType, payload: &[u8]) -> std::io::Result<()> {
        let encoded = frame::encode(frame_type, payload).expect("test frames are within bounds");
        self.send_raw(&encoded)
    }

    /// Writes one control message.
    pub fn send_control(&mut self, message: &ControlMessage) -> std::io::Result<()> {
        self.send_frame(FrameType::Control, &control::encode(message))
    }

    /// Writes terminal input.
    pub fn send_input(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.send_frame(FrameType::TerminalInput, bytes)
    }

    /// Sends `attach` and returns the worker's reply.
    ///
    /// The reply is returned rather than asserted so the refusal cases —
    /// `control_busy`, an unsupported version — are expressed by the test that
    /// cares about them.
    pub fn attach(&mut self, mode: AttachMode, steal: bool, size: Size) -> ControlMessage {
        self.attach_with_protocol(PROTOCOL_VERSION, mode, steal, size)
    }

    /// Sends `attach` naming a specific protocol version.
    pub fn attach_with_protocol(
        &mut self,
        protocol: u32,
        mode: AttachMode,
        steal: bool,
        size: Size,
    ) -> ControlMessage {
        self.send_control(&ControlMessage::Attach {
            protocol,
            mode,
            steal,
            client: ClientInfo {
                kind: "cli".to_owned(),
                name: "test".to_owned(),
            },
            size,
        })
        .expect("the attach should be delivered");
        self.next_control(SETTLE)
            .expect("the worker should answer an attach")
    }

    /// The next frame of any type, or `None` if none arrives in time.
    pub fn next_frame(&mut self, timeout: Duration) -> Option<OwnedFrame> {
        if let Some(frame) = self.pending.pop_front() {
            return Some(frame);
        }
        let deadline = Instant::now() + timeout;
        let mut buf = [0u8; 8192];
        loop {
            match self.decoder.next_frame() {
                Ok(Some(frame)) => {
                    return Some(OwnedFrame {
                        frame_type: frame.frame_type,
                        payload: frame.payload.to_vec(),
                    })
                }
                Ok(None) => {}
                Err(err) => panic!("the worker sent an undecodable frame: {err}"),
            }
            if Instant::now() >= deadline {
                return None;
            }
            match self.stream.read(&mut buf) {
                Ok(0) => return None,
                Ok(n) => self.decoder.feed(&buf[..n]),
                Err(err)
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::TimedOut =>
                {
                    continue
                }
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return None,
            }
        }
    }

    /// The next control message, discarding terminal frames.
    pub fn next_control(&mut self, timeout: Duration) -> Option<ControlMessage> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let frame = self.next_frame(remaining.max(Duration::from_millis(1)))?;
            if frame.frame_type == FrameType::Control {
                return Some(
                    control::decode(&frame.payload).expect("the worker sends decodable control"),
                );
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
    }

    /// Terminal output accumulated over `window`, ignoring control frames.
    pub fn output_for(&mut self, window: Duration) -> Vec<u8> {
        let deadline = Instant::now() + window;
        let mut out = Vec::new();
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.next_frame(remaining) {
                Some(frame) if frame.frame_type == FrameType::TerminalOutput => {
                    out.extend_from_slice(&frame.payload)
                }
                Some(_) => continue,
                None => break,
            }
        }
        out
    }

    /// Reads output until it contains `needle`, or fails.
    pub fn wait_for_output(&mut self, needle: &str, timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        while Instant::now() < deadline {
            if String::from_utf8_lossy(&out).contains(needle) {
                return out;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.next_frame(remaining) {
                Some(frame) if frame.frame_type == FrameType::TerminalOutput => {
                    out.extend_from_slice(&frame.payload)
                }
                Some(_) => continue,
                None => break,
            }
        }
        if String::from_utf8_lossy(&out).contains(needle) {
            return out;
        }
        panic!(
            "timed out waiting for {needle:?} in session output; saw {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    /// Closes the connection abruptly, without a protocol goodbye.
    ///
    /// There is no goodbye message to send: control is released by socket close,
    /// and every client departure in production looks exactly like this one.
    pub fn abandon(self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

/// The longest unbroken run of `needle` in some received bytes.
///
/// A serialized screen writes each row's cells contiguously, so the widest run
/// of a repeated character is the width the screen was serialized at. That
/// makes it the one measurement that distinguishes a snapshot taken at the
/// recipient's geometry from one taken at somebody else's.
pub fn longest_run(bytes: &[u8], needle: char) -> u32 {
    let mut longest = 0;
    let mut current = 0;
    for character in String::from_utf8_lossy(bytes).chars() {
        if character == needle {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

/// A wire size, spelled short because the resize tests are full of them.
pub fn size(cols: u16, rows: u16) -> Size {
    Size { cols, rows }
}

/// The history preamble of a snapshot: everything between the hard reset that
/// opens it and the alternate-screen statement that opens the screen.
///
/// The two are separable because no cell can contain an escape, so nothing the
/// history block prints can imitate the marker. Empty means the snapshot
/// carried no history — which is what a resync must always look like.
pub fn attach_history(payload: &[u8]) -> &[u8] {
    const RESET: &[u8] = b"\x1bc";
    const MARKER: &[u8] = b"\x1b[?1049";
    let Some(body) = payload.strip_prefix(RESET) else {
        return &[];
    };
    match body
        .windows(MARKER.len())
        .position(|window| window == MARKER)
    {
        Some(at) => &body[..at],
        None => &[],
    }
}

/// An `attach` as the registry receives it, after decoding and sanitizing.
pub fn attach_request(
    mode: AttachMode,
    steal: bool,
    name: &str,
    size: Size,
) -> latch::worker::registry::AttachRequest {
    latch::worker::registry::AttachRequest {
        protocol: PROTOCOL_VERSION,
        mode,
        steal,
        client: ClientInfo {
            kind: "cli".to_owned(),
            name: name.to_owned(),
        },
        size,
    }
}

/// Every message an effect list broadcasts to all attachments, in order.
pub fn broadcasts(effects: &[latch::worker::registry::Effect]) -> Vec<&ControlMessage> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            latch::worker::registry::Effect::Broadcast { message } => Some(message),
            _ => None,
        })
        .collect()
}

/// Every message an effect list sends to one attachment, in order.
pub fn sent_to<'a>(
    effects: &'a [latch::worker::registry::Effect],
    to: &latch::worker::registry::AttachmentId,
) -> Vec<&'a ControlMessage> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            latch::worker::registry::Effect::Send { to: id, message } if id == to => Some(message),
            _ => None,
        })
        .collect()
}

/// Every size an effect list asks the PTY to take, in order.
pub fn resizes(effects: &[latch::worker::registry::Effect]) -> Vec<Size> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            latch::worker::registry::Effect::Resize { size } => Some(*size),
            _ => None,
        })
        .collect()
}

/// Whether an effect list asks for a snapshot to be sent to `to`.
pub fn snapshots_to(
    effects: &[latch::worker::registry::Effect],
    to: &latch::worker::registry::AttachmentId,
) -> usize {
    effects
        .iter()
        .filter(|effect| {
            matches!(effect, latch::worker::registry::Effect::Snapshot { to: id } if id == to)
        })
        .count()
}

/// The first run of decimal digits in some output, as a number.
///
/// Not for output read from an attachment. Every attach is answered with the
/// serialized screen, and its leading mode resets carry digits of their own —
/// `\x1b[?1049l` parses as 1049 — so the first number in an attached client's
/// stream belongs to the terminal, not to the child. Use [`last_marker`] with a
/// prefix the child printed.
pub fn first_number(bytes: &[u8]) -> Option<i64> {
    let text = String::from_utf8_lossy(bytes);
    let digits: String = text
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// The largest `PREFIX<number>` marker in some output.
pub fn last_marker(bytes: &[u8], prefix: &str) -> Option<i64> {
    let text = String::from_utf8_lossy(bytes);
    text.match_indices(prefix)
        .filter_map(|(at, _)| {
            let rest = &text[at + prefix.len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<i64>().ok()
        })
        .max()
}

/// A worker started through a process the test can kill or stop.
///
/// The launcher is a shell that starts the worker and then goes on living, which
/// is the shape of the real thing: `latch` in a terminal window starts a session
/// and stays around to drive the keyboard. What the tests do to this process —
/// `SIGKILL` for "the window was closed", `SIGSTOP` for "the client hung" — must
/// not reach the session's child.
pub struct Launcher {
    child: Child,
    pgid: i32,
    paths: SessionPaths,
    manifest_file: PathBuf,
    _tempdir: Option<tempfile::TempDir>,
}

impl Launcher {
    /// The launcher's process id.
    pub fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    /// The launcher's process group, which it leads.
    pub fn group(&self) -> i32 {
        self.pgid
    }

    /// The session it started.
    pub fn paths(&self) -> &SessionPaths {
        &self.paths
    }

    /// Whether the launcher is still running.
    pub fn is_alive(&self) -> bool {
        is_alive(self.pid())
    }

    /// Kills the launcher's entire process group.
    pub fn kill_group(&mut self) {
        signal_group(self.pgid, libc::SIGKILL);
        let _ = self.child.wait();
    }

    /// Stops the launcher without killing it, as a hung client would appear.
    pub fn stop_group(&self) {
        signal_group(self.pgid, libc::SIGSTOP);
    }

    /// Resumes a stopped launcher.
    pub fn continue_group(&self) {
        signal_group(self.pgid, libc::SIGCONT);
    }
}

impl Drop for Launcher {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.manifest_file);
        if self.is_alive() {
            let _ = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
        }
        let _ = self.child.wait();
    }
}

/// Starts a session through a shell that survives the worker's startup.
///
/// The shell is put in a process group of its own, so a test can address it and
/// everything it started without touching the test runner. If the worker did not
/// detach itself, it would be in that group too — which is precisely what these
/// tests check.
pub fn launch_via_shell(harness: &Harness, manifest: LaunchManifest) -> Launcher {
    let id = harness.next_session_id();
    let paths = harness.home().session(&id);
    paths
        .ensure()
        .expect("the session directory should be created");

    let manifest_file = harness.tempdir().join(format!("{id}.manifest.json"));
    let file = std::fs::File::create(&manifest_file).expect("cannot write the launch manifest");
    latch::worker::manifest::write(file, &manifest).expect("cannot write the launch manifest");

    let script = format!(
        "{bin} worker --session-dir {dir} < {manifest} & sleep 600",
        bin = shell_quote(&worker_binary()),
        dir = shell_quote(paths.dir()),
        manifest = shell_quote(&manifest_file),
    );

    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: `setsid` in the child between fork and exec is async-signal-safe
    // and touches nothing this process owns. It makes the launcher a session and
    // group leader so the test can signal it as a group.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().expect("cannot start the launcher shell");
    let pgid = child.id() as i32;

    spawn::wait_until_ready(&paths, SETTLE).expect("the launched worker should become ready");

    Launcher {
        child,
        pgid,
        paths,
        manifest_file,
        _tempdir: None,
    }
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// The entries of a session directory, sorted.
pub fn dir_entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("cannot list {}: {err}", dir.display()))
        .map(|entry| {
            entry
                .expect("readable entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// CLI helpers
//
// The CLI suite drives the real binary under a throwaway `LATCH_HOME`. Startup
// cost, nesting, and the `--json` contract are properties of the process a
// person runs, so the tests reach it the same way: argv in, stdout/stderr out.
// ---------------------------------------------------------------------------

/// Budget for `latch --version`. Every terminal window pays this; single-digit
/// milliseconds is the product requirement, and 50 ms leaves room for a loaded
/// CI host without accepting a Node-shaped hitch.
pub const VERSION_BUDGET: Duration = Duration::from_millis(50);

/// Budget for `latch list` against an empty home.
pub const EMPTY_LIST_BUDGET: Duration = Duration::from_millis(100);

/// Budget for `latch list` with thirty live sessions — the navigation surface
/// with every window a session.
pub const THIRTY_LIST_BUDGET: Duration = Duration::from_millis(1000);

/// Runs `latch` with `args` under `harness`'s home, capturing output.
pub fn latch_cmd(harness: &Harness, args: &[&str]) -> std::process::Output {
    Command::new(worker_binary())
        .args(args)
        .env(latch::worker::paths::HOME_ENV, harness.home().root())
        .env_remove(latch::cli::SESSION_ID_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("cannot spawn latch")
}

/// Runs `latch` with a stdin body (for `--manifest-file -`).
pub fn latch_cmd_with_stdin(
    harness: &Harness,
    args: &[&str],
    stdin: &[u8],
) -> std::process::Output {
    let mut child = Command::new(worker_binary())
        .args(args)
        .env(latch::worker::paths::HOME_ENV, harness.home().root())
        .env_remove(latch::cli::SESSION_ID_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("cannot spawn latch");
    {
        let mut handle = child.stdin.take().expect("stdin was piped");
        handle
            .write_all(stdin)
            .expect("cannot write the launch manifest to stdin");
    }
    child.wait_with_output().expect("cannot wait for latch")
}

/// Runs `latch` with an enclosing `LATCH_SESSION_ID`, as a nested invocation.
pub fn latch_cmd_nested(harness: &Harness, enclosing: &str, args: &[&str]) -> std::process::Output {
    Command::new(worker_binary())
        .args(args)
        .env(latch::worker::paths::HOME_ENV, harness.home().root())
        .env(latch::cli::SESSION_ID_ENV, enclosing)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("cannot spawn latch")
}

/// stdout as UTF-8, panicking with stderr on failure.
pub fn stdout_str(output: &std::process::Output) -> &str {
    std::str::from_utf8(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout was not utf-8; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// stderr as UTF-8.
pub fn stderr_str(output: &std::process::Output) -> &str {
    std::str::from_utf8(&output.stderr).unwrap_or("")
}

/// Parses stdout as JSON, panicking with context on failure.
pub fn stdout_json(output: &std::process::Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "latch exited {}; stderr={}",
        output.status,
        stderr_str(output)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout was not JSON ({err}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

// ---------------------------------------------------------------------------
// A phone-shaped client
//
// M2's client runs on the far end of a link that dies. Two pieces of machinery
// make that testable against the real binary rather than against a mock of it:
// a pty, because `latch attach` refuses to be interactive without one, and a
// cuttable proxy in front of the session socket, because a dropped SSH tunnel
// is a transport that vanishes while the worker keeps running. Killing the
// worker would prove something else entirely.
// ---------------------------------------------------------------------------

/// A `latch` process running on a pty, the way a person runs it.
///
/// The master end is both the screen (what the client painted) and the keyboard
/// (what it reads), so assertions here are about what somebody would actually
/// see on a phone.
pub struct PtyProcess {
    master: std::os::fd::OwnedFd,
    child: Child,
    transcript: Vec<u8>,
}

impl PtyProcess {
    /// Runs `latch` with `args` on a pty of the given size.
    pub fn spawn(harness: &Harness, args: &[&str], cols: u16, rows: u16) -> Self {
        use std::os::fd::{FromRawFd, OwnedFd};

        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        let mut winsize = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // Darwin declares the trailing `openpty` parameters `*mut` and Linux
        // `*const`; mutable pointers satisfy both. Mirrors `worker::process`.
        let termp = std::ptr::null_mut::<libc::termios>();
        // SAFETY: `openpty` initializes both descriptors and reads the winsize
        // it is given; the name argument is optional and omitted.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                termp,
                &mut winsize as *mut _,
            )
        };
        assert_eq!(opened, 0, "cannot allocate a pty for the client");
        // SAFETY: both descriptors were just returned by `openpty`.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };

        let mut command = Command::new(worker_binary());
        command
            .args(args)
            .env(latch::worker::paths::HOME_ENV, harness.home().root())
            .env_remove(latch::cli::SESSION_ID_ENV)
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(
                slave.try_clone().expect("cannot duplicate the pty slave"),
            ))
            .stdout(Stdio::from(
                slave.try_clone().expect("cannot duplicate the pty slave"),
            ))
            .stderr(Stdio::from(slave));
        // SAFETY: `setsid` and `TIOCSCTTY` between fork and exec are
        // async-signal-safe and touch nothing this process owns. They give the
        // client its own session with the pty as controlling terminal, so the
        // test runner's signals and job control never reach it.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("cannot spawn latch on a pty");

        Self {
            master,
            child,
            transcript: Vec::new(),
        }
    }

    /// Everything the client has painted so far.
    pub fn transcript(&self) -> String {
        String::from_utf8_lossy(&self.transcript).into_owned()
    }

    /// Sends keystrokes.
    pub fn type_in(&mut self, bytes: &[u8]) {
        use std::io::Write;
        let mut file = std::fs::File::from(
            self.master
                .try_clone()
                .expect("cannot duplicate the pty master"),
        );
        let _ = file.write_all(bytes);
        let _ = file.flush();
        std::mem::forget(file);
    }

    /// Changes the client's window size, as rotating a phone does.
    pub fn set_window(&self, cols: u16, rows: u16) {
        let winsize = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: the master descriptor is open and `winsize` is initialized.
        let rc = unsafe {
            libc::ioctl(
                std::os::fd::AsRawFd::as_raw_fd(&self.master),
                libc::TIOCSWINSZ,
                &winsize,
            )
        };
        assert_ne!(rc, -1, "cannot resize the client's pty");
        signal(self.child.id() as i32, libc::SIGWINCH);
    }

    /// Reads whatever the client paints, up to `window`.
    pub fn drain(&mut self, window: Duration) {
        let deadline = Instant::now() + window;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !self.read_once(remaining) {
                return;
            }
        }
    }

    /// Reads until the transcript contains `needle`, or fails.
    pub fn wait_for_screen(&mut self, needle: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        loop {
            if self.transcript().contains(needle) {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "timed out waiting for {needle:?} on the client's screen; saw {:?}",
                    self.transcript()
                );
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            self.read_once(remaining.min(Duration::from_millis(200)));
        }
    }

    /// Waits for the client to exit, draining its output meanwhile.
    ///
    /// Draining matters: a client blocked writing to a full pty would look
    /// exactly like one that failed to detect a dead transport.
    pub fn wait(&mut self, timeout: Duration) -> std::process::ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait().expect("cannot poll the client") {
                Some(status) => {
                    self.drain(Duration::from_millis(50));
                    return status;
                }
                None if Instant::now() >= deadline => panic!(
                    "the client did not exit within {timeout:?}; it is hanging rather than \
                     reporting a dead transport. Screen so far: {:?}",
                    self.transcript()
                ),
                None => {
                    self.read_once(Duration::from_millis(50));
                }
            }
        }
    }

    /// Whether the client is still running.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// One bounded read into the transcript; `false` once the pty is finished.
    fn read_once(&mut self, timeout: Duration) -> bool {
        use std::os::fd::AsRawFd;

        let mut fds = [libc::pollfd {
            fd: self.master.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        let millis = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
        // SAFETY: `fds` is valid writable storage over a descriptor held open
        // for the call.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, millis) };
        if ready <= 0 {
            return true;
        }
        let mut buffer = [0u8; 8192];
        // SAFETY: reading into owned storage from an open descriptor.
        let read = unsafe {
            libc::read(
                self.master.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
            )
        };
        match read {
            // The last slave closed. On Linux this is `EIO` rather than 0, and
            // both mean the same thing here.
            0 | -1 => false,
            n => {
                self.transcript.extend_from_slice(&buffer[..n as usize]);
                true
            }
        }
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        if self.is_running() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

/// A session directory whose socket is a proxy in front of a real worker.
///
/// This is the SSH tunnel. The client attaches through it exactly as it would
/// attach directly, and [`SocketProxy::cut`] makes the link vanish mid-stream
/// while the worker on the other side carries on. A test that killed the worker
/// instead would be asserting that a dead session is detected, which is a
/// different and much easier claim.
pub struct SocketProxy {
    id: SessionId,
    live: std::sync::Arc<std::sync::Mutex<Vec<UnixStream>>>,
    accepted: std::sync::Arc<AtomicU32>,
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    truncate_next: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl SocketProxy {
    /// Publishes a proxied session in `harness`'s home, forwarding to `upstream`.
    pub fn in_front_of(harness: &Harness, upstream: &Session) -> Self {
        use std::sync::atomic::AtomicBool;
        use std::sync::{Arc, Mutex};

        let id = harness.next_session_id();
        let paths = harness.home().session(&id);
        paths.ensure().expect("cannot create the proxy session dir");
        let listener =
            UnixListener::bind(paths.socket()).expect("cannot bind the proxy control socket");
        listener
            .set_nonblocking(true)
            .expect("cannot poll the proxy listener");

        let live: Arc<Mutex<Vec<UnixStream>>> = Arc::new(Mutex::new(Vec::new()));
        let accepted = Arc::new(AtomicU32::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let truncate_next = Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
        let target = upstream.paths().socket().to_path_buf();

        {
            let live = Arc::clone(&live);
            let accepted = Arc::clone(&accepted);
            let stopped = Arc::clone(&stopped);
            let truncate_next = Arc::clone(&truncate_next);
            std::thread::spawn(move || {
                while !stopped.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((downstream, _)) => {
                            let Ok(upstream) = UnixStream::connect(&target) else {
                                continue;
                            };
                            downstream.set_nonblocking(false).ok();
                            accepted.fetch_add(1, Ordering::Relaxed);
                            for side in [&downstream, &upstream] {
                                if let Ok(handle) = side.try_clone() {
                                    live.lock().expect("proxy lock").push(handle);
                                }
                            }
                            // Consumed by this connection only: a truncated link
                            // that drops is followed by a healthy one, which is
                            // what makes reconnection observable.
                            let limit = truncate_next.swap(u64::MAX, Ordering::Relaxed);
                            pump(&downstream, &upstream, u64::MAX);
                            pump(&upstream, &downstream, limit);
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            });
        }

        Self {
            id,
            live,
            accepted,
            stopped,
            truncate_next,
        }
    }

    /// Makes the next connection carry `bytes` toward the client and then end.
    ///
    /// The cut lands wherever `bytes` happens to fall — part-way through the
    /// `attached` frame, or part-way through the snapshot that follows it. A
    /// client holding an incomplete frame when the stream ends must report a
    /// dead link rather than wait for a remainder that is never coming.
    pub fn truncate_next_connection_after(&self, bytes: u64) {
        self.truncate_next.store(bytes, Ordering::Relaxed);
    }

    /// The session id a client attaches to in order to arrive through here.
    pub fn session_id(&self) -> &SessionId {
        &self.id
    }

    /// How many connections the proxy has carried, reconnections included.
    pub fn connections(&self) -> u32 {
        self.accepted.load(Ordering::Relaxed)
    }

    /// Drops the link mid-stream, leaving the worker running and reattachable.
    pub fn cut(&self) {
        for stream in self.live.lock().expect("proxy lock").drain(..) {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
    }

    /// Stops carrying new connections, as a tunnel that will not come back.
    pub fn close(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        self.cut();
    }
}

impl Drop for SocketProxy {
    fn drop(&mut self) {
        self.close();
    }
}

fn pump(from: &UnixStream, to: &UnixStream, mut budget: u64) {
    let (Ok(mut from), Ok(mut to)) = (from.try_clone(), to.try_clone()) else {
        return;
    };
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        while budget > 0 {
            match from.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let carried = (n as u64).min(budget) as usize;
                    budget -= carried as u64;
                    if to.write_all(&buffer[..carried]).is_err() || to.flush().is_err() {
                        break;
                    }
                }
            }
        }
        let _ = to.shutdown(std::net::Shutdown::Write);
    });
}

/// A launch manifest JSON body suitable for `latch create --manifest-file -`.
pub fn create_manifest_json(
    argv: &[&str],
    cwd: &Path,
    cols: u16,
    rows: u16,
    title: Option<&str>,
    command_label: Option<&str>,
) -> Vec<u8> {
    let mut launch = serde_json::json!({
        "format_version": 1,
        "launch": {
            "argv": argv,
            "cwd": cwd,
            "size": { "cols": cols, "rows": rows },
        },
        "display": {
            "source": { "kind": "cli" }
        }
    });
    if let Some(title) = title {
        launch["display"]["title"] = serde_json::json!(title);
    }
    if let Some(label) = command_label {
        launch["display"]["command_label"] = serde_json::json!(label);
    }
    serde_json::to_vec(&launch).expect("manifest serializes")
}

/// Session directories currently under the harness home, sorted.
pub fn session_dirs(harness: &Harness) -> Vec<String> {
    let sessions = harness.home().sessions_dir();
    if !sessions.exists() {
        return Vec::new();
    }
    dir_entries(&sessions)
}

/// Reads `meta.json` for a session directory name under the harness.
pub fn read_meta(harness: &Harness, session_dir: &str) -> serde_json::Value {
    let path = harness
        .home()
        .sessions_dir()
        .join(session_dir)
        .join("meta.json");
    let bytes =
        std::fs::read(&path).unwrap_or_else(|err| panic!("cannot read {}: {err}", path.display()));
    serde_json::from_slice(&bytes).expect("meta.json is JSON")
}
