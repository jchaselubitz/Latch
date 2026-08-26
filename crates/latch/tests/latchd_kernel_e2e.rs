//! Exclusive attach and session lifecycle, on the `latchd` kernel.
//!
//! The parity gate for `planning/HEADLESS_KERNEL_PROPOSAL.md` Phase A: the
//! real `latch` binary, with `LATCH_KERNEL=latchd`, driving real `latchd`
//! daemons through real PTYs, exercising the paths `exclusive_attach_e2e.rs`
//! proves on the patched tmux — first attach paints the current frame, typed
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
use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const EXIT_STOLEN: i32 = 75;
const EXIT_SESSION_EXITED: i32 = 77;

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
        let temp = tempfile::tempdir().expect("temp dir");
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
            .env("LATCH_KERNEL", "latchd")
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
            &mut size,
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

// -------------------------------------------------------------- control plane

#[test]
fn the_engine_drives_the_pane_through_the_control_plane() {
    with_kernel("control-plane", |h| {
        let id = h.create("stty -echo; while read line; do printf 'line=[%s]\\n' \"$line\"; done");
        thread::sleep(Duration::from_millis(300));
        // The library path the Conversation Hub uses, on this kernel.
        std::env::set_var("LATCH_HOME", &h.home);
        std::env::set_var("LATCH_KERNEL", "latchd");
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
