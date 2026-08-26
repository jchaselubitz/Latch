//! The session daemon: one child in one PTY, one surface, many observers.
//!
//! Threads, and the one rule that orders them:
//!
//! - **reader** — reads the PTY master. Under the session lock it appends
//!   each chunk to the surface's queue (if any) and to the parser's queue.
//!   Holding the lock for both pushes is what makes a surface installation
//!   a point in the byte stream: everything before it is in the snapshot,
//!   everything after it is in the queue, nothing is in both or neither.
//! - **parser** — owns the screen model, consumes the parser queue in order.
//!   A snapshot request is an item in that queue, so it is answered from the
//!   state at exactly its position in the stream.
//! - **surface writer** — drains the surface queue into the socket. If the
//!   queue outgrows [`SURFACE_QUEUE_CAP`] the reader evicts the surface;
//!   the child is never asked to wait for a viewer.
//! - **connection** threads — one per accepted socket; control verbs, or
//!   the surface's input half after an attach handshake.
//!
//! Invariant, from `DECISION_EXCLUSIVE_ATTACH.md`: the screen model is never
//! on the live path. After the handshake the surface receives the child's
//! bytes, unmodified.

use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use latch_term::{Screen, Size, Terminal, TerminalConfig};

use crate::keys::{self, KeyModes};
use crate::paths::{KernelRecord, EXIT_RECORD, KERNEL_NAME};
use crate::protocol::{
    self, Event, Exit, ReleaseReason, Reply, Request, Response, SnapshotFormat, Stat, State,
    PROTOCOL_VERSION,
};
use crate::pty::{self, PtyChild};
use crate::render;

/// Bytes a surface may fall behind before it is evicted.
pub const SURFACE_QUEUE_CAP: usize = 4 * 1024 * 1024;
/// PTY read size.
const READ_CHUNK: usize = 64 * 1024;
/// Release reasons retained for late lookup.
const RELEASE_HISTORY: usize = 64;
/// Scrollback lines the screen model retains.
const SCROLLBACK_LINES: usize = 50_000;

/// What to run and where to listen.
#[derive(Debug, Clone)]
pub struct Config {
    /// Session id, reported by `stat`.
    pub id: String,
    /// Socket to listen on. Any stale file is replaced.
    pub socket: PathBuf,
    /// Session directory for `kernel.json` and `exit.json`, if any.
    pub session_dir: Option<PathBuf>,
    /// Program and arguments.
    pub argv: Vec<String>,
    /// Working directory.
    pub cwd: PathBuf,
    /// Environment overrides merged over the daemon's own.
    pub env: Vec<(String, String)>,
    /// Initial columns.
    pub cols: u16,
    /// Initial rows.
    pub rows: u16,
    /// Milliseconds without output before `output-quiet` is announced.
    pub quiet_ms: u64,
}

/// A surface's outbound queue.
struct Queue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

struct QueueState {
    buf: VecDeque<u8>,
    /// The handshake has not finished; the writer must not drain yet.
    held: bool,
    closed: bool,
}

impl Queue {
    fn new() -> Self {
        Self {
            state: Mutex::new(QueueState {
                buf: VecDeque::new(),
                held: true,
                closed: false,
            }),
            ready: Condvar::new(),
        }
    }

    /// Appends, reporting whether the cap was exceeded.
    fn push(&self, bytes: &[u8]) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return false;
        }
        state.buf.extend(bytes);
        let over = state.buf.len() > SURFACE_QUEUE_CAP;
        drop(state);
        self.ready.notify_one();
        over
    }

    fn release_hold(&self) {
        self.state.lock().unwrap().held = false;
        self.ready.notify_one();
    }

    fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.ready.notify_all();
    }

    /// Blocks for the next batch; `None` once closed and drained.
    fn pop(&self) -> Option<Vec<u8>> {
        let mut state = self.state.lock().unwrap();
        loop {
            if !state.held && !state.buf.is_empty() {
                let take = state.buf.len().min(READ_CHUNK);
                return Some(state.buf.drain(..take).collect());
            }
            if state.closed {
                return None;
            }
            state = self.ready.wait(state).unwrap();
        }
    }
}

struct Surface {
    id: u64,
    stream: UnixStream,
    queue: Arc<Queue>,
}

/// Mutable session facts, under one lock.
struct Session {
    cols: u16,
    rows: u16,
    pinned: bool,
    exit: Option<Exit>,
    activity: u64,
    surface: Option<Surface>,
    ever_attached: bool,
    releases: VecDeque<(u64, ReleaseReason)>,
}

/// Work for the parser thread, in stream order.
enum Item {
    Output(Vec<u8>),
    Resize(u16, u16),
    Query(Box<dyn FnOnce(&mut Terminal) + Send>),
}

struct Parser {
    queue: Mutex<VecDeque<Item>>,
    ready: Condvar,
}

impl Parser {
    fn push(&self, item: Item) {
        self.queue.lock().unwrap().push_back(item);
        self.ready.notify_one();
    }

    fn pop(&self) -> Item {
        let mut queue = self.queue.lock().unwrap();
        loop {
            if let Some(item) = queue.pop_front() {
                return item;
            }
            queue = self.ready.wait(queue).unwrap();
        }
    }
}

/// Event fan-out.
struct Events {
    subscribers: Mutex<Vec<mpsc::Sender<Event>>>,
}

impl Events {
    fn broadcast(&self, event: Event) {
        let mut subscribers = self.subscribers.lock().unwrap();
        subscribers.retain(|sender| sender.send(event.clone()).is_ok());
    }

    fn subscribe(&self) -> mpsc::Receiver<Event> {
        let (sender, receiver) = mpsc::channel();
        self.subscribers.lock().unwrap().push(sender);
        receiver
    }
}

struct Shared {
    config: Config,
    child_pid: i32,
    /// Serialized writes into the child.
    input: Mutex<File>,
    master_fd: i32,
    session: Mutex<Session>,
    changed: Condvar,
    parser: Parser,
    events: Events,
    next_surface: AtomicU64,
    last_output: Mutex<Instant>,
    quiet_announced: AtomicBool,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Runs a session to completion. `ready` is called once the child is spawned
/// and the socket is listening, so a parent can stop waiting.
pub fn run(config: Config, ready: impl FnOnce()) -> Result<()> {
    let env: Vec<(String, String)> = {
        let mut merged: Vec<(String, String)> = std::env::vars().collect();
        for (key, value) in &config.env {
            merged.retain(|(k, _)| k != key);
            merged.push((key.clone(), value.clone()));
        }
        merged
    };
    // Listen and record before the child exists: the child (or its launch
    // shim) may look for the socket the moment it starts. Connections that
    // arrive before the accept loop simply wait in the backlog.
    let _ = fs::remove_file(&config.socket);
    let listener = UnixListener::bind(&config.socket)
        .with_context(|| format!("cannot listen on {}", config.socket.display()))?;
    fs::set_permissions(&config.socket, fs::Permissions::from_mode(0o600))?;
    if let Some(dir) = &config.session_dir {
        KernelRecord {
            kernel: KERNEL_NAME.into(),
            socket: config.socket.clone(),
            pid: std::process::id() as i32,
        }
        .write(dir)
        .with_context(|| format!("cannot record the kernel in {}", dir.display()))?;
    }
    let PtyChild { master, pid } = match pty::spawn(
        &config.argv,
        &config.cwd,
        Some(&env),
        config.cols,
        config.rows,
    ) {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_file(&config.socket);
            if let Some(dir) = &config.session_dir {
                let _ = fs::remove_file(dir.join(crate::paths::KERNEL_RECORD));
            }
            return Err(error).context("cannot spawn the session command");
        }
    };

    let input = pty::dup_file(master.as_raw_fd())?;
    let output = pty::dup_file(master.as_raw_fd())?;
    let shared = Arc::new(Shared {
        child_pid: pid,
        input: Mutex::new(input),
        master_fd: master.as_raw_fd(),
        session: Mutex::new(Session {
            cols: config.cols,
            rows: config.rows,
            pinned: false,
            exit: None,
            activity: unix_now(),
            surface: None,
            ever_attached: false,
            releases: VecDeque::new(),
        }),
        changed: Condvar::new(),
        parser: Parser {
            queue: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
        },
        events: Events {
            subscribers: Mutex::new(Vec::new()),
        },
        next_surface: AtomicU64::new(1),
        last_output: Mutex::new(Instant::now()),
        quiet_announced: AtomicBool::new(true),
        config,
    });
    install_signal_handlers(&shared);

    {
        let shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("parser".into())
            .spawn(move || parser_loop(&shared))?;
    }
    {
        let shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("reader".into())
            .spawn(move || reader_loop(&shared, output))?;
    }
    {
        let shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("quiet".into())
            .spawn(move || quiet_loop(&shared))?;
    }
    ready();

    for connection in listener.incoming() {
        let stream = match connection {
            Ok(stream) => stream,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("accept failed"),
        };
        if !peer_is_us(&stream) {
            continue;
        }
        let shared = Arc::clone(&shared);
        let _ = thread::Builder::new()
            .name("conn".into())
            .spawn(move || connection_loop(&shared, stream));
    }
    drop(master);
    Ok(())
}

static SOCKET_TO_UNLINK: Mutex<Option<std::ffi::CString>> = Mutex::new(None);
static CHILD_TO_HUP: AtomicU64 = AtomicU64::new(0);

extern "C" fn on_terminate(_signal: libc::c_int) {
    // Async-signal-safe only: kill, unlink, _exit.
    let pid = CHILD_TO_HUP.load(Ordering::Relaxed) as i32;
    if pid > 0 {
        // SAFETY: signalling the child group and unlinking our own socket.
        unsafe {
            libc::kill(-pid, libc::SIGHUP);
        }
    }
    if let Ok(guard) = SOCKET_TO_UNLINK.try_lock() {
        if let Some(path) = guard.as_ref() {
            unsafe {
                libc::unlink(path.as_ptr());
            }
        }
    }
    unsafe { libc::_exit(0) }
}

fn install_signal_handlers(shared: &Shared) {
    CHILD_TO_HUP.store(shared.child_pid as u64, Ordering::Relaxed);
    if let Ok(path) = std::ffi::CString::new(shared.config.socket.as_os_str().as_encoded_bytes()) {
        *SOCKET_TO_UNLINK.lock().unwrap() = Some(path);
    }
    // SAFETY: installing handlers with plain function pointers.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(
            libc::SIGTERM,
            on_terminate as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
}

fn peer_is_us(stream: &UnixStream) -> bool {
    let fd = stream.as_raw_fd();
    let mut uid: libc::uid_t = u32::MAX;
    let mut gid: libc::gid_t = u32::MAX;
    // SAFETY: getpeereid writes two ids we own.
    let rc = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
    // SAFETY: getuid has no preconditions.
    rc == 0 && uid == unsafe { libc::getuid() }
}

// ---------------------------------------------------------------------------
// Reader: PTY -> surface queue + parser queue

fn reader_loop(shared: &Arc<Shared>, mut output: File) {
    let mut buf = vec![0u8; READ_CHUNK];
    loop {
        let n = match output.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            // EIO is how a PTY master reports the slave side closed.
            Err(_) => break,
        };
        let chunk = &buf[..n];
        *shared.last_output.lock().unwrap() = Instant::now();
        shared.quiet_announced.store(false, Ordering::Relaxed);
        let mut evict = None;
        {
            let mut session = shared.session.lock().unwrap();
            session.activity = unix_now();
            if let Some(surface) = &session.surface {
                if surface.queue.push(chunk) {
                    evict = Some(surface.id);
                }
            }
            shared.parser.push(Item::Output(chunk.to_vec()));
        }
        if let Some(id) = evict {
            release_surface(shared, id, ReleaseReason::SlowClient);
        }
    }
    // The slave closed: reap the child and record how it ended.
    let (status, signal) = pty::wait(shared.child_pid);
    let exit = Exit {
        status,
        signal,
        exited_at: unix_now(),
    };
    if let Some(dir) = &shared.config.session_dir {
        if let Ok(body) = serde_json::to_vec_pretty(&exit) {
            let _ = fs::write(dir.join(EXIT_RECORD), body);
        }
    }
    let holder = {
        let mut session = shared.session.lock().unwrap();
        session.exit = Some(exit.clone());
        session.activity = exit.exited_at;
        session.surface.as_ref().map(|surface| surface.id)
    };
    shared.changed.notify_all();
    // Let the parser reach the end of the stream before anyone looks at the
    // final frame, then release the holder: its last bytes are queued ahead
    // of the close.
    parser_barrier(shared);
    if let Some(id) = holder {
        release_surface(shared, id, ReleaseReason::SessionExited);
    }
    shared.events.broadcast(Event::ChildExited { exit });
}

fn quiet_loop(shared: &Arc<Shared>) {
    let quiet = Duration::from_millis(shared.config.quiet_ms.max(50));
    loop {
        thread::sleep(quiet / 4);
        if shared.quiet_announced.load(Ordering::Relaxed) {
            continue;
        }
        let elapsed = shared.last_output.lock().unwrap().elapsed();
        if elapsed >= quiet {
            shared.quiet_announced.store(true, Ordering::Relaxed);
            shared.events.broadcast(Event::OutputQuiet {
                ms: elapsed.as_millis() as u64,
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Parser: owns the screen model

fn parser_loop(shared: &Arc<Shared>) {
    let mut terminal = Terminal::new(
        TerminalConfig::new(Size::new(shared.config.cols, shared.config.rows))
            .with_scrollback_limit(SCROLLBACK_LINES),
    );
    let mut alternate = false;
    let mut title: Option<String> = None;
    loop {
        match shared.parser.pop() {
            Item::Output(bytes) => {
                terminal.advance(&bytes);
                let now_alternate = terminal.alternate_screen_active();
                if now_alternate != alternate {
                    alternate = now_alternate;
                    shared
                        .events
                        .broadcast(Event::AltScreen { active: alternate });
                }
                let now_title = terminal.title();
                if now_title != title {
                    title = now_title;
                    shared.events.broadcast(Event::TitleChanged {
                        title: title.clone(),
                    });
                }
            }
            Item::Resize(cols, rows) => terminal.resize(Size::new(cols, rows)),
            Item::Query(query) => query(&mut terminal),
        }
    }
}

/// Runs `query` on the parser thread at the current stream position and
/// waits for its answer.
fn parser_query<T: Send + 'static>(
    shared: &Shared,
    query: impl FnOnce(&mut Terminal) -> T + Send + 'static,
) -> T {
    let (sender, receiver) = mpsc::channel();
    shared.parser.push(Item::Query(Box::new(move |terminal| {
        let _ = sender.send(query(terminal));
    })));
    receiver.recv().expect("parser thread is alive")
}

fn parser_barrier(shared: &Shared) {
    parser_query(shared, |_| ());
}

// ---------------------------------------------------------------------------
// Surface lifecycle

/// Releases surface `id` with `reason` if it is still the holder. A surface
/// that has already been released keeps its first reason.
fn release_surface(shared: &Shared, id: u64, reason: ReleaseReason) {
    let surface = {
        let mut session = shared.session.lock().unwrap();
        match &session.surface {
            Some(surface) if surface.id == id => {
                session.releases.push_back((id, reason));
                while session.releases.len() > RELEASE_HISTORY {
                    session.releases.pop_front();
                }
                session.surface.take()
            }
            _ => None,
        }
    };
    let Some(surface) = surface else {
        return;
    };
    surface.queue.close();
    let _ = surface.stream.shutdown(std::net::Shutdown::Both);
    shared.changed.notify_all();
    shared.events.broadcast(Event::SurfaceDetached {
        surface: id,
        reason,
    });
}

fn surface_writer_loop(shared: &Arc<Shared>, id: u64, queue: Arc<Queue>, mut stream: UnixStream) {
    while let Some(batch) = queue.pop() {
        if stream.write_all(&batch).is_err() {
            release_surface(shared, id, ReleaseReason::Normal);
            return;
        }
    }
}

/// The attach handshake and the surface's input half.
fn attach(shared: &Arc<Shared>, mut stream: UnixStream, cols: u16, rows: u16) {
    let id = shared.next_surface.fetch_add(1, Ordering::Relaxed);
    let queue = Arc::new(Queue::new());
    let Ok(writer_stream) = stream.try_clone() else {
        return;
    };
    let Ok(holder_stream) = stream.try_clone() else {
        return;
    };

    // Install: steal, resize, mark the snapshot point — one critical section.
    let (previous, snapshot_rx, exited) = {
        let mut session = shared.session.lock().unwrap();
        let previous = session.surface.take();
        if let Some(previous) = &previous {
            session
                .releases
                .push_back((previous.id, ReleaseReason::Stolen));
        }
        let resize = !session.pinned
            && (session.cols, session.rows) != (cols, rows)
            && session.exit.is_none();
        if resize && pty::resize(shared.master_fd, cols, rows).is_ok() {
            session.cols = cols;
            session.rows = rows;
            shared.parser.push(Item::Resize(cols, rows));
        }
        session.surface = Some(Surface {
            id,
            stream: holder_stream,
            queue: Arc::clone(&queue),
        });
        session.ever_attached = true;
        let (sender, receiver) = mpsc::channel();
        shared.parser.push(Item::Query(Box::new(move |terminal| {
            let _ = sender.send(terminal.snapshot());
        })));
        (previous, receiver, session.exit.is_some())
    };
    shared.changed.notify_all();
    if let Some(previous) = previous {
        previous.queue.close();
        let _ = previous.stream.shutdown(std::net::Shutdown::Both);
        shared.events.broadcast(Event::SurfaceDetached {
            surface: previous.id,
            reason: ReleaseReason::Stolen,
        });
    }

    let snapshot = snapshot_rx.recv().unwrap_or_default();
    let response = Response::ok(Reply {
        surface: Some(id),
        snapshot_len: Some(snapshot.len()),
        ..Reply::default()
    });
    if protocol::write_frame(&mut stream, &response).is_err()
        || stream.write_all(&snapshot).is_err()
    {
        release_surface(shared, id, ReleaseReason::Normal);
        return;
    }
    shared
        .events
        .broadcast(Event::SurfaceAttached { surface: id });

    if exited {
        // Nothing more will ever be painted; hand the frame over and end.
        release_surface(shared, id, ReleaseReason::SessionExited);
        return;
    }

    queue.release_hold();
    {
        let shared = Arc::clone(shared);
        let queue = Arc::clone(&queue);
        let _ = thread::Builder::new()
            .name("surface-writer".into())
            .spawn(move || surface_writer_loop(&shared, id, queue, writer_stream));
    }

    // Input half: raw bytes into the child until the client goes away.
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if write_input(shared, &buf[..n]).is_err() {
                    break;
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    release_surface(shared, id, ReleaseReason::Normal);
}

fn write_input(shared: &Shared, bytes: &[u8]) -> io::Result<()> {
    let mut input = shared.input.lock().unwrap();
    input.write_all(bytes)?;
    shared.session.lock().unwrap().activity = unix_now();
    Ok(())
}

// ---------------------------------------------------------------------------
// Control connections

fn connection_loop(shared: &Arc<Shared>, mut stream: UnixStream) {
    loop {
        let request: Request = match protocol::read_frame(&mut stream) {
            Ok(Some(request)) => request,
            Ok(None) | Err(_) => return,
        };
        match request {
            Request::Attach {
                cols,
                rows,
                protocol,
            } => {
                if protocol != PROTOCOL_VERSION {
                    let _ = protocol::write_frame(
                        &mut stream,
                        &Response::err(format!(
                            "kernel speaks protocol {PROTOCOL_VERSION}, client speaks {protocol}"
                        )),
                    );
                    return;
                }
                attach(shared, stream, cols.max(1), rows.max(1));
                return;
            }
            Request::Subscribe => {
                let _ = protocol::write_frame(&mut stream, &Response::done());
                let receiver = shared.events.subscribe();
                while let Ok(event) = receiver.recv() {
                    if protocol::write_frame(&mut stream, &event).is_err() {
                        return;
                    }
                }
                return;
            }
            Request::Kill => {
                let _ = protocol::write_frame(&mut stream, &Response::done());
                shutdown(shared);
            }
            other => {
                let response = match handle(shared, other) {
                    Ok(reply) => Response::ok(reply),
                    Err(error) => Response::err(error.to_string()),
                };
                if protocol::write_frame(&mut stream, &response).is_err() {
                    return;
                }
            }
        }
    }
}

fn shutdown(shared: &Shared) -> ! {
    let running = shared.session.lock().unwrap().exit.is_none();
    if running {
        let _ = pty::signal_group(shared.child_pid, libc::SIGHUP);
    }
    let _ = fs::remove_file(&shared.config.socket);
    std::process::exit(0)
}

fn handle(shared: &Arc<Shared>, request: Request) -> Result<Reply> {
    Ok(match request {
        Request::Stat => Reply {
            stat: Some(stat(shared)),
            ..Reply::default()
        },
        Request::Write { bytes } => {
            require_running(shared)?;
            write_input(shared, &bytes)?;
            Reply::default()
        }
        Request::Key { keys } => {
            require_running(shared)?;
            let modes = parser_query(shared, |terminal| terminal.modes());
            let modes = KeyModes {
                application_cursor_keys: modes.application_cursor_keys,
                application_keypad: modes.application_keypad,
            };
            let mut bytes = Vec::new();
            for key in &keys {
                bytes.extend(keys::encode(key, modes));
            }
            write_input(shared, &bytes)?;
            Reply::default()
        }
        Request::Paste { text } => {
            require_running(shared)?;
            write_input(shared, &paste_bytes(shared, &text))?;
            Reply::default()
        }
        Request::Submit { text } => {
            require_running(shared)?;
            let mut bytes = paste_bytes(shared, &text);
            bytes.push(b'\r');
            write_input(shared, &bytes)?;
            Reply::default()
        }
        Request::Snapshot {
            format,
            scrollback_lines,
        } => snapshot(shared, format, scrollback_lines)?,
        Request::History { max } => {
            let (lines, dropped) = parser_query(shared, move |terminal| {
                let len = terminal.scrollback_len();
                let start = len.saturating_sub(max as usize);
                let lines: Vec<String> = (start..len)
                    .filter_map(|index| terminal.scrollback_line(index))
                    .map(|row| render::row_text(&row))
                    .collect();
                (lines, terminal.scrollback_dropped())
            });
            Reply {
                lines: Some(lines),
                dropped: Some(dropped),
                ..Reply::default()
            }
        }
        Request::Resize { cols, rows, pin } => {
            let (cols, rows) = (cols.max(1), rows.max(1));
            let mut session = shared.session.lock().unwrap();
            if session.exit.is_none() {
                pty::resize(shared.master_fd, cols, rows).context("cannot resize the terminal")?;
            }
            session.cols = cols;
            session.rows = rows;
            session.pinned |= pin;
            shared.parser.push(Item::Resize(cols, rows));
            Reply::default()
        }
        Request::Signal { signal } => {
            require_running(shared)?;
            pty::signal_group(shared.child_pid, signal).context("cannot signal the session")?;
            Reply::default()
        }
        Request::AwaitSurface { timeout_ms } => {
            let deadline = Instant::now() + Duration::from_millis(timeout_ms);
            let mut session = shared.session.lock().unwrap();
            let attached = loop {
                if session.ever_attached {
                    break true;
                }
                if session.exit.is_some() {
                    break false;
                }
                let now = Instant::now();
                if now >= deadline {
                    break false;
                }
                let (guard, _) = shared
                    .changed
                    .wait_timeout(session, deadline - now)
                    .unwrap();
                session = guard;
            };
            Reply {
                attached: Some(attached),
                ..Reply::default()
            }
        }
        Request::ReleaseReason { surface } => {
            let session = shared.session.lock().unwrap();
            let reason = session
                .releases
                .iter()
                .rev()
                .find(|(id, _)| *id == surface)
                .map(|(_, reason)| *reason);
            match reason {
                Some(reason) => Reply {
                    reason: Some(reason),
                    ..Reply::default()
                },
                None if session.surface.as_ref().is_some_and(|s| s.id == surface) => {
                    anyhow::bail!("surface {surface} is still attached")
                }
                None => anyhow::bail!("unknown surface {surface}"),
            }
        }
        Request::Attach { .. } | Request::Subscribe | Request::Kill => unreachable!(),
    })
}

fn require_running(shared: &Shared) -> Result<()> {
    if shared.session.lock().unwrap().exit.is_some() {
        anyhow::bail!("session has exited");
    }
    Ok(())
}

fn paste_bytes(shared: &Shared, text: &str) -> Vec<u8> {
    let bracketed = parser_query(shared, |terminal| terminal.modes().bracketed_paste);
    let mut bytes = Vec::with_capacity(text.len() + 12);
    if bracketed {
        bytes.extend_from_slice(b"\x1b[200~");
    }
    bytes.extend_from_slice(text.as_bytes());
    if bracketed {
        bytes.extend_from_slice(b"\x1b[201~");
    }
    bytes
}

fn stat(shared: &Shared) -> Stat {
    let (alternate_screen, title) = parser_query(shared, |terminal| {
        (terminal.alternate_screen_active(), terminal.title())
    });
    let session = shared.session.lock().unwrap();
    Stat {
        id: shared.config.id.clone(),
        protocol: PROTOCOL_VERSION,
        daemon_pid: std::process::id() as i32,
        child_pid: shared.child_pid,
        state: if session.exit.is_some() {
            State::Exited
        } else {
            State::Running
        },
        cols: session.cols,
        rows: session.rows,
        pinned: session.pinned,
        attached: session.surface.is_some(),
        activity: session.activity,
        exit: session.exit.clone(),
        alternate_screen,
        title,
    }
}

fn snapshot(shared: &Shared, format: SnapshotFormat, scrollback_lines: u32) -> Result<Reply> {
    Ok(parser_query(shared, move |terminal| {
        let model = terminal.model();
        let history = |styled: bool| -> String {
            if scrollback_lines == 0 || model.alternate_screen {
                return String::new();
            }
            let len = terminal.scrollback_len();
            let start = len.saturating_sub(scrollback_lines as usize);
            let mut out = String::new();
            for index in start..len {
                if let Some(row) = terminal.scrollback_line(index) {
                    out.push_str(&if styled {
                        render::styled_row(&row)
                    } else {
                        render::row_text(&row)
                    });
                    out.push('\n');
                }
            }
            out
        };
        match format {
            SnapshotFormat::Text => Reply {
                text: Some(history(false) + &render::text(&model)),
                ..Reply::default()
            },
            SnapshotFormat::Styled => Reply {
                text: Some(history(true) + &render::styled(&model)),
                ..Reply::default()
            },
            SnapshotFormat::Escape => Reply {
                bytes: Some(terminal.snapshot()),
                ..Reply::default()
            },
            SnapshotFormat::Json => Reply {
                screen: Some(render::json(&model)),
                ..Reply::default()
            },
        }
    }))
}
