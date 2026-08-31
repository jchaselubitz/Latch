//! Exclusive attach and session lifecycle, on the `latchd` kernel.
//!
//! The parity gate for `planning/HEADLESS_KERNEL_PROPOSAL.md` Phase A: the
//! real `latch` binary driving real `latchd`
//! daemons through real PTYs, exercising the exclusive-attach paths at the
//! kernel boundary — first attach paints the current frame, typed
//! bytes reach the pane untranslated, steal releases the loser with exit 75,
//! a pane exit releases with 77, geometry follows the surface, and the
//! lifecycle verbs (`list`, `inspect`, `stop`, `remove`) work unchanged.
//!
//! It needs a built daemon: `cargo build -p latchd` puts one next to the
//! `latch` test binary, or set `LATCH_E2E_LATCHD_BIN`. Absence is a hard
//! failure: a parity gate must never turn green without running its kernel.
//! The harness serializes the tests because they hold real PTYs and one case
//! temporarily selects the process-wide engine kernel:
//!
//! ```text
//! cargo build -p latchd && \
//!     cargo test -p latch --test latchd_kernel_e2e -- --test-threads=1
//! ```

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const EXIT_STOLEN: i32 = 75;
const EXIT_SESSION_EXITED: i32 = 77;
const WS_CLOSE_STOLEN: u16 = 4409;

static KERNEL_TEST_LOCK: Mutex<()> = Mutex::new(());

fn daemon_binary() -> PathBuf {
    let path = if let Some(path) = std::env::var_os("LATCH_E2E_LATCHD_BIN") {
        PathBuf::from(path)
    } else {
        let latch = PathBuf::from(env!("CARGO_BIN_EXE_latch"));
        latch
            .parent()
            .expect("latch test binary has no parent")
            .join("latchd")
    };
    fs::canonicalize(&path).unwrap_or_else(|error| {
        panic!(
            "latchd parity binary {} is unavailable: {error}; run `cargo build -p latchd` or set LATCH_E2E_LATCHD_BIN",
            path.display()
        )
    })
}

fn with_kernel(name: &str, body: impl FnOnce(&Harness)) {
    let _serial = KERNEL_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let daemon = daemon_binary();
    eprintln!("latchd parity case: {name}");
    let harness = Harness::new(daemon);
    body(&harness);
}

struct Harness {
    temp: tempfile::TempDir,
    home: PathBuf,
    daemon: PathBuf,
    latch: PathBuf,
}

impl Harness {
    fn new(daemon: PathBuf) -> Self {
        // Unix-domain socket paths are short. Keep the parity root out of a
        // potentially deep runner-specific TMPDIR so the real daemon is
        // exercised instead of failing at bind time.
        let temp = tempfile::Builder::new()
            .prefix("latchd-e2e-")
            .tempdir_in("/tmp")
            .expect("temp dir");
        let home = temp.path().join("home");
        Self {
            temp,
            home,
            daemon,
            latch: PathBuf::from(env!("CARGO_BIN_EXE_latch")),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.latch);
        command
            .env("LATCH_HOME", &self.home)
            .env("LATCH_LATCHD_BIN", &self.daemon)
            .env("LATCHD_SOCKET_DIR", self.temp.path().join("s"))
            .env_remove("LATCH_SESSION_ID")
            .env_remove("TMUX");
        command
    }

    fn create(&self, shell: &str) -> String {
        self.create_sized(shell, 80, 24)
    }

    fn create_sized(&self, shell: &str, cols: u16, rows: u16) -> String {
        self.create_sized_with(self.command(), shell, cols, rows)
    }

    fn create_sized_with(&self, mut command: Command, shell: &str, cols: u16, rows: u16) -> String {
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
        let mut child = command
            .args(["create", "--manifest-file", "-", "--json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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
        serde_json::from_slice(&output.stdout).expect("JSON output")
    }

    fn inspect(&self, id: &str) -> Value {
        self.json(&["inspect", id, "--json"])
    }

    fn surface_attached(&self, id: &str) -> bool {
        self.inspect(id)["surfaceAttached"] == true
    }

    fn socket(&self, id: &str) -> PathBuf {
        let record = latchd::paths::KernelRecord::read(&self.home.join("sessions").join(id))
            .expect("kernel record")
            .expect("kernel record present");
        record.socket
    }

    /// The pane's visible frame, read through the control plane.
    fn visible(&self, id: &str) -> String {
        latchd::client::call(
            &self.socket(id),
            &latchd::protocol::Request::Snapshot {
                format: latchd::protocol::SnapshotFormat::Text,
                scrollback_lines: 0,
            },
        )
        .expect("snapshot")
        .text
        .unwrap_or_default()
    }

    fn wait_visible(&self, id: &str, needle: &str) {
        wait_until(
            || self.visible(id).contains(needle),
            &format!("pane never showed {needle:?}"),
            Duration::from_secs(5),
        );
    }

    fn attach(&self, id: &str, cols: u16, rows: u16) -> Surface {
        Surface::spawn(
            self.command().args(["attach", id]),
            cols,
            rows,
            self.temp.path(),
        )
    }

    fn remove(&self, id: &str) {
        let _ = self
            .command()
            .args(["remove", id, "--force"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// A `latch attach` running on its own PTY, with the master end readable.
struct Surface {
    child: Child,
    master: fs::File,
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
        unsafe { libc::close(slave) };
        set_nonblocking(master);
        Self {
            child,
            master: unsafe { <fs::File as std::os::fd::FromRawFd>::from_raw_fd(master) },
            output: Vec::new(),
        }
    }

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

    fn type_bytes(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).expect("type into surface");
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        set_winsize(&self.master, cols, rows);
    }

    fn release(&mut self, within: Duration) -> Option<i32> {
        let deadline = Instant::now() + within;
        loop {
            self.pump();
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

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        if let Ok(Some(_)) = self.child.try_wait() {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn stdio_dup(fd: libc::c_int) -> Stdio {
    unsafe {
        Stdio::from(<fs::File as std::os::fd::FromRawFd>::from_raw_fd(
            libc::dup(fd),
        ))
    }
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

fn set_nonblocking(raw: libc::c_int) {
    unsafe {
        let flags = libc::fcntl(raw, libc::F_GETFL);
        libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK);
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
    tungstenite::connect(request).expect("terminal websocket").0
}

fn read_ws_until(socket: &mut Ws, needle: &[u8], within: Duration) -> bool {
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_ref() {
        let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
    }
    let deadline = Instant::now() + within;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        match socket.read() {
            Ok(tungstenite::Message::Binary(bytes)) => {
                seen.extend_from_slice(&bytes);
                if seen.windows(needle.len()).any(|window| window == needle) {
                    return true;
                }
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return false,
        }
    }
    false
}

fn read_ws_close(socket: &mut Ws, within: Duration) -> Option<(u16, String)> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        match socket.read() {
            Ok(tungstenite::Message::Close(Some(frame))) => {
                return Some((frame.code.into(), frame.reason.to_string()));
            }
            Ok(_) => {}
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

// ------------------------------------------------------------------ lifecycle

#[test]
fn create_list_inspect_stop_and_remove_work_on_the_daemon_kernel() {
    with_kernel("lifecycle", |h| {
        let id = h.create_sized("printf 'ready\\n'; sleep 60", 100, 30);
        h.wait_visible(&id, "ready");
        let inspect = h.inspect(&id);
        assert_eq!(inspect["state"], "running");
        assert_eq!(inspect["size"], json!({"cols": 100, "rows": 30}));
        assert_eq!(inspect["surfaceAttached"], false);
        assert!(h
            .home
            .join("sessions")
            .join(&id)
            .join("kernel.json")
            .is_file());

        let listed = h.json(&["list", "--json"]);
        assert_eq!(listed["sessions"][0]["id"], id);
        assert_eq!(listed["sessions"][0]["state"], "running");

        let stopped = h.json(&["stop", &id, "--json"]);
        assert_eq!(stopped["state"], "exited");
        let inspect = h.inspect(&id);
        assert_eq!(inspect["state"], "exited");
        assert!(inspect["exit"]["signal"].is_string(), "{inspect}");
        assert!(h
            .home
            .join("sessions")
            .join(&id)
            .join("exit.json")
            .is_file());

        let socket = h.socket(&id);
        h.json(&["remove", &id, "--force", "--json"]);
        assert!(!h.home.join("sessions").join(&id).exists());
        assert!(!socket.exists(), "daemon socket outlived remove");
    });
}

#[test]
fn an_exit_code_is_recorded_and_the_session_stays_inspectable() {
    with_kernel("exit-record", |h| {
        let id = h.create("printf 'last words\\n'; exit 7");
        wait_until(
            || h.inspect(&id)["state"] == "exited",
            "session never exited",
            Duration::from_secs(5),
        );
        let inspect = h.inspect(&id);
        assert_eq!(inspect["exit"]["code"], 7);
        assert!(h.visible(&id).contains("last words"));
        h.remove(&id);
    });
}

#[test]
fn resize_pins_geometry_and_later_attaches_do_not_override_it() {
    with_kernel("resize-pin", |h| {
        let id = h.create("sleep 60");
        h.json(&[
            "resize", &id, "--cols", "132", "--rows", "43", "--pin", "--json",
        ]);
        assert_eq!(h.inspect(&id)["size"], json!({"cols": 132, "rows": 43}));
        let surface = h.attach(&id, 80, 24);
        wait_until(
            || h.surface_attached(&id),
            "no surface",
            Duration::from_secs(5),
        );
        assert_eq!(h.inspect(&id)["size"], json!({"cols": 132, "rows": 43}));
        drop(surface);
        h.remove(&id);
    });
}

// --------------------------------------------------------------------- attach

#[test]
fn local_first_attach_paints_the_current_frame_of_a_silent_pane() {
    with_kernel("first-attach", |h| {
        let id = h.create("printf 'painted before attach\\n'; sleep 60");
        h.wait_visible(&id, "painted before attach");
        let mut surface = h.attach(&id, 80, 24);
        assert!(
            surface.wait_for(b"painted before attach", Duration::from_secs(5)),
            "surface never showed the frame: {:?}",
            surface.text()
        );
        wait_until(
            || h.surface_attached(&id),
            "no surface",
            Duration::from_secs(5),
        );
        drop(surface);
        wait_until(
            || !h.surface_attached(&id),
            "surface never released",
            Duration::from_secs(5),
        );
        h.remove(&id);
    });
}

#[test]
fn typed_input_reaches_the_pane_byte_for_byte() {
    with_kernel("typed-input", |h| {
        let id = h.create("stty -echo; read line; printf 'got:[%s]\\n' \"$line\"; sleep 60");
        thread::sleep(Duration::from_millis(300));
        let mut surface = h.attach(&id, 80, 24);
        wait_until(
            || h.surface_attached(&id),
            "no surface",
            Duration::from_secs(5),
        );
        surface.type_bytes(b"hello \x1b[A world\r");
        assert!(
            surface.wait_for(b"got:[hello \x1b[A world]", Duration::from_secs(5)),
            "{:?}",
            surface.text()
        );
        h.remove(&id);
    });
}

#[test]
fn a_second_local_attach_steals_the_surface_and_the_first_can_steal_it_back() {
    with_kernel("steal", |h| {
        let id = h.create("sleep 60");
        let mut first = h.attach(&id, 80, 24);
        wait_until(
            || h.surface_attached(&id),
            "no surface",
            Duration::from_secs(5),
        );
        let mut second = h.attach(&id, 80, 24);
        assert_eq!(first.release(Duration::from_secs(5)), Some(EXIT_STOLEN));
        assert!(h.surface_attached(&id));
        let mut third = h.attach(&id, 80, 24);
        assert_eq!(second.release(Duration::from_secs(5)), Some(EXIT_STOLEN));
        assert!(h.surface_attached(&id));
        third.pump();
        h.remove(&id);
    });
}

#[test]
fn the_stealing_surface_geometry_wins_and_later_resizes_reach_the_pane() {
    with_kernel("geometry", |h| {
        // The child reports its own winsize continuously, so a WINCH reaching
        // the pane is observable as a new line rather than a trap race.
        let id = h.create("stty -echo; while :; do stty size; sleep 0.2; done");
        let first = h.attach(&id, 80, 24);
        wait_until(
            || h.surface_attached(&id),
            "no surface",
            Duration::from_secs(5),
        );
        let mut second = h.attach(&id, 120, 40);
        assert!(
            second.wait_for(b"40 120", Duration::from_secs(5)),
            "pane did not see the stealing geometry: {:?}",
            second.text()
        );
        assert_eq!(h.inspect(&id)["size"], json!({"cols": 120, "rows": 40}));
        second.resize(90, 30);
        assert!(
            second.wait_for(b"30 90", Duration::from_secs(5)),
            "pane did not see the later resize: {:?}",
            second.text()
        );
        assert_eq!(h.inspect(&id)["size"], json!({"cols": 90, "rows": 30}));
        drop(first);
        h.remove(&id);
    });
}

#[test]
fn a_pane_that_exits_releases_its_surface_with_session_exited() {
    with_kernel("session-exited", |h| {
        let id = h.create("sleep 0.5; printf 'bye\\n'; exit 0");
        let mut surface = h.attach(&id, 80, 24);
        assert_eq!(
            surface.release(Duration::from_secs(5)),
            Some(EXIT_SESSION_EXITED)
        );
        assert!(surface.text().contains("bye"), "{:?}", surface.text());
        assert_eq!(h.inspect(&id)["state"], "exited");
        h.remove(&id);
    });
}

#[test]
fn attaching_to_an_exited_session_paints_the_last_frame_and_releases() {
    with_kernel("attach-exited", |h| {
        let id = h.create("printf 'final frame\\n'; exit 0");
        wait_until(
            || h.inspect(&id)["state"] == "exited",
            "session never exited",
            Duration::from_secs(5),
        );
        let mut surface = h.attach(&id, 80, 24);
        assert_eq!(
            surface.release(Duration::from_secs(5)),
            Some(EXIT_SESSION_EXITED)
        );
        assert!(
            surface.text().contains("final frame"),
            "{:?}",
            surface.text()
        );
        h.remove(&id);
    });
}

#[test]
fn network_gateway_and_local_surface_steal_work_on_latchd() {
    with_kernel("gateway-latchd", |h| {
        let id = h.create("printf 'gateway prompt> '; while :; do sleep 3600; done");
        h.wait_visible(&id, "gateway prompt>");
        let gateway = start_gateway(h);

        let mut desk = h.attach(&id, 100, 30);
        assert!(desk.wait_for(b"gateway prompt>", Duration::from_secs(5)));
        let mut remote = connect_terminal(&gateway, &id, 60, 20);
        assert!(read_ws_until(
            &mut remote,
            b"gateway prompt>",
            Duration::from_secs(10)
        ));
        assert_eq!(desk.release(Duration::from_secs(10)), Some(EXIT_STOLEN));

        let mut desk_again = h.attach(&id, 100, 30);
        assert!(desk_again.wait_for(b"gateway prompt>", Duration::from_secs(5)));
        assert_eq!(
            read_ws_close(&mut remote, Duration::from_secs(10)),
            Some((WS_CLOSE_STOLEN, "stolen".to_owned()))
        );
        drop(desk_again);
        h.remove(&id);
    });
}

// -------------------------------------------------------------- control plane

#[test]
fn the_engine_drives_the_pane_through_the_control_plane() {
    with_kernel("control-plane", |h| {
        let id = h.create("stty -echo; while read line; do printf 'line=[%s]\\n' \"$line\"; done");
        thread::sleep(Duration::from_millis(300));
        // The library path the Conversation Hub uses, on this kernel.
        std::env::set_var("LATCH_HOME", &h.home);
        let home = latch::session::paths::LatchHome::new(&h.home);
        let session = latch::session::paths::SessionId::parse(&id).unwrap();
        latch::engine::paste_message(latch::engine::PasteMessageRequest {
            home: &home,
            id: &session,
            message: b"from the hub",
        })
        .expect("paste");
        h.wait_visible(&id, "line=[from the hub]");
        latch::engine::send_keys(latch::engine::SendKeysRequest {
            home: &home,
            id: &session,
            keys: &["y".to_owned(), "Enter".to_owned()],
        })
        .expect("send keys");
        h.wait_visible(&id, "line=[y]");
        let screen = latch::engine::capture_pane(&home, &session).expect("capture");
        assert!(screen.contains("line=[y]"), "{screen}");
        let metrics =
            latch::engine::pane_metrics_with_timeout(&home, &session, Duration::from_secs(5))
                .expect("metrics");
        assert_eq!(
            (metrics.cols, metrics.rows, metrics.alternate_screen),
            (80, 24, false)
        );
        h.remove(&id);
    });
}

#[test]
fn persistent_hub_control_reconnects_and_resynchronizes_from_events() {
    with_kernel("hub-persistent-control", |h| {
        let id = h.create(
            r#"stty -echo; i=0; while [ $i -lt 40 ]; do echo history-$i; i=$((i+1)); done;
             while read line; do
               if [ "$line" = events ]; then
                 printf '\033]0;hub-title\007'; sleep 0.1;
                 printf '\033[?1049hEVENT-ALT'; sleep 0.1;
                 printf '\033[?1049lEVENT-DONE\n';
               elif [ "$line" = quit ]; then exit 0;
               else printf 'line=[%s]\n' "$line"; fi;
             done"#,
        );
        h.wait_visible(&id, "history-39");
        let home = latch::session::paths::LatchHome::new(&h.home);
        let session = latch::session::paths::SessionId::parse(&id).unwrap();
        let mut control = latch::engine::ConversationControl::open(&home, &session).unwrap();
        assert!(control.is_event_driven());
        assert_eq!(
            control
                .wait_for_activity(Duration::from_millis(20), Duration::from_secs(2))
                .unwrap(),
            latch::engine::ConversationWake::Resynchronized
        );

        // The structured snapshot is a parser barrier and history is fetched
        // over the same persistent control connection.
        let first = control.snapshot(Duration::from_secs(2)).unwrap();
        assert!(first.lines.iter().any(|line| line.contains("history-39")));
        assert!(!first.history.is_empty());

        control.submit("events", Duration::from_secs(2)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw_title = false;
        let mut saw_alt = false;
        let mut saw_quiet = false;
        while Instant::now() < deadline && !(saw_title && saw_alt && saw_quiet) {
            match control
                .wait_for_activity(Duration::from_millis(20), Duration::from_secs(1))
                .unwrap()
            {
                latch::engine::ConversationWake::TitleChanged { title } => {
                    saw_title |= title.as_deref() == Some("hub-title")
                }
                latch::engine::ConversationWake::AlternateScreen { active } => saw_alt |= active,
                latch::engine::ConversationWake::OutputQuiet { .. } => saw_quiet = true,
                _ => {}
            }
        }
        assert!((saw_title && saw_alt && saw_quiet), "missing latchd event");
        let barrier = control.snapshot(Duration::from_secs(2)).unwrap();
        assert!(barrier.lines.iter().any(|line| line.contains("EVENT-DONE")));

        // Losing the event/control object is recovered by a new subscription
        // followed by a mandatory snapshot, not by replaying guessed events.
        drop(control);
        let mut reconnected = latch::engine::ConversationControl::open(&home, &session).unwrap();
        assert_eq!(
            reconnected
                .wait_for_activity(Duration::from_millis(20), Duration::from_secs(2))
                .unwrap(),
            latch::engine::ConversationWake::Resynchronized
        );
        assert!(reconnected
            .snapshot(Duration::from_secs(2))
            .unwrap()
            .lines
            .iter()
            .any(|line| line.contains("EVENT-DONE")));
        reconnected
            .key(&["y".into(), "Enter".into()], Duration::from_secs(2))
            .unwrap();
        h.wait_visible(&id, "line=[y]");
        reconnected.submit("quit", Duration::from_secs(2)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exited = false;
        while Instant::now() < deadline && !exited {
            exited = matches!(
                reconnected
                    .wait_for_activity(Duration::from_millis(20), Duration::from_secs(1))
                    .unwrap(),
                latch::engine::ConversationWake::ChildExited
            );
        }
        assert!(exited, "child-exited was not delivered");
        h.remove(&id);
    });
}

#[test]
fn daemon_suspend_and_resume_preserves_the_session_and_snapshot() {
    with_kernel("daemon-suspend-resume", |h| {
        let id = h.create("printf 'before suspend\\n'; sleep 30");
        h.wait_visible(&id, "before suspend");
        let stat = latchd::client::stat(&h.socket(&id)).unwrap();
        // SAFETY: the daemon pid came from its authenticated control socket.
        assert_eq!(unsafe { libc::kill(stat.daemon_pid, libc::SIGSTOP) }, 0);
        thread::sleep(Duration::from_millis(250));
        // SAFETY: resume the same daemon captured immediately above.
        assert_eq!(unsafe { libc::kill(stat.daemon_pid, libc::SIGCONT) }, 0);
        wait_until(
            || h.inspect(&id)["state"] == "running",
            "session did not recover after daemon resume",
            Duration::from_secs(5),
        );
        let mut surface = h.attach(&id, 80, 24);
        assert!(surface.wait_for(b"before suspend", Duration::from_secs(5)));
        drop(surface);
        h.remove(&id);
    });
}

#[test]
fn abrupt_daemon_failure_is_reported_lost_and_remains_removable() {
    with_kernel("daemon-failure", |h| {
        let id = h.create("printf 'before daemon failure\\n'; sleep 30");
        h.wait_visible(&id, "before daemon failure");
        let stat = latchd::client::stat(&h.socket(&id)).unwrap();
        // SAFETY: the daemon pid came from its authenticated control socket.
        assert_eq!(unsafe { libc::kill(stat.daemon_pid, libc::SIGKILL) }, 0);
        wait_until(
            || h.inspect(&id)["state"] == "lost",
            "daemon failure was not surfaced as lost",
            Duration::from_secs(5),
        );
        let output = h
            .command()
            .args(["remove", &id, "--force"])
            .output()
            .expect("remove failed daemon session");
        assert_success(&output);
        assert!(!h.home.join("sessions").join(&id).exists());
    });
}
