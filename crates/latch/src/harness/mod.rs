//! Harness transcript discovery, normalization, and streaming.

mod generated;
mod interaction;
mod permission;

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context};
use serde::Serialize;
use serde_json::Value;

use crate::cli::manage;
use crate::engine::{self, SessionState};
use crate::session::manifest::LaunchManifest;
use crate::session::paths::{LatchHome, SessionId, SessionPaths, DIR_MODE, FILE_MODE};

pub use generated::{
    AwaitingInputKind, CanSend, HarnessEvent, HarnessEventPayload, InteractionCapabilities,
};
pub use interaction::{
    capabilities as interaction_capabilities, send, InteractionOptions, SendAction, SendInvalid,
    SendOptions, SendRefused, SendReport,
};

/// Derivation version for cursor compatibility.
pub const CONNECTOR_EPOCH: u32 = 1;
/// Poll interval used while tailing an append-only transcript.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Maximum hook payload accepted from Claude Code.
pub const MAX_HOOK_BYTES: usize = 1024 * 1024;

const CLAUDE_PLUGIN_NAME: &str = "latch-observer";

/// Whether a remote client should offer a chat view for this session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventsAvailability {
    /// True when a harness connector can produce events for this session.
    pub ok: bool,
    /// Why events are unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Hosted harness id, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// Connector epoch a stored cursor must match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connector_epoch: Option<u32>,
}

/// Named arguments for an event subscription.
#[derive(Debug, Clone)]
pub struct EventsOptions {
    /// Latch state root.
    pub home: LatchHome,
    /// Latch session id/name or Claude Code session id.
    pub session: String,
    /// Number of deterministic events already consumed.
    pub from: usize,
    /// Explicit transcript path, primarily for connector tests.
    pub transcript: Option<PathBuf>,
    /// Polling interval.
    pub poll_interval: Duration,
}

#[derive(Debug)]
struct Transcript {
    path: PathBuf,
    latch_id: Option<SessionId>,
}

#[derive(Clone, Copy)]
struct EventContext<'a> {
    session_id: &'a str,
    at: &'a str,
    harness_version: &'a str,
}

struct NormalizeRecord<'a> {
    record: &'a Value,
    context: EventContext<'a>,
    tools: &'a mut HashMap<String, String>,
    events: &'a mut Vec<HarnessEvent>,
}

struct NormalizeUser<'a> {
    record: &'a Value,
    context: EventContext<'a>,
    tools: &'a HashMap<String, String>,
    events: &'a mut Vec<HarnessEvent>,
}

struct NormalizeAssistant<'a> {
    record: &'a Value,
    context: EventContext<'a>,
    tools: &'a mut HashMap<String, String>,
    events: &'a mut Vec<HarnessEvent>,
}

struct PrivateWrite<'a> {
    path: &'a Path,
    contents: &'a [u8],
    mode: u32,
}

struct ReconcileLedger<'a> {
    transcript: &'a Path,
    paths: &'a SessionPaths,
}

struct HookCapture<'a> {
    home: &'a LatchHome,
    id: &'a SessionId,
    raw: &'a [u8],
}

struct StreamTranscript<'a, W: Write> {
    options: &'a EventsOptions,
    transcript: &'a Transcript,
    output: &'a mut W,
}

struct StreamLedger<'a, W: Write> {
    options: &'a EventsOptions,
    transcript: &'a Transcript,
    id: &'a SessionId,
    output: &'a mut W,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: SystemTime,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SourceStamps {
    transcript: Option<FileStamp>,
    hooks: Option<FileStamp>,
}

#[derive(Default)]
struct LedgerView {
    events: Vec<HarnessEvent>,
    counts: HashMap<String, usize>,
    offset: u64,
    lines: usize,
}

struct ReconcileIfChanged<'a> {
    request: ReconcileLedger<'a>,
    previous: &'a mut Option<SourceStamps>,
    view: &'a mut LedgerView,
}

struct HostedTranscript<'a> {
    projects: &'a Path,
    metadata: &'a crate::session::meta::SessionMeta,
}

/// Injects Latch's private observation plugin into a directly launched Claude
/// Code process.
pub fn prepare_claude_launch(
    home: &LatchHome,
    manifest: &mut LaunchManifest,
) -> anyhow::Result<()> {
    if crate::session::meta::harness_kind(&manifest.launch.argv) != Some("claude") {
        return Ok(());
    }
    let plugin = ensure_claude_plugin(home)?;
    if manifest
        .launch
        .argv
        .windows(2)
        .any(|pair| pair[0] == "--plugin-dir" && Path::new(&pair[1]) == plugin)
    {
        return Ok(());
    }
    manifest.launch.argv.splice(
        1..1,
        ["--plugin-dir".to_owned(), plugin.display().to_string()],
    );
    Ok(())
}

/// Captures one bounded Claude hook payload in the hosted session's private
/// raw sidecar.
pub fn capture_claude_hook(home: &LatchHome, reader: impl Read) -> anyhow::Result<()> {
    let raw = read_bounded_hook(reader)?;
    let latch_id = std::env::var(crate::session::paths::SESSION_ID_ENV)
        .context("Claude hook did not inherit LATCH_SESSION_ID")?;
    let id = SessionId::parse(&latch_id)?;
    capture_hook_record(HookCapture {
        home,
        id: &id,
        raw: &raw,
    })
}

fn capture_hook_record(request: HookCapture<'_>) -> anyhow::Result<()> {
    let HookCapture { home, id, raw } = request;
    let mut record: Value =
        serde_json::from_slice(raw).context("Claude hook payload is not JSON")?;
    let object = record
        .as_object_mut()
        .context("Claude hook payload must be an object")?;
    object
        .entry("timestamp")
        .or_insert_with(|| Value::String(engine::format_rfc3339(SystemTime::now())));
    object.entry("version").or_insert_with(|| {
        Value::String(std::env::var("CLAUDE_CODE_VERSION").unwrap_or_else(|_| "unknown".to_owned()))
    });

    let paths = home.session(id);
    if !paths.meta().is_file() {
        bail!("Claude hook belongs to unknown Latch session {id}");
    }
    let mut line = serde_json::to_vec(&record)?;
    line.push(b'\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(FILE_MODE)
        .open(paths.harness_hooks())
        .context("cannot append the Claude hook sidecar")?;
    lock_exclusive(&file)?;
    file.write_all(&line)?;
    file.flush()?;
    Ok(())
}

fn ensure_claude_plugin(home: &LatchHome) -> anyhow::Result<PathBuf> {
    let root = home
        .root()
        .join("harness")
        .join(format!("{CLAUDE_PLUGIN_NAME}-v{CONNECTOR_EPOCH}"));
    let metadata_dir = root.join(".claude-plugin");
    let hooks_dir = root.join("hooks");
    for directory in [&root, &metadata_dir, &hooks_dir] {
        fs::create_dir_all(directory)
            .with_context(|| format!("cannot create {}", directory.display()))?;
        fs::set_permissions(directory, fs::Permissions::from_mode(DIR_MODE))?;
    }

    let executable =
        fs::canonicalize(std::env::current_exe().context("cannot locate the latch executable")?)?;
    let command = format!("{} __harness-hook", shell_quote(&executable));
    let plugin = serde_json::to_vec_pretty(&serde_json::json!({
        "name": CLAUDE_PLUGIN_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Captures pending Claude input requests for Latch."
    }))?;
    let hooks = serde_json::to_vec_pretty(&serde_json::json!({
        "hooks": {
            "PermissionRequest": [{
                "matcher": ".*",
                "hooks": [{
                    "type": "command",
                    "command": command
                }]
            }]
        }
    }))?;
    write_private(PrivateWrite {
        path: &metadata_dir.join("plugin.json"),
        contents: &plugin,
        mode: FILE_MODE,
    })?;
    write_private(PrivateWrite {
        path: &hooks_dir.join("hooks.json"),
        contents: &hooks,
        mode: FILE_MODE,
    })?;
    Ok(root)
}

fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\"'\"'"))
}

fn write_private(request: PrivateWrite<'_>) -> anyhow::Result<()> {
    if fs::read(request.path).ok().as_deref() == Some(request.contents) {
        fs::set_permissions(request.path, fs::Permissions::from_mode(request.mode))?;
        return Ok(());
    }
    let parent = request
        .path
        .parent()
        .context("private file has no parent")?;
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = parent.join(format!(".tmp-{}-{nonce}", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(request.mode)
        .open(&temp)
        .with_context(|| format!("cannot write {}", temp.display()))?;
    file.write_all(request.contents)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temp, request.path)?;
    fs::set_permissions(request.path, fs::Permissions::from_mode(request.mode))?;
    Ok(())
}

fn read_bounded_hook(reader: impl Read) -> anyhow::Result<Vec<u8>> {
    let mut raw = Vec::new();
    reader
        .take((MAX_HOOK_BYTES + 1) as u64)
        .read_to_end(&mut raw)?;
    if raw.len() > MAX_HOOK_BYTES {
        bail!("Claude hook payload exceeds {MAX_HOOK_BYTES} bytes");
    }
    Ok(raw)
}

/// Reports whether `latch events` can be offered for a hosted session.
pub fn events_availability(options: InteractionOptions) -> anyhow::Result<EventsAvailability> {
    let id = manage::resolve_existing(&options.home, &options.session)?;
    let paths = options.home.session(&id);
    if !interaction::hosts_known_harness(&paths)? {
        return Ok(EventsAvailability {
            ok: false,
            reason: Some("no harness connector".to_owned()),
            harness: None,
            connector_epoch: None,
        });
    }
    let harness = match crate::session::meta::read(&paths) {
        Ok(metadata) => metadata
            .harness
            .or_else(|| (metadata.command_label == "claude").then(|| "claude".to_owned())),
        Err(_) => Some("claude".to_owned()),
    };
    Ok(EventsAvailability {
        ok: true,
        reason: None,
        harness,
        connector_epoch: Some(CONNECTOR_EPOCH),
    })
}

/// Streams newline-delimited normalized events until a hosted session exits.
pub fn stream(options: EventsOptions) -> anyhow::Result<()> {
    let stdout = io::stdout();
    let mut output = stdout.lock();
    stream_to(options, &mut output)
}

fn stream_to(options: EventsOptions, output: &mut impl Write) -> anyhow::Result<()> {
    let transcript = resolve_transcript(&options)?;
    if let Some(id) = transcript.latch_id.clone() {
        return stream_ledger(StreamLedger {
            options: &options,
            transcript: &transcript,
            id: &id,
            output,
        });
    }
    stream_transcript(StreamTranscript {
        options: &options,
        transcript: &transcript,
        output,
    })
}

fn stream_transcript<W: Write>(request: StreamTranscript<'_, W>) -> anyhow::Result<()> {
    let StreamTranscript {
        options,
        transcript,
        output,
    } = request;
    let mut emitted = options.from;
    let mut prior = Vec::new();
    let mut previous_stamp = None;

    loop {
        let stamp = file_stamp(&transcript.path)?
            .with_context(|| format!("cannot read {}", transcript.path.display()))?;
        if previous_stamp == Some(stamp) {
            thread::sleep(options.poll_interval);
            continue;
        }
        previous_stamp = Some(stamp);

        let raw = fs::read_to_string(&transcript.path)
            .with_context(|| format!("cannot read {}", transcript.path.display()))?;
        let complete = complete_jsonl(&raw);
        let events = parse_transcript(complete)?;
        if emitted > events.len() {
            bail!(
                "cursor {} is beyond the transcript's {} derived events",
                emitted,
                events.len()
            );
        }
        if !prior.is_empty() && (events.len() < prior.len() || events[..prior.len()] != prior[..]) {
            bail!(
                "Claude Code changed the active transcript branch; restart from cursor 0 for connector epoch {CONNECTOR_EPOCH}"
            );
        }
        for event in events.iter().skip(emitted) {
            serde_json::to_writer(&mut *output, event)?;
            output.write_all(b"\n")?;
            output.flush()?;
            emitted += 1;
        }
        prior = events;

        if transcript_has_ended(complete) {
            return Ok(());
        }
        thread::sleep(options.poll_interval);
    }
}

fn stream_ledger<W: Write>(request: StreamLedger<'_, W>) -> anyhow::Result<()> {
    let StreamLedger {
        options,
        transcript,
        id,
        output,
    } = request;
    let paths = options.home.session(id);
    let (lock, mut writer) = ledger_lock(&paths)?;
    let mut emitted = options.from;
    let mut checked_cursor = false;
    let mut previous = None;
    let mut view = LedgerView::default();

    loop {
        if writer {
            reconcile_if_changed(ReconcileIfChanged {
                request: ReconcileLedger {
                    transcript: &transcript.path,
                    paths: &paths,
                },
                previous: &mut previous,
                view: &mut view,
            })?;
        } else {
            writer = try_lock_exclusive(&lock)?;
            view.pull(&paths)?;
        }
        if !checked_cursor {
            if emitted > view.events.len() {
                bail!(
                    "cursor {} is beyond the session's {} persisted events",
                    emitted,
                    view.events.len()
                );
            }
            checked_cursor = true;
        }
        for event in view.events.iter().skip(emitted) {
            serde_json::to_writer(&mut *output, event)?;
            output.write_all(b"\n")?;
            output.flush()?;
            emitted += 1;
        }
        if engine::inspect(&options.home, id)?
            .is_none_or(|session| session.state != SessionState::Running)
        {
            return Ok(());
        }
        thread::sleep(options.poll_interval);
    }
}

fn ledger_lock(paths: &SessionPaths) -> anyhow::Result<(fs::File, bool)> {
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .mode(FILE_MODE)
        .open(paths.harness_events_lock())?;
    fs::set_permissions(
        paths.harness_events_lock(),
        fs::Permissions::from_mode(FILE_MODE),
    )?;
    let writer = try_lock_exclusive(&file)?;
    Ok((file, writer))
}

fn try_lock_exclusive(file: &fs::File) -> anyhow::Result<bool> {
    // SAFETY: `file` owns this descriptor for longer than the advisory lock.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::WouldBlock {
        Ok(false)
    } else {
        Err(error).context("cannot lock the harness event ledger")
    }
}

fn lock_exclusive(file: &fs::File) -> anyhow::Result<()> {
    // SAFETY: `file` remains alive for the duration of the write guarded by the
    // advisory lock.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()).context("cannot lock the Claude hook sidecar")
    }
}

#[cfg(test)]
fn reconcile_ledger(request: ReconcileLedger<'_>) -> anyhow::Result<()> {
    let mut previous = None;
    let mut view = LedgerView::default();
    reconcile_if_changed(ReconcileIfChanged {
        request,
        previous: &mut previous,
        view: &mut view,
    })?;
    Ok(())
}

fn reconcile_if_changed(request: ReconcileIfChanged<'_>) -> anyhow::Result<bool> {
    let ReconcileIfChanged {
        request,
        previous,
        view,
    } = request;
    let current = SourceStamps {
        transcript: file_stamp(request.transcript)?,
        hooks: file_stamp(&request.paths.harness_hooks())?,
    };
    if *previous == Some(current) {
        return Ok(false);
    }

    view.pull(request.paths)?;
    let transcript = fs::read_to_string(request.transcript)
        .with_context(|| format!("cannot read {}", request.transcript.display()))?;
    let mut candidates = parse_transcript(complete_jsonl(&transcript))?;
    let hooks_path = request.paths.harness_hooks();
    if hooks_path.is_file() {
        let hooks = fs::read_to_string(&hooks_path)
            .with_context(|| format!("cannot read {}", hooks_path.display()))?;
        candidates.extend(parse_transcript(complete_jsonl(&hooks))?);
    }
    candidates.sort_by(|left, right| left.at.cmp(&right.at));

    let mut candidate_counts = HashMap::<String, usize>::new();
    let mut additions = Vec::new();
    for event in candidates {
        let key = serde_json::to_string(&event)?;
        let occurrence = candidate_counts.entry(key.clone()).or_default();
        *occurrence += 1;
        if *occurrence > view.counts.get(&key).copied().unwrap_or_default() {
            additions.push(event);
        }
    }
    append_event_ledger(request.paths, &additions)?;
    view.pull(request.paths)?;
    *previous = Some(current);
    Ok(true)
}

fn file_stamp(path: &Path) -> anyhow::Result<Option<FileStamp>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(FileStamp {
            len: metadata.len(),
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        })),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("cannot stat {}", path.display())),
    }
}

impl LedgerView {
    fn pull(&mut self, paths: &SessionPaths) -> anyhow::Result<()> {
        let path = paths.harness_events();
        let mut file = match fs::File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("cannot read event ledger {}", path.display()));
            }
        };
        let len = file.metadata()?.len();
        if len < self.offset {
            self.events.clear();
            self.counts.clear();
            self.offset = 0;
            self.lines = 0;
        }
        if len == self.offset {
            return Ok(());
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut appended = String::new();
        file.read_to_string(&mut appended)
            .with_context(|| format!("cannot read event ledger {}", path.display()))?;
        let complete = complete_jsonl(&appended);
        let consumed = consumed_complete_jsonl_len(&appended);
        for line in complete.lines() {
            self.lines += 1;
            if line.trim().is_empty() {
                continue;
            }
            let Ok(event) = serde_json::from_str::<HarnessEvent>(line) else {
                // The ledger is a derived presentation cache. A process may
                // have been terminated while appending an older record; keep
                // valid later rows readable and let reconciliation restore the
                // missing event from the transcript or hook sidecar.
                continue;
            };
            *self
                .counts
                .entry(serde_json::to_string(&event)?)
                .or_default() += 1;
            self.events.push(event);
        }
        self.offset += consumed as u64;
        Ok(())
    }
}

#[cfg(test)]
fn read_event_ledger(paths: &SessionPaths) -> anyhow::Result<Vec<HarnessEvent>> {
    let mut view = LedgerView::default();
    view.pull(paths)?;
    Ok(view.events)
}

fn append_event_ledger(paths: &SessionPaths, events: &[HarnessEvent]) -> anyhow::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let path = paths.harness_events();
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .mode(FILE_MODE)
        .open(&path)
        .with_context(|| format!("cannot append event ledger {}", path.display()))?;
    truncate_incomplete_jsonl_tail(&mut file)
        .with_context(|| format!("cannot repair event ledger {}", path.display()))?;
    for event in events {
        let mut line = serde_json::to_vec(event)?;
        line.push(b'\n');
        file.write_all(&line)?;
    }
    file.flush()?;
    fs::set_permissions(path, fs::Permissions::from_mode(FILE_MODE))?;
    Ok(())
}

/// Drop an interrupted final JSONL record before a later append makes it look
/// complete. Event subscribers are intentionally short-lived and may be
/// terminated while reconciling; the exclusive ledger lock prevents another
/// writer from racing this repair.
fn truncate_incomplete_jsonl_tail(file: &mut fs::File) -> io::Result<()> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(());
    }

    file.seek(SeekFrom::End(-1))?;
    let mut last = [0_u8; 1];
    file.read_exact(&mut last)?;
    if last[0] == b'\n' {
        file.seek(SeekFrom::End(0))?;
        return Ok(());
    }

    let mut end = len;
    let mut buffer = [0_u8; 8 * 1024];
    while end > 0 {
        let start = end.saturating_sub(buffer.len() as u64);
        let width = (end - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer[..width])?;
        if let Some(index) = buffer[..width].iter().rposition(|byte| *byte == b'\n') {
            file.set_len(start + index as u64 + 1)?;
            file.seek(SeekFrom::End(0))?;
            return Ok(());
        }
        end = start;
    }

    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    Ok(())
}

fn complete_jsonl(raw: &str) -> &str {
    if raw.is_empty() || raw.ends_with('\n') {
        raw
    } else {
        raw.rsplit_once('\n').map_or("", |(complete, _)| complete)
    }
}

fn consumed_complete_jsonl_len(raw: &str) -> usize {
    if raw.is_empty() {
        0
    } else if raw.ends_with('\n') {
        raw.len()
    } else {
        raw.rfind('\n').map_or(0, |index| index + 1)
    }
}

/// Converts a complete Claude Code JSONL transcript into deterministic events.
pub fn parse_transcript(raw: &str) -> anyhow::Result<Vec<HarnessEvent>> {
    let records = parse_records(raw)?;
    let active = active_records(&records);
    let mut events = Vec::new();
    let mut tools = HashMap::<String, String>::new();

    for record in active {
        let session_id = string(record, &["sessionId", "session_id"]).unwrap_or("unknown");
        let at = string(record, &["timestamp", "at"]).unwrap_or("1970-01-01T00:00:00Z");
        let harness_version = string(record, &["version", "harnessVersion"]).unwrap_or("unknown");
        let context = EventContext {
            session_id,
            at,
            harness_version,
        };
        normalize_record(NormalizeRecord {
            record,
            context,
            tools: &mut tools,
            events: &mut events,
        });
    }
    Ok(events)
}

fn parse_records(raw: &str) -> anyhow::Result<Vec<Value>> {
    raw.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line)
                .with_context(|| format!("malformed Claude Code transcript line {}", index + 1))
        })
        .collect()
}

fn active_records(records: &[Value]) -> Vec<&Value> {
    let by_uuid = records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| string(record, &["uuid"]).map(|uuid| (uuid, index)))
        .collect::<HashMap<_, _>>();
    let Some((mut current, _)) = records
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, record)| {
            (!record
                .get("isSidechain")
                .and_then(Value::as_bool)
                .unwrap_or(false))
            .then(|| string(record, &["uuid"]).map(|uuid| (uuid, index)))
            .flatten()
        })
    else {
        return records.iter().collect();
    };

    let mut selected = HashSet::new();
    while let Some(index) = by_uuid.get(current).copied() {
        if !selected.insert(index) {
            break;
        }
        let Some(parent) = string(&records[index], &["parentUuid"]) else {
            break;
        };
        current = parent;
    }

    records
        .iter()
        .enumerate()
        .filter(|(index, record)| record.get("uuid").is_none() || selected.contains(index))
        .map(|(_, record)| record)
        .collect()
}

fn normalize_record(mut request: NormalizeRecord<'_>) {
    if let Some(derived) = permission::from_record(request.record) {
        request.events.push(event(
            request.context,
            HarnessEventPayload::AwaitingInput {
                request_id: derived.request_id,
                kind: derived.kind,
                prompt: derived.prompt,
                choices: derived.choices,
            },
        ));
        return;
    }
    if normalize_hook(&mut request) {
        return;
    }

    match string(request.record, &["type"]).unwrap_or_default() {
        "user" => normalize_user(NormalizeUser {
            record: request.record,
            context: request.context,
            tools: request.tools,
            events: request.events,
        }),
        "assistant" => normalize_assistant(NormalizeAssistant {
            record: request.record,
            context: request.context,
            tools: request.tools,
            events: request.events,
        }),
        "assistant_delta" | "content_block_delta" => {
            if let Some(text) = delta_text(request.record) {
                request.events.push(event(
                    request.context,
                    HarnessEventPayload::AssistantDelta { text },
                ));
            }
        }
        "system" => {
            if let Some(subtype) = string(request.record, &["subtype"]) {
                let status = match subtype {
                    "turn_duration" | "stop_hook_summary" => "idle",
                    other => other,
                };
                request.events.push(event(
                    request.context,
                    HarnessEventPayload::Status {
                        status: status.to_owned(),
                    },
                ));
            }
        }
        "result" => {
            let status = if request
                .record
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "failed"
            } else {
                "completed"
            };
            request.events.push(event(
                request.context,
                HarnessEventPayload::Status {
                    status: status.to_owned(),
                },
            ));
        }
        "status" => {
            if let Some(status) = string(request.record, &["status"]) {
                request.events.push(event(
                    request.context,
                    HarnessEventPayload::Status {
                        status: status.to_owned(),
                    },
                ));
            }
        }
        _ => {}
    }
}

fn normalize_hook(request: &mut NormalizeRecord<'_>) -> bool {
    match string(request.record, &["hook_event_name", "hookEventName"]) {
        Some("UserPromptSubmit") => {
            if let Some(prompt) = string(request.record, &["prompt"]) {
                request.events.push(event(
                    request.context,
                    HarnessEventPayload::UserMessage {
                        text: redact_secrets(prompt),
                    },
                ));
            }
            true
        }
        Some("PreToolUse") => {
            let tool = string(request.record, &["tool_name", "toolName"])
                .unwrap_or("unknown")
                .to_owned();
            let input = request
                .record
                .get("tool_input")
                .or_else(|| request.record.get("toolInput"))
                .map(|value| safe_tool_input(&tool, value))
                .unwrap_or(Value::Null);
            request.events.push(event(
                request.context,
                HarnessEventPayload::ToolStarted { tool, input },
            ));
            true
        }
        Some("PostToolUse") | Some("PostToolUseFailure") => {
            let tool = string(request.record, &["tool_name", "toolName"])
                .unwrap_or("unknown")
                .to_owned();
            let failed = string(request.record, &["hook_event_name", "hookEventName"])
                == Some("PostToolUseFailure");
            request.events.push(event(
                request.context,
                HarnessEventPayload::ToolFinished {
                    tool,
                    output: serde_json::json!({ "ok": !failed }),
                },
            ));
            true
        }
        Some("SessionStart") => {
            request.events.push(event(
                request.context,
                HarnessEventPayload::Status {
                    status: "running".to_owned(),
                },
            ));
            true
        }
        Some("Stop") | Some("SessionEnd") => {
            request.events.push(event(
                request.context,
                HarnessEventPayload::Status {
                    status: "idle".to_owned(),
                },
            ));
            true
        }
        Some(_) => true,
        None => false,
    }
}

fn normalize_user(request: NormalizeUser<'_>) {
    let Some(content) = request.record.pointer("/message/content") else {
        return;
    };
    if let Some(text) = content.as_str() {
        request.events.push(event(
            request.context,
            HarnessEventPayload::UserMessage {
                text: text.to_owned(),
            },
        ));
        return;
    }
    let Some(blocks) = content.as_array() else {
        return;
    };
    let text = blocks
        .iter()
        .filter(|block| string(block, &["type"]) == Some("text"))
        .filter_map(|block| string(block, &["text"]))
        .collect::<Vec<_>>()
        .join("\n");
    if !text.is_empty() {
        request.events.push(event(
            request.context,
            HarnessEventPayload::UserMessage { text },
        ));
    }
    for block in blocks {
        if string(block, &["type"]) != Some("tool_result") {
            continue;
        }
        let id = string(block, &["tool_use_id"]).unwrap_or("unknown");
        let tool = request
            .tools
            .get(id)
            .cloned()
            .unwrap_or_else(|| "unknown".to_owned());
        let output = serde_json::json!({ "available": block.get("content").is_some() });
        request.events.push(event(
            request.context,
            HarnessEventPayload::ToolFinished { tool, output },
        ));
    }
}

fn normalize_assistant(request: NormalizeAssistant<'_>) {
    let Some(blocks) = request
        .record
        .pointer("/message/content")
        .and_then(Value::as_array)
    else {
        return;
    };
    for block in blocks {
        match string(block, &["type"]).unwrap_or_default() {
            "text" => {
                if let Some(text) = string(block, &["text"]).filter(|text| !text.is_empty()) {
                    request.events.push(event(
                        request.context,
                        HarnessEventPayload::AssistantMessage {
                            text: text.to_owned(),
                        },
                    ));
                }
            }
            "tool_use" => {
                let id = string(block, &["id"]).unwrap_or("unknown").to_owned();
                let tool = string(block, &["name"]).unwrap_or("unknown").to_owned();
                let raw_input = block.get("input").cloned().unwrap_or(Value::Null);
                let input = safe_tool_input(&tool, &raw_input);
                request.tools.insert(id.clone(), tool.clone());
                request.events.push(event(
                    request.context,
                    HarnessEventPayload::ToolStarted {
                        tool: tool.clone(),
                        input: input.clone(),
                    },
                ));
                if tool == "AskUserQuestion" {
                    request.events.push(event(
                        request.context,
                        HarnessEventPayload::AwaitingInput {
                            request_id: id,
                            kind: AwaitingInputKind::Question,
                            prompt: permission::question_prompt(&raw_input),
                            choices: permission::question_choices(&raw_input),
                        },
                    ));
                }
            }
            _ => {}
        }
    }
}

fn safe_tool_input(tool: &str, input: &Value) -> Value {
    if tool == "AskUserQuestion" {
        return input.clone();
    }
    if tool == "Bash" {
        let program = input
            .get("command")
            .and_then(Value::as_str)
            .and_then(|command| command.split_whitespace().next())
            .map(redact_secrets);
        let description = input
            .get("description")
            .and_then(Value::as_str)
            .map(redact_secrets);
        return serde_json::json!({
            "program": program,
            "description": description
        });
    }
    let Some(object) = input.as_object() else {
        return Value::Null;
    };
    let allowed = ["file_path", "path", "pattern", "query", "description"];
    Value::Object(
        object
            .iter()
            .filter(|(key, _)| allowed.contains(&key.as_str()))
            .map(|(key, value)| {
                let safe = value
                    .as_str()
                    .map(redact_secrets)
                    .map(Value::String)
                    .unwrap_or(Value::Null);
                (key.clone(), safe)
            })
            .collect(),
    )
}

fn redact_secrets(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            let token = word.trim_matches(|character: char| {
                !character.is_ascii_alphanumeric() && character != '_'
            });
            if (token.starts_with("out_") || token.starts_with("sk_") || token.starts_with("AKIA"))
                && token.len() >= 12
            {
                word.replace(token, "[redacted]")
            } else {
                word.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn event(context: EventContext<'_>, payload: HarnessEventPayload) -> HarnessEvent {
    HarnessEvent {
        session_id: context.session_id.to_owned(),
        at: context.at.to_owned(),
        harness_version: context.harness_version.to_owned(),
        connector_epoch: CONNECTOR_EPOCH,
        payload,
    }
}

fn delta_text(record: &Value) -> Option<String> {
    record
        .pointer("/delta/text")
        .or_else(|| record.get("text"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
}

fn transcript_has_ended(raw: &str) -> bool {
    raw.lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .is_some_and(|record| string(&record, &["type"]) == Some("result"))
        })
}

fn resolve_transcript(options: &EventsOptions) -> anyhow::Result<Transcript> {
    let latch_id = manage::resolve_existing(&options.home, &options.session).ok();
    if let Some(path) = &options.transcript {
        return Ok(Transcript {
            path: path.clone(),
            latch_id,
        });
    }

    let config = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude")))
        .context("cannot locate Claude Code configuration; set CLAUDE_CONFIG_DIR")?;
    let projects = config.join("projects");

    if let Some(id) = &latch_id {
        let metadata = crate::session::meta::read(&options.home.session(id))?;
        let path = hosted_transcript(HostedTranscript {
            projects: &projects,
            metadata: &metadata,
        })?
        .with_context(|| {
            format!(
                "no Claude Code transcript found for Latch session {} under {}",
                options.session,
                projects.display()
            )
        })?;
        return Ok(Transcript { path, latch_id });
    }

    let path = transcript_by_session_id(&projects, &options.session)?.with_context(|| {
        format!(
            "no Claude Code transcript found for session {} under {}",
            options.session,
            projects.display()
        )
    })?;
    Ok(Transcript {
        path,
        latch_id: None,
    })
}

fn encode_project_path(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

fn hosted_transcript(request: HostedTranscript<'_>) -> anyhow::Result<Option<PathBuf>> {
    let encoded = request
        .projects
        .join(encode_project_path(&request.metadata.cwd));
    if let Some(external_id) = &request.metadata.source.external_run_id {
        let exact = encoded.join(format!("{external_id}.jsonl"));
        if exact.is_file() {
            return Ok(Some(exact));
        }
        if let Some(path) = transcript_by_session_id(request.projects, external_id)? {
            return Ok(Some(path));
        }
    }
    if encoded.is_dir() {
        return newest_jsonl(&encoded);
    }
    Ok(None)
}

fn newest_jsonl(directory: &Path) -> anyhow::Result<Option<PathBuf>> {
    let mut candidates = fs::read_dir(directory)
        .with_context(|| format!("cannot read {}", directory.display()))?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "jsonl")
        })
        .map(|entry| {
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (modified, entry.path())
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
    Ok(candidates.into_iter().next().map(|(_, path)| path))
}

fn transcript_by_session_id(projects: &Path, session_id: &str) -> anyhow::Result<Option<PathBuf>> {
    let filename = format!("{session_id}.jsonl");
    let entries = match fs::read_dir(projects) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", projects.display()));
        }
    };
    for project in entries.filter_map(Result::ok) {
        let candidate = project.path().join(&filename);
        if candidate.is_file() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str, file: &str) -> String {
        fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/harness/claude-code")
                .join(name)
                .join(file),
        )
        .unwrap()
    }

    fn overlord_payload(name: &str) -> String {
        let document: Value =
            serde_json::from_str(&fixture("overlord", &format!("{name}.json"))).unwrap();
        let mut payload = document["payload"].clone();
        let object = payload.as_object_mut().unwrap();
        object.insert(
            "timestamp".to_owned(),
            document
                .get("occurredAt")
                .cloned()
                .unwrap_or_else(|| Value::String("2026-01-01T00:00:00.000Z".to_owned())),
        );
        object.insert("version".to_owned(), Value::String("fixture".to_owned()));
        format!("{}\n", serde_json::to_string(&payload).unwrap())
    }

    #[test]
    fn conversation_fixture_matches() {
        let actual = parse_transcript(&fixture("conversation", "raw.jsonl")).unwrap();
        let expected = fixture("conversation", "expected.ndjson")
            .lines()
            .map(|line| serde_json::from_str::<HarnessEvent>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn live_claude_2_1_228_fixture_matches() {
        let actual = parse_transcript(&fixture("live-2.1.228", "raw.jsonl")).unwrap();
        let expected = fixture("live-2.1.228", "expected.ndjson")
            .lines()
            .map(|line| serde_json::from_str::<HarnessEvent>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn live_permission_2_1_228_fixture_matches() {
        let actual = parse_transcript(&fixture("live-permission-2.1.228", "raw.jsonl")).unwrap();
        let expected = fixture("live-permission-2.1.228", "expected.ndjson")
            .lines()
            .map(|line| serde_json::from_str::<HarnessEvent>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn awaiting_question_fixture_matches() {
        let actual = parse_transcript(&fixture("awaiting-question", "raw.jsonl")).unwrap();
        let expected = fixture("awaiting-question", "expected.ndjson")
            .lines()
            .map(|line| serde_json::from_str::<HarnessEvent>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn permission_request_fixture_matches() {
        let actual = parse_transcript(&fixture("permission-request", "raw.jsonl")).unwrap();
        let expected = fixture("permission-request", "expected.ndjson")
            .lines()
            .map(|line| serde_json::from_str::<HarnessEvent>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn imported_overlord_permission_fixture_is_supported() {
        let events = parse_transcript(&overlord_payload("permission-request")).unwrap();
        assert!(matches!(
            &events[0].payload,
            HarnessEventPayload::AwaitingInput {
                request_id,
                kind: AwaitingInputKind::Permission,
                ..
            } if request_id == "permission-1"
        ));
    }

    #[test]
    fn imported_overlord_prompt_fixture_redacts_connector_tokens() {
        let events = parse_transcript(&overlord_payload("normalize-user-prompt-submit")).unwrap();
        let serialized = serde_json::to_string(&events).unwrap();
        assert!(!serialized.contains("out_abcdefgh12345678"));
        assert!(!serialized.contains(".jsonl"));
        assert!(!serialized.contains("/Users/jake"));
        assert!(serialized.contains("[redacted]"));
    }

    #[test]
    fn imported_overlord_tool_fixture_does_not_emit_raw_output() {
        let events = parse_transcript(&overlord_payload("normalize-post-tool-use-shell")).unwrap();
        let serialized = serde_json::to_string(&events).unwrap();
        assert!(matches!(
            &events[0].payload,
            HarnessEventPayload::ToolFinished { tool, .. } if tool == "Bash"
        ));
        for forbidden in [
            "transcript",
            ".jsonl",
            "PASS 412 tests",
            "hunter2",
            "/Users/jake",
        ] {
            assert!(!serialized.contains(forbidden));
        }
    }

    #[test]
    fn parent_chain_excludes_abandoned_branch() {
        let raw = concat!(
            "{\"uuid\":\"one\",\"parentUuid\":null,\"type\":\"user\",\"sessionId\":\"s\",\"timestamp\":\"2026-08-13T10:00:00Z\",\"version\":\"2.1\",\"message\":{\"content\":\"one\"}}\n",
            "{\"uuid\":\"old\",\"parentUuid\":\"one\",\"isSidechain\":true,\"type\":\"assistant\",\"sessionId\":\"s\",\"timestamp\":\"2026-08-13T10:00:01Z\",\"version\":\"2.1\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"old\"}]}}\n",
            "{\"uuid\":\"new\",\"parentUuid\":\"one\",\"type\":\"assistant\",\"sessionId\":\"s\",\"timestamp\":\"2026-08-13T10:00:02Z\",\"version\":\"2.1\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"new\"}]}}\n"
        );
        let events = parse_transcript(raw).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            &events[1].payload,
            HarnessEventPayload::AssistantMessage { text } if text == "new"
        ));
    }

    #[test]
    fn claude_launch_gets_a_private_latch_plugin() {
        let directory = tempfile::tempdir().unwrap();
        let home = LatchHome::new(directory.path().join("latch"));
        home.ensure().unwrap();
        let mut manifest = LaunchManifest::new(crate::session::manifest::LaunchRequest {
            argv: vec!["/usr/local/bin/claude".to_owned(), "-p".to_owned()],
            cwd: directory.path().to_owned(),
            size: crate::session::manifest::TerminalSize::new(80, 24),
        });
        prepare_claude_launch(&home, &mut manifest).unwrap();
        assert_eq!(manifest.launch.argv[1], "--plugin-dir");
        let plugin = PathBuf::from(&manifest.launch.argv[2]);
        assert!(plugin.starts_with(home.root()));
        let hooks = fs::read_to_string(plugin.join("hooks/hooks.json")).unwrap();
        assert!(hooks.contains("PermissionRequest"));
        assert!(hooks.contains("__harness-hook"));
        assert_eq!(
            fs::metadata(&plugin).unwrap().permissions().mode() & 0o777,
            DIR_MODE
        );
        assert_eq!(
            fs::metadata(plugin.join("hooks/hooks.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            FILE_MODE
        );
        assert_eq!(
            manifest
                .launch
                .argv
                .windows(2)
                .filter(|pair| pair[0] == "--plugin-dir")
                .count(),
            1
        );
    }

    #[test]
    fn hook_capture_and_ledger_reconciliation_are_append_only() {
        let directory = tempfile::tempdir().unwrap();
        let home = LatchHome::new(directory.path().join("latch"));
        home.ensure().unwrap();
        let id = SessionId::parse("ses_fixture").unwrap();
        let paths = home.session(&id);
        paths.ensure().unwrap();
        fs::write(paths.meta(), "{}").unwrap();
        capture_hook_record(HookCapture {
            home: &home,
            id: &id,
            raw: fixture("permission-request", "raw.jsonl").as_bytes(),
        })
        .unwrap();

        let transcript = directory.path().join("transcript.jsonl");
        fs::write(&transcript, fixture("conversation", "raw.jsonl")).unwrap();
        let request = || ReconcileLedger {
            transcript: &transcript,
            paths: &paths,
        };
        reconcile_ledger(request()).unwrap();
        reconcile_ledger(request()).unwrap();
        let mut events = read_event_ledger(&paths).unwrap();
        assert_eq!(events.len(), 7);
        assert!(matches!(
            &events[6].payload,
            HarnessEventPayload::AwaitingInput {
                request_id,
                kind: AwaitingInputKind::Permission,
                ..
            } if request_id == "permission-1"
        ));
        let ledger = fs::read_to_string(paths.harness_events()).unwrap();
        assert_eq!(ledger.lines().count(), 7);
        let mut transcript_file = OpenOptions::new().append(true).open(&transcript).unwrap();
        transcript_file
            .write_all(
                b"{\"parentUuid\":\"system-1\",\"type\":\"assistant\",\"sessionId\":\"claude-session-1\",\"version\":\"2.1.119\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"late transcript write\"}]},\"uuid\":\"assistant-late\",\"timestamp\":\"2026-08-13T10:00:05Z\"}\n",
            )
            .unwrap();
        reconcile_ledger(request()).unwrap();
        events = read_event_ledger(&paths).unwrap();
        assert_eq!(events.len(), 8);
        assert!(matches!(
            &events[6].payload,
            HarnessEventPayload::AwaitingInput { .. }
        ));
        assert!(matches!(
            &events[7].payload,
            HarnessEventPayload::AssistantMessage { text } if text == "late transcript write"
        ));
        assert_eq!(
            fs::metadata(paths.harness_hooks())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            FILE_MODE
        );
        assert_eq!(
            fs::metadata(paths.harness_events())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            FILE_MODE
        );
    }

    #[test]
    fn ledger_append_repairs_an_interrupted_final_record() {
        let directory = tempfile::tempdir().unwrap();
        let home = LatchHome::new(directory.path().join("latch"));
        home.ensure().unwrap();
        let paths = fixture_session(&home);
        fs::write(
            paths.harness_events(),
            b"{\"sessionId\":\"old\",\"at\":\"2026-08-13T10:00:00Z\",\"harnessVersion\":\"2.1\",\"connectorEpoch\":1,\"type\":\"status\",\"status\":\"running\"}\n{\"sessionId\":\"interrupted\"",
        )
        .unwrap();
        let event = HarnessEvent {
            session_id: "new".to_owned(),
            at: "2026-08-13T10:00:01Z".to_owned(),
            harness_version: "2.1".to_owned(),
            connector_epoch: CONNECTOR_EPOCH,
            payload: HarnessEventPayload::Status {
                status: "completed".to_owned(),
            },
        };

        append_event_ledger(&paths, &[event.clone()]).unwrap();

        let events = read_event_ledger(&paths).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1], event);
        let raw = fs::read_to_string(paths.harness_events()).unwrap();
        assert!(!raw.contains("interrupted"));
        assert!(raw.ends_with('\n'));
    }

    #[test]
    fn ledger_read_skips_an_existing_malformed_record() {
        let directory = tempfile::tempdir().unwrap();
        let home = LatchHome::new(directory.path().join("latch"));
        home.ensure().unwrap();
        let paths = fixture_session(&home);
        let first = HarnessEvent {
            session_id: "first".to_owned(),
            at: "2026-08-13T10:00:00Z".to_owned(),
            harness_version: "2.1".to_owned(),
            connector_epoch: CONNECTOR_EPOCH,
            payload: HarnessEventPayload::Status {
                status: "running".to_owned(),
            },
        };
        let second = HarnessEvent {
            session_id: "second".to_owned(),
            at: "2026-08-13T10:00:01Z".to_owned(),
            harness_version: "2.1".to_owned(),
            connector_epoch: CONNECTOR_EPOCH,
            payload: HarnessEventPayload::Status {
                status: "completed".to_owned(),
            },
        };
        let raw = format!(
            "{}\n{{\"sessionId\":\"interrupted\"\n{}\n",
            serde_json::to_string(&first).unwrap(),
            serde_json::to_string(&second).unwrap()
        );
        fs::write(paths.harness_events(), raw).unwrap();

        let events = read_event_ledger(&paths).unwrap();

        assert_eq!(events, vec![first, second]);
    }

    #[test]
    fn subscription_emits_records_appended_after_it_starts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("live.jsonl");
        fs::write(
            &path,
            "{\"uuid\":\"one\",\"parentUuid\":null,\"type\":\"user\",\"sessionId\":\"live\",\"timestamp\":\"2026-08-13T10:00:00Z\",\"version\":\"2.1\",\"message\":{\"content\":\"hello\"}}\n",
        )
        .unwrap();
        let append_path = path.clone();
        let appender = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            let mut file = fs::OpenOptions::new()
                .append(true)
                .open(append_path)
                .unwrap();
            file.write_all(
                b"{\"type\":\"result\",\"sessionId\":\"live\",\"timestamp\":\"2026-08-13T10:00:01Z\",\"version\":\"2.1\",\"is_error\":false}\n",
            )
            .unwrap();
        });
        let mut output = Vec::new();
        stream_to(
            EventsOptions {
                home: LatchHome::new(directory.path().join("latch")),
                session: "live".to_owned(),
                from: 0,
                transcript: Some(path),
                poll_interval: Duration::from_millis(5),
            },
            &mut output,
        )
        .unwrap();
        appender.join().unwrap();
        let events = String::from_utf8(output).unwrap();
        assert!(events.contains("\"type\":\"user_message\""));
        assert!(events.contains("\"type\":\"status\",\"status\":\"completed\""));
    }

    fn fixture_session(home: &LatchHome) -> SessionPaths {
        let id = SessionId::parse("ses_fixture").unwrap();
        let paths = home.session(&id);
        paths.ensure().unwrap();
        paths
    }

    fn sample_meta(cwd: &Path, external_id: Option<&str>) -> crate::session::meta::SessionMeta {
        let launch = crate::session::manifest::LaunchSpec {
            argv: vec!["claude".to_owned()],
            cwd: cwd.to_owned(),
            env: Default::default(),
            inherit_env: true,
            size: crate::session::manifest::TerminalSize::new(80, 24),
            term: "xterm-256color".to_owned(),
        };
        let mut display = crate::session::manifest::DisplayMetadata::default();
        display.source.external_run_id = external_id.map(str::to_owned);
        crate::session::meta::derive(crate::session::meta::MetaRequest {
            id: "ses_fixture",
            launch: &launch,
            display: &display,
            created_at: "2026-08-13T00:00:00Z",
        })
    }

    fn poll_reconcile(request: ReconcileIfChanged<'_>) -> bool {
        reconcile_if_changed(request).unwrap()
    }

    #[test]
    fn unchanged_sources_skip_reparse() {
        let directory = tempfile::tempdir().unwrap();
        let home = LatchHome::new(directory.path().join("latch"));
        home.ensure().unwrap();
        let paths = fixture_session(&home);
        fs::write(paths.meta(), "{}").unwrap();
        capture_hook_record(HookCapture {
            home: &home,
            id: &SessionId::parse("ses_fixture").unwrap(),
            raw: fixture("permission-request", "raw.jsonl").as_bytes(),
        })
        .unwrap();
        let transcript = directory.path().join("transcript.jsonl");
        fs::write(&transcript, fixture("conversation", "raw.jsonl")).unwrap();

        let mut previous = None;
        let mut view = LedgerView::default();
        assert!(poll_reconcile(ReconcileIfChanged {
            request: ReconcileLedger {
                transcript: &transcript,
                paths: &paths,
            },
            previous: &mut previous,
            view: &mut view,
        }));
        let events_after_parse = view.events.len();
        let offset_after_parse = view.offset;
        assert!(events_after_parse > 0);
        assert!(!poll_reconcile(ReconcileIfChanged {
            request: ReconcileLedger {
                transcript: &transcript,
                paths: &paths,
            },
            previous: &mut previous,
            view: &mut view,
        }));
        assert_eq!(view.events.len(), events_after_parse);
        assert_eq!(view.offset, offset_after_parse);
    }

    #[test]
    fn large_transcript_reconcile_stays_incremental() {
        let directory = tempfile::tempdir().unwrap();
        let home = LatchHome::new(directory.path().join("latch"));
        home.ensure().unwrap();
        let paths = fixture_session(&home);
        fs::write(paths.meta(), "{}").unwrap();

        let mut raw = String::new();
        for index in 0..2_000 {
            raw.push_str(&format!(
                "{{\"uuid\":\"u{index}\",\"parentUuid\":{},\"type\":\"user\",\"sessionId\":\"large\",\"timestamp\":\"2026-08-13T10:00:00.{index:06}Z\",\"version\":\"2.1\",\"message\":{{\"content\":\"m{index}\"}}}}\n",
                if index == 0 {
                    "null".to_owned()
                } else {
                    format!("\"u{}\"", index - 1)
                }
            ));
        }
        let transcript = directory.path().join("transcript.jsonl");
        fs::write(&transcript, &raw).unwrap();

        let mut previous = None;
        let mut view = LedgerView::default();
        assert!(poll_reconcile(ReconcileIfChanged {
            request: ReconcileLedger {
                transcript: &transcript,
                paths: &paths,
            },
            previous: &mut previous,
            view: &mut view,
        }));
        assert_eq!(view.events.len(), 2_000);
        let offset = view.offset;
        assert!(!poll_reconcile(ReconcileIfChanged {
            request: ReconcileLedger {
                transcript: &transcript,
                paths: &paths,
            },
            previous: &mut previous,
            view: &mut view,
        }));
        assert_eq!(view.offset, offset);

        let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
        file.write_all(
            b"{\"uuid\":\"u2000\",\"parentUuid\":\"u1999\",\"type\":\"user\",\"sessionId\":\"large\",\"timestamp\":\"2026-08-13T10:00:01.000000Z\",\"version\":\"2.1\",\"message\":{\"content\":\"tail\"}}\n",
        )
        .unwrap();
        drop(file);
        assert!(poll_reconcile(ReconcileIfChanged {
            request: ReconcileLedger {
                transcript: &transcript,
                paths: &paths,
            },
            previous: &mut previous,
            view: &mut view,
        }));
        assert_eq!(view.events.len(), 2_001);
        assert!(matches!(
            &view.events[2_000].payload,
            HarnessEventPayload::UserMessage { text } if text == "tail"
        ));
    }

    #[test]
    fn encode_project_path_matches_claude_code() {
        assert_eq!(
            encode_project_path(Path::new("/Users/jake/dev/my.app")),
            "-Users-jake-dev-my-app"
        );
        assert_eq!(
            encode_project_path(Path::new("/tmp/work_dir")),
            "-tmp-work-dir"
        );
        assert_eq!(
            encode_project_path(Path::new("/Users/jake/Development/Cooperativ/Latch")),
            "-Users-jake-Development-Cooperativ-Latch"
        );
    }

    #[test]
    fn hosted_transcript_uses_claude_encoding_and_scans_by_session_id() {
        let directory = tempfile::tempdir().unwrap();
        let projects = directory.path().join("projects");
        let encoded = projects.join("-Users-jake-dev-my-app");
        fs::create_dir_all(&encoded).unwrap();
        let transcript = encoded.join("claude-dotted-1.jsonl");
        fs::write(&transcript, "{}\n").unwrap();
        let metadata = sample_meta(Path::new("/Users/jake/dev/my.app"), Some("claude-dotted-1"));
        assert_eq!(
            hosted_transcript(HostedTranscript {
                projects: &projects,
                metadata: &metadata,
            })
            .unwrap()
            .as_deref(),
            Some(transcript.as_path())
        );

        let misplaced = projects.join("-wrong-encoding-with-dot");
        fs::create_dir_all(&misplaced).unwrap();
        let scanned = misplaced.join("claude-scan-1.jsonl");
        fs::write(&scanned, "{}\n").unwrap();
        let metadata = sample_meta(Path::new("/Users/jake/dev/my.app"), Some("claude-scan-1"));
        assert_eq!(
            hosted_transcript(HostedTranscript {
                projects: &projects,
                metadata: &metadata,
            })
            .unwrap()
            .as_deref(),
            Some(scanned.as_path())
        );
    }

    #[test]
    fn events_and_resolve_share_fallback_permission_ids() {
        let raw = "{\"hook_event_name\":\"PermissionRequest\",\"tool_name\":\"Bash\",\"tool_input\":{\"description\":\"Run tests\"}}\n";
        let record: Value = serde_json::from_str(raw.trim()).unwrap();
        let events = parse_transcript(raw).unwrap();
        let derived = super::permission::from_record(&record).unwrap();
        match &events[0].payload {
            HarnessEventPayload::AwaitingInput {
                request_id,
                prompt,
                choices,
                ..
            } => {
                assert_eq!(request_id, &derived.request_id);
                assert_eq!(prompt, &derived.prompt);
                assert_eq!(choices, &derived.choices);
            }
            other => panic!("unexpected payload {other:?}"),
        }
        assert_eq!(derived.request_id, "permission:Bash:1970-01-01T00:00:00Z");
    }
}
