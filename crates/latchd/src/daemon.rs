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
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use latch_term::{Screen, Size, Terminal, TerminalConfig};

use crate::keys::{self, KeyModes};
use crate::paths::{self, KernelRecord, EXIT_RECORD, KERNEL_NAME, KERNEL_RECORD};
use crate::peer;
use crate::protocol::{
    self, Event, Exit, ReleaseReason, Reply, Request, Response, SnapshotFormat, Stat, State,
    MAX_DIMENSION, PROTOCOL_VERSION,
};
use crate::pty::{self, PtyChild};
use crate::render;

/// Bytes a surface may fall behind before it is evicted.
pub const SURFACE_QUEUE_CAP: usize = 4 * 1024 * 1024;
/// Bytes the screen model may fall behind the child before the reader
/// stops taking output.
///
/// The parser is off the live path, so a child that writes faster than the
/// model can parse would otherwise grow this queue without limit and the
/// daemon would be the process the OS kills. Past the cap the reader waits,
/// the PTY buffer fills, and the child blocks on its own `write` — exactly
/// what a slow physical terminal would do to it. Surfaces are unaffected:
/// they are fed before the wait, and a slow one is evicted, never waited on.
pub const PARSER_BACKLOG_CAP: u64 = 32 * 1024 * 1024;
/// Events a subscriber may leave unread before it must reconnect and resync.
pub const EVENT_QUEUE_CAP: usize = 1024;
/// PTY read size.
const READ_CHUNK: usize = 64 * 1024;
/// Release reasons retained for late lookup.
const RELEASE_HISTORY: usize = 64;
/// Scrollback lines the screen model retains.
const SCROLLBACK_LINES: usize = 50_000;
/// Default ceiling for a creator to finish handing its launch manifest over.
pub const DEFAULT_LAUNCH_TIMEOUT_MS: u64 = 15_000;

/// What to run and where to listen.
#[derive(Debug, Clone)]
pub struct Config {
    /// Session id, reported by `stat`.
    pub id: String,
    /// Socket to listen on. Any stale file is replaced.
    pub socket: PathBuf,
    /// Session directory for `kernel.json` and `exit.json`, if any.
    pub session_dir: Option<PathBuf>,
    /// FIFO removed once the creator has completed launch handoff.
    pub launch_marker: Option<PathBuf>,
    /// Ceiling for [`Self::launch_marker`] to disappear.
    pub launch_timeout_ms: u64,
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
    peak_bytes: AtomicU64,
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
            peak_bytes: AtomicU64::new(0),
        }
    }

    /// Appends, reporting whether the cap was exceeded.
    fn push(&self, bytes: &[u8]) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return false;
        }
        state.buf.extend(bytes);
        self.peak_bytes
            .fetch_max(state.buf.len() as u64, Ordering::Relaxed);
        let over = state.buf.len() > SURFACE_QUEUE_CAP;
        drop(state);
        self.ready.notify_one();
        over
    }

    fn len(&self) -> u64 {
        self.state.lock().unwrap().buf.len() as u64
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
    /// Signalled after each output item is consumed, for [`Parser::wait_for_room`].
    drained: Condvar,
    backlog_bytes: AtomicU64,
    peak_bytes: AtomicU64,
    /// Times the reader paused on the cap, for observability.
    stalls: AtomicU64,
}

impl Parser {
    fn push(&self, item: Item) {
        if let Item::Output(bytes) = &item {
            let backlog = self
                .backlog_bytes
                .fetch_add(bytes.len() as u64, Ordering::Relaxed)
                + bytes.len() as u64;
            self.peak_bytes.fetch_max(backlog, Ordering::Relaxed);
        }
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

    /// Records that `len` output bytes were consumed and wakes a waiting reader.
    fn consumed(&self, len: u64) {
        self.backlog_bytes.fetch_sub(len, Ordering::Relaxed);
        let _queue = self.queue.lock().unwrap();
        self.drained.notify_all();
    }

    /// Blocks until the output backlog is within [`PARSER_BACKLOG_CAP`].
    /// Only the reader calls this, and never while holding the session lock:
    /// blocking there would stall attaches and `stat` along with the child.
    fn wait_for_room(&self) {
        if self.backlog_bytes.load(Ordering::Relaxed) <= PARSER_BACKLOG_CAP {
            return;
        }
        self.stalls.fetch_add(1, Ordering::Relaxed);
        let mut queue = self.queue.lock().unwrap();
        while self.backlog_bytes.load(Ordering::Relaxed) > PARSER_BACKLOG_CAP {
            queue = self.drained.wait(queue).unwrap();
        }
    }
}

/// Event fan-out.
struct Events {
    subscribers: Mutex<Vec<mpsc::SyncSender<Event>>>,
    evictions: AtomicU64,
}

impl Events {
    fn broadcast(&self, event: Event) {
        let mut subscribers = self.subscribers.lock().unwrap();
        subscribers.retain(|sender| match sender.try_send(event.clone()) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Disconnected(_)) => false,
            Err(mpsc::TrySendError::Full(_)) => {
                self.evictions.fetch_add(1, Ordering::Relaxed);
                false
            }
        });
    }

    fn subscribe(&self) -> mpsc::Receiver<Event> {
        let (sender, receiver) = mpsc::sync_channel(EVENT_QUEUE_CAP);
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
    bytes_from_child: AtomicU64,
    bytes_to_surfaces: AtomicU64,
    surface_queue_peak: AtomicU64,
    surface_attaches: AtomicU64,
    surface_steals: AtomicU64,
    slow_client_evictions: AtomicU64,
    control_failures: AtomicU64,
    parser_resets: AtomicU64,
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
    let listener = listen(&config.socket)?;
    raise_open_file_limit();
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
                let _ = fs::remove_file(dir.join(KERNEL_RECORD));
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
            drained: Condvar::new(),
            backlog_bytes: AtomicU64::new(0),
            peak_bytes: AtomicU64::new(0),
            stalls: AtomicU64::new(0),
        },
        events: Events {
            subscribers: Mutex::new(Vec::new()),
            evictions: AtomicU64::new(0),
        },
        next_surface: AtomicU64::new(1),
        last_output: Mutex::new(Instant::now()),
        quiet_announced: AtomicBool::new(true),
        bytes_from_child: AtomicU64::new(0),
        bytes_to_surfaces: AtomicU64::new(0),
        surface_queue_peak: AtomicU64::new(0),
        surface_attaches: AtomicU64::new(0),
        surface_steals: AtomicU64::new(0),
        slow_client_evictions: AtomicU64::new(0),
        control_failures: AtomicU64::new(0),
        parser_resets: AtomicU64::new(0),
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
    {
        let shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("lifecycle".into())
            .spawn(move || lifecycle_loop(&shared))?;
    }
    ready();

    for connection in listener.incoming() {
        let stream = match connection {
            Ok(stream) => stream,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if accept_error_is_transient(&error) => {
                // Descriptor or memory pressure, or a peer that vanished
                // between connect and accept. Neither is a reason to exit
                // and orphan the child behind a stale socket; wait for the
                // pressure to pass and keep serving.
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            Err(_) => {
                // The listener itself is broken. End the session cleanly —
                // child signalled, socket and record removed — rather than
                // leave a child nobody can reach.
                shutdown(&shared);
            }
        };
        if !peer::is_same_user(&stream) {
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

/// Binds the session socket, owner-accessible only from the first instant.
///
/// A live daemon already on the path is left alone and this one refuses to
/// start: replacing its socket would strand a running child behind a path
/// nothing points at any more. Only a dead socket file is replaced. The
/// socket is created under a `0077` umask so it is never briefly
/// group- or world-connectable before a chmod; `run` is single-threaded
/// here, which is what makes touching the process umask safe.
fn listen(socket: &PathBuf) -> Result<UnixListener> {
    if UnixStream::connect(socket).is_ok() {
        anyhow::bail!(
            "another session kernel is already listening on {}",
            socket.display()
        );
    }
    let _ = fs::remove_file(socket);
    // SAFETY: umask has no preconditions; the previous mask is restored
    // below so the child inherits the daemon's original.
    let previous = unsafe { libc::umask(0o077) };
    let listener = UnixListener::bind(socket);
    unsafe {
        libc::umask(previous);
    }
    let listener = listener.with_context(|| format!("cannot listen on {}", socket.display()))?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Lifts the soft descriptor limit to the hard one. Every connection and
/// surface holds descriptors, so the default soft limit of a login shell is
/// the easiest way for a busy observer to push `accept` into `EMFILE`.
fn raise_open_file_limit() {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit/setrlimit read and write a struct we own.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return;
    }
    if limit.rlim_cur < limit.rlim_max {
        limit.rlim_cur = limit.rlim_max;
        unsafe {
            libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
        }
    }
}

fn accept_error_is_transient(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM | libc::ECONNABORTED)
    ) || error.kind() == ErrorKind::WouldBlock
}

static SOCKET_TO_UNLINK: Mutex<Option<std::ffi::CString>> = Mutex::new(None);
static RECORD_TO_UNLINK: Mutex<Option<std::ffi::CString>> = Mutex::new(None);
/// The child's pid while it is alive and unreaped; zero afterwards, so a
/// late signal handler never sends `SIGHUP` to whatever process the kernel
/// reissued that pid to.
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
    for path in [&SOCKET_TO_UNLINK, &RECORD_TO_UNLINK] {
        if let Ok(guard) = path.try_lock() {
            if let Some(path) = guard.as_ref() {
                unsafe {
                    libc::unlink(path.as_ptr());
                }
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
    if let Some(dir) = &shared.config.session_dir {
        let record = dir.join(KERNEL_RECORD);
        if let Ok(path) = std::ffi::CString::new(record.as_os_str().as_encoded_bytes()) {
            *RECORD_TO_UNLINK.lock().unwrap() = Some(path);
        }
    }
    // SAFETY: installing handlers with plain function pointers.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        for signal in [libc::SIGTERM, libc::SIGQUIT] {
            libc::signal(
                signal,
                on_terminate as extern "C" fn(libc::c_int) as libc::sighandler_t,
            );
        }
    }
}

/// Reaps launches whose creator died between daemon readiness and manifest
/// handoff, and daemons whose durable session directory was removed. Neither
/// condition may leave a child or socket alive indefinitely.
fn lifecycle_loop(shared: &Shared) {
    if let Some(marker) = &shared.config.launch_marker {
        let deadline =
            Instant::now() + Duration::from_millis(shared.config.launch_timeout_ms.max(1));
        while marker.exists() {
            if shared
                .config
                .session_dir
                .as_ref()
                .is_some_and(|dir| !dir.exists())
                || Instant::now() >= deadline
            {
                shutdown(shared);
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    let Some(dir) = &shared.config.session_dir else {
        return;
    };
    loop {
        if !dir.exists() {
            shutdown(shared);
        }
        thread::sleep(Duration::from_millis(250));
    }
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
        shared
            .bytes_from_child
            .fetch_add(n as u64, Ordering::Relaxed);
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
                shared.surface_queue_peak.fetch_max(
                    surface.queue.peak_bytes.load(Ordering::Relaxed),
                    Ordering::Relaxed,
                );
            }
            shared.parser.push(Item::Output(chunk.to_vec()));
        }
        shared.parser.wait_for_room();
        if let Some(id) = evict {
            shared.slow_client_evictions.fetch_add(1, Ordering::Relaxed);
            release_surface(shared, id, ReleaseReason::SlowClient);
        }
    }
    // The slave closed. Learn how the child ended *before* reaping it: while
    // it is a zombie its pid cannot be reissued, so recording the exit under
    // the session lock and only then reaping means no `signal` or `kill`
    // request can ever race a recycled pid. Anything that checks `exit`
    // under the lock sees it set before the pid is free.
    let (status, signal) = pty::wait_exit(shared.child_pid);
    let exit = Exit {
        status,
        signal,
        exited_at: unix_now(),
    };
    let holder = {
        let mut session = shared.session.lock().unwrap();
        session.exit = Some(exit.clone());
        session.activity = exit.exited_at;
        CHILD_TO_HUP.store(0, Ordering::Relaxed);
        let _ = pty::reap(shared.child_pid);
        session.surface.as_ref().map(|surface| surface.id)
    };
    if let Some(dir) = &shared.config.session_dir {
        let _ = paths::write_json(dir, EXIT_RECORD, &exit);
    }
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
    let mut size = Size::new(shared.config.cols, shared.config.rows);
    let mut alternate = false;
    let mut title: Option<String> = None;
    loop {
        match shared.parser.pop() {
            Item::Output(bytes) => {
                let len = bytes.len() as u64;
                // The screen model is off the live path, so a bug in it must
                // never take the session down: a panic on hostile output
                // costs the model, which is rebuilt empty at the current
                // size, and nothing else. The surface keeps receiving raw
                // bytes throughout.
                if catch_unwind(AssertUnwindSafe(|| terminal.advance(&bytes))).is_err() {
                    shared.parser_resets.fetch_add(1, Ordering::Relaxed);
                    terminal = Terminal::new(
                        TerminalConfig::new(size).with_scrollback_limit(SCROLLBACK_LINES),
                    );
                }
                shared.parser.consumed(len);
                let now_alternate = terminal.alternate_screen_active();
                if now_alternate != alternate {
                    alternate = now_alternate;
                    shared
                        .events
                        .broadcast(Event::AltScreen { active: alternate });
                }
                let now_title = render::sanitize_title_opt(terminal.title());
                if now_title != title {
                    title = now_title;
                    shared.events.broadcast(Event::TitleChanged {
                        title: title.clone(),
                    });
                }
            }
            Item::Resize(cols, rows) => {
                size = Size::new(cols, rows);
                terminal.resize(size);
            }
            Item::Query(query) => {
                if catch_unwind(AssertUnwindSafe(|| query(&mut terminal))).is_err() {
                    shared.parser_resets.fetch_add(1, Ordering::Relaxed);
                    terminal = Terminal::new(
                        TerminalConfig::new(size).with_scrollback_limit(SCROLLBACK_LINES),
                    );
                }
            }
        }
    }
}

/// Runs `query` on the parser thread at the current stream position and
/// waits for its answer.
///
/// A query that panics (or a parser thread that is gone) is an error for
/// this one request, not for the connection thread or the daemon.
fn parser_query<T: Send + 'static>(
    shared: &Shared,
    query: impl FnOnce(&mut Terminal) -> T + Send + 'static,
) -> Result<T> {
    let (sender, receiver) = mpsc::channel();
    shared.parser.push(Item::Query(Box::new(move |terminal| {
        let _ = sender.send(query(terminal));
    })));
    receiver
        .recv()
        .map_err(|_| anyhow::anyhow!("the screen model could not answer"))
}

fn parser_barrier(shared: &Shared) {
    let _ = parser_query(shared, |_| ());
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
        shared
            .bytes_to_surfaces
            .fetch_add(batch.len() as u64, Ordering::Relaxed);
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
    shared.surface_attaches.fetch_add(1, Ordering::Relaxed);
    shared.changed.notify_all();
    if let Some(previous) = previous {
        shared.surface_steals.fetch_add(1, Ordering::Relaxed);
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
            Ok(None) => return,
            Err(_) => {
                shared.control_failures.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };
        match request {
            Request::Attach {
                cols,
                rows,
                protocol,
            } => {
                if protocol != PROTOCOL_VERSION {
                    shared.control_failures.fetch_add(1, Ordering::Relaxed);
                    let _ = protocol::write_frame(
                        &mut stream,
                        &Response::err(format!(
                            "kernel speaks protocol {PROTOCOL_VERSION}, client speaks {protocol}"
                        )),
                    );
                    return;
                }
                attach(
                    shared,
                    stream,
                    cols.clamp(1, MAX_DIMENSION),
                    rows.clamp(1, MAX_DIMENSION),
                );
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
                    Err(error) => {
                        shared.control_failures.fetch_add(1, Ordering::Relaxed);
                        Response::err(error.to_string())
                    }
                };
                if protocol::write_frame(&mut stream, &response).is_err() {
                    return;
                }
            }
        }
    }
}

fn shutdown(shared: &Shared) -> ! {
    {
        // Check and signal under one lock: the reader records the exit and
        // reaps under the same lock, so the pid signalled here is always
        // still the child's.
        let session = shared.session.lock().unwrap();
        if session.exit.is_none() {
            let _ = pty::signal_group(shared.child_pid, libc::SIGHUP);
        }
    }
    let _ = fs::remove_file(&shared.config.socket);
    if let Some(dir) = &shared.config.session_dir {
        let _ = fs::remove_file(dir.join(KERNEL_RECORD));
    }
    std::process::exit(0)
}

fn handle(shared: &Arc<Shared>, request: Request) -> Result<Reply> {
    Ok(match request {
        Request::Stat => Reply {
            stat: Some(stat(shared)?),
            ..Reply::default()
        },
        Request::Write { bytes } => {
            require_running(shared)?;
            write_input(shared, &bytes)?;
            Reply::default()
        }
        Request::Key { keys } => {
            require_running(shared)?;
            let modes = parser_query(shared, |terminal| terminal.modes())?;
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
            write_input(shared, &paste_bytes(shared, &text)?)?;
            Reply::default()
        }
        Request::Submit { text } => {
            require_running(shared)?;
            let mut bytes = paste_bytes(shared, &text)?;
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
            })?;
            Reply {
                lines: Some(lines),
                dropped: Some(dropped),
                ..Reply::default()
            }
        }
        Request::Resize { cols, rows, pin } => {
            if !(1..=MAX_DIMENSION).contains(&cols) || !(1..=MAX_DIMENSION).contains(&rows) {
                anyhow::bail!("cols and rows must be between 1 and {MAX_DIMENSION}");
            }
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
            // Held across the check and the kill so the pid cannot be
            // reaped — and reissued — in between.
            let session = shared.session.lock().unwrap();
            if session.exit.is_some() {
                anyhow::bail!("session has exited");
            }
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

fn paste_bytes(shared: &Shared, text: &str) -> Result<Vec<u8>> {
    let bracketed = parser_query(shared, |terminal| terminal.modes().bracketed_paste)?;
    let mut bytes = Vec::with_capacity(text.len() + 12);
    if bracketed {
        bytes.extend_from_slice(b"\x1b[200~");
    }
    bytes.extend_from_slice(text.as_bytes());
    if bracketed {
        bytes.extend_from_slice(b"\x1b[201~");
    }
    Ok(bytes)
}

fn stat(shared: &Shared) -> Result<Stat> {
    let (alternate_screen, title) = parser_query(shared, |terminal| {
        (
            terminal.alternate_screen_active(),
            render::sanitize_title_opt(terminal.title()),
        )
    })?;
    let session = shared.session.lock().unwrap();
    let surface_queue_bytes = session
        .surface
        .as_ref()
        .map_or(0, |surface| surface.queue.len());
    Ok(Stat {
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
        bytes_from_child: shared.bytes_from_child.load(Ordering::Relaxed),
        bytes_to_surfaces: shared.bytes_to_surfaces.load(Ordering::Relaxed),
        parser_backlog_bytes: shared.parser.backlog_bytes.load(Ordering::Relaxed),
        parser_backlog_peak_bytes: shared.parser.peak_bytes.load(Ordering::Relaxed),
        surface_queue_bytes,
        surface_queue_peak_bytes: shared.surface_queue_peak.load(Ordering::Relaxed),
        surface_attaches: shared.surface_attaches.load(Ordering::Relaxed),
        surface_steals: shared.surface_steals.load(Ordering::Relaxed),
        slow_client_evictions: shared.slow_client_evictions.load(Ordering::Relaxed),
        control_failures: shared.control_failures.load(Ordering::Relaxed),
        parser_resets: shared.parser_resets.load(Ordering::Relaxed),
        subscriber_evictions: shared.events.evictions.load(Ordering::Relaxed),
    })
}

fn snapshot(shared: &Shared, format: SnapshotFormat, scrollback_lines: u32) -> Result<Reply> {
    parser_query(shared, move |terminal| {
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalled_event_subscriber_is_evicted_and_can_resubscribe() {
        let events = Events {
            subscribers: Mutex::new(Vec::new()),
            evictions: AtomicU64::new(0),
        };
        let stalled = events.subscribe();
        for ms in 0..=EVENT_QUEUE_CAP {
            events.broadcast(Event::OutputQuiet { ms: ms as u64 });
        }
        assert_eq!(events.evictions.load(Ordering::Relaxed), 1);
        assert!(events.subscribers.lock().unwrap().is_empty());
        drop(stalled);

        let recovered = events.subscribe();
        events.broadcast(Event::OutputQuiet { ms: 42 });
        assert_eq!(recovered.recv().unwrap(), Event::OutputQuiet { ms: 42 });
    }
}
