//! Own the private Codex server used by the terminal and relay its observation
//! notifications into the session's append-only conversation source.
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, ErrorKind, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tungstenite::{protocol::WebSocketConfig, Message, WebSocket};

use super::{write_private, PrivateWrite, FILE_MODE};
use crate::session::{
    manifest::LaunchManifest,
    paths::{LatchHome, SessionId, SessionPaths, SESSION_ID_ENV},
};

const MAX_RECORD: usize = 1024 * 1024;
const MAX_FRAME: usize = 8 * MAX_RECORD;
const IO_TIMEOUT: Duration = Duration::from_millis(250);
const SUBSCRIBE_INTERVAL: Duration = Duration::from_millis(350);
const MAX_SUBAGENTS: usize = 128;
type Socket = WebSocket<UnixStream>;

mod answers;
mod live_requests;
mod requests;
pub(crate) use answers::submit as submit_answer;
pub(crate) use requests::decode_presentation;

/// Prepares a fresh hosted Codex TUI to use its own app-server.
pub(crate) fn wrap_launch(paths: &SessionPaths, manifest: &mut LaunchManifest) -> Result<bool> {
    if crate::session::meta::launch_harness(&manifest.launch) != Some("codex")
        || manifest.launch.env.contains_key(super::CODEX_SOURCE_ENV)
        || server_config_flags(&manifest.launch.argv[1..]).is_none()
    {
        return Ok(false);
    }
    let executable = fs::canonicalize(std::env::current_exe()?)?;
    let dir = fs::canonicalize(paths.dir())?;
    manifest.launch.argv.splice(
        ..0,
        [
            executable.to_string_lossy().into_owned(),
            "__codex-session".to_owned(),
            dir.to_string_lossy().into_owned(),
            "--".to_owned(),
        ],
    );
    // The launcher already persisted the actual agent identity. The FIFO
    // now names a Latch wrapper, which cannot claim to be the Codex binary.
    manifest.launch.agent = None;
    Ok(true)
}

/// Fresh hosted TUIs use session config overrides and an optional model.
/// Leave alternate commands, resumes and explicitly remote clients on the
/// existing launch path, where their CLI argument semantics are preserved.
fn server_config_flags(args: &[String]) -> Option<Vec<String>> {
    let mut remaining = args;
    let mut config = Vec::new();
    while !remaining.is_empty() {
        let [flag, value, rest @ ..] = remaining else {
            return None;
        };
        let value = match flag.as_str() {
            "-c" | "--config" if value.contains('=') => value.clone(),
            "-m" | "--model" if !value.starts_with('-') && !value.is_empty() => {
                format!("model={}", serde_json::to_string(value).ok()?)
            }
            _ => return None,
        };
        config.extend(["-c".to_owned(), value]);
        remaining = rest;
    }
    Some(config)
}

/// Runs the session's Codex terminal and supervises its private app-server.
pub fn run(dir: &Path, mut argv: Vec<String>) -> Result<i32> {
    let (program, args) = argv.split_first().context("Codex command is missing")?;
    let config = server_config_flags(args).context("unsupported Codex server launch")?;
    let socket = dir.join("codex.sock");
    let address = format!("unix://{}", socket.display());
    // Both children inherit the session process group for kernel shutdown.
    // The server has no PTY streams and gets no external listener.
    let server = Command::new(program)
        .args(["app-server", "--listen", &address])
        .args(config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(server) = server {
        let mut server = Server(server);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && server.0.try_wait()?.is_none() {
            if let Ok(connection) = connect(&socket) {
                if let Ok(mut relay) = Relay::new(dir) {
                    let endpoint = answers::Endpoint::bind(&relay.home, &relay.session).ok();
                    let listener = endpoint
                        .as_ref()
                        .and_then(|endpoint| endpoint.listener.try_clone().ok());
                    let observer = thread::Builder::new().name("codex-activity".into());
                    if observer
                        .spawn(move || relay.observe(connection, &socket, listener.as_ref()))
                        .is_ok()
                    {
                        let exit = Command::new(program)
                            .args(["--remote", &address])
                            .args(args)
                            .status()
                            .context("launch remote Codex terminal")?;
                        return Ok(exit
                            .code()
                            .unwrap_or_else(|| 128 + exit.signal().unwrap_or(0)));
                    }
                }
                break;
            }
            thread::sleep(Duration::from_millis(40));
        }
    }
    // Older Codex versions and unavailable listeners keep the original TUI.
    super::install_codex_hook(&mut argv)?;
    Err(Command::new(&argv[0]).args(&argv[1..]).exec()).context("launch local Codex terminal")
}

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn connect(path: &Path) -> Result<Socket> {
    // Codex may expose a short socket via a symlink for Darwin's sockaddr_un limit.
    let path = fs::canonicalize(path)?;
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        bail!("Codex app-server socket is not private");
    }
    let stream = UnixStream::connect(path)?;
    latchd::peer::require_same_user(&stream)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let (mut socket, _) = tungstenite::client::client_with_config(
        "ws://localhost/",
        stream,
        Some(
            WebSocketConfig::default()
                .max_message_size(Some(MAX_FRAME))
                .max_frame_size(Some(MAX_FRAME)),
        ),
    )
    .map_err(|_| anyhow::anyhow!("Codex app-server handshake failed"))?;
    send(
        &mut socket,
        json!({"id": 1, "method":"initialize","params":{
            "clientInfo":{"name":"latch","title":"Latch","version":env!("CARGO_PKG_VERSION")},
            "capabilities":{"experimentalApi":true,"mcpServerOpenaiFormElicitation":true,"requiredHumanApproval":true}
        }}),
    )?;
    let deadline = Instant::now() + IO_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(value) = receive(&mut socket)? {
            if value["id"] == 1 && value.get("result").is_some() {
                send(&mut socket, json!({"method":"initialized"}))?;
                return Ok(socket);
            }
        }
    }
    bail!("Codex app-server did not initialize")
}

fn send(socket: &mut Socket, message: Value) -> Result<()> {
    socket.send(Message::Text(message.to_string().into()))?;
    Ok(())
}

fn receive(socket: &mut Socket) -> Result<Option<Value>> {
    match socket.read() {
        Ok(Message::Text(raw)) => Ok(serde_json::from_str(&raw).ok()),
        Ok(Message::Close(_)) => bail!("Codex app-server disconnected"),
        Ok(_) => Ok(None),
        Err(tungstenite::Error::Io(error))
            if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

struct Relay {
    home: LatchHome,
    session: SessionId,
    binding: PathBuf,
    journal_path: PathBuf,
    journal: File,
    root: Option<String>,
    children: HashMap<String, String>,
    child_offsets: HashMap<String, u64>,
    rollout: Option<PathBuf>,
    rollout_offset: u64,
    rollout_prefix_done: bool,
}

impl Relay {
    fn new(dir: &Path) -> Result<Self> {
        let root = dir
            .parent()
            .and_then(Path::parent)
            .context("missing Latch home")?;
        let home = LatchHome::new(root);
        let session = SessionId::parse(&std::env::var(SESSION_ID_ENV)?)?;
        if home.session(&session).dir() != dir {
            bail!("Codex launch directory does not match its session");
        }
        let journal_path = dir.join("codex-activity.jsonl");
        Ok(Self {
            home,
            session,
            binding: dir.join("conversation-source-binding.json"),
            journal: OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(FILE_MODE)
                .open(&journal_path)?,
            journal_path,
            root: None,
            children: HashMap::new(),
            child_offsets: HashMap::new(),
            rollout: None,
            rollout_offset: 0,
            rollout_prefix_done: false,
        })
    }

    fn observe(&mut self, mut socket: Socket, path: &Path, listener: Option<&UnixListener>) {
        loop {
            let Ok(mut requests) = live_requests::LiveRequests::new() else {
                return;
            };
            let _ = self.stream(&mut socket, listener, &mut requests);
            let _ = requests.disconnected(self);
            // A replacement connection has to subscribe again. Replay the
            // uncertain rollout suffix before admitting more live events.
            loop {
                let _ = self.catch_up_rollout();
                thread::sleep(SUBSCRIBE_INTERVAL);
                if let Ok(connection) = connect(path) {
                    socket = connection;
                    break;
                }
            }
        }
    }

    fn stream(
        &mut self,
        socket: &mut Socket,
        listener: Option<&UnixListener>,
        requests: &mut live_requests::LiveRequests,
    ) -> Result<()> {
        let mut next_id = 2u64;
        let mut pending: HashMap<u64, String> = HashMap::new();
        let mut subscribed = HashSet::new();
        let mut retry = Instant::now();
        let mut events_since_handoff = 0;
        loop {
            if Instant::now() >= retry {
                if self.root.is_none() {
                    send(
                        socket,
                        json!({"id":0,"method":"thread/loaded/list","params":{"limit":1}}),
                    )?;
                }
                let children = self.rollout_prefix_done.then_some(&self.children);
                for id in self
                    .root
                    .iter()
                    .chain(children.into_iter().flat_map(HashMap::keys))
                {
                    if !subscribed.contains(id) && !pending.values().any(|p| p == id) {
                        send(
                            socket,
                            json!({
                                "id":next_id,"method":"thread/resume",
                                "params":{"threadId":id,"excludeTurns":true}
                            }),
                        )?;
                        pending.insert(next_id, id.clone());
                        next_id += 1;
                    }
                }
                retry = Instant::now() + SUBSCRIBE_INTERVAL;
            }
            let Some(event) = receive(socket)? else {
                // Process queued Codex resolutions before checking for a
                // phone answer. Also give active streams a bounded cadence.
                requests.accept(listener, socket)?;
                events_since_handoff = 0;
                continue;
            };
            let method = event["method"].as_str();
            let params = &event["params"];
            let thread = params["threadId"]
                .as_str()
                .or_else(|| params["conversationId"].as_str());
            let known_thread = thread.is_some_and(|thread| {
                self.root.as_deref() == Some(thread) || self.children.contains_key(thread)
            });
            if method.is_some() && event.get("id").is_some() {
                if known_thread {
                    requests.open(self, &event, listener.is_some())?;
                }
            } else if method == Some("serverRequest/resolved") {
                requests.resolved(self, params)?;
            } else if let Some(id) = event.get("id").and_then(Value::as_u64) {
                if id == 0 && self.root.is_none() {
                    self.root = event
                        .pointer("/result/data/0")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
                if let Some(thread) = pending.remove(&id) {
                    if let Some(metadata) = event.pointer("/result/thread") {
                        subscribed.insert(thread.clone());
                        if self.root.as_ref() == Some(&thread) {
                            self.bootstrap(metadata, &thread)?;
                        } else if self.rollout_prefix_done {
                            self.bootstrap_child(metadata, &thread)?;
                        }
                    }
                }
            } else {
                if known_thread {
                    requests.activity(self, &event)?;
                }
                self.notification(event, &subscribed)?;
            }
            events_since_handoff += 1;
            if events_since_handoff == 32 {
                requests.accept(listener, socket)?;
                events_since_handoff = 0;
            }
        }
    }

    fn bootstrap(&mut self, metadata: &Value, thread: &str) -> Result<()> {
        if let Some(path) = metadata
            .get("path")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        {
            self.rollout = Some(path);
        }
        self.catch_up_rollout()?;
        if !self.rollout_prefix_done {
            if let Some(rollout) = &self.rollout {
                if !crate::cli::create::saved_chat::observe(
                    &self.home,
                    &self.session,
                    "codex",
                    Some(thread),
                    rollout,
                )? {
                    bail!("Codex thread does not match the saved chat");
                }
            }
            write_private(PrivateWrite {
                path: &self.binding,
                contents: &serde_json::to_vec(&json!({
                    "connector":"codex","source":self.journal_path,"agentSessionId":thread
                }))?,
                mode: FILE_MODE,
            })?;
            self.rollout_prefix_done = true;
        }
        self.append(json!({"method":"latch/codex/status","params":{
            "threadId":thread, "status":metadata["status"]
        }}))
    }

    fn bootstrap_child(&mut self, metadata: &Value, thread: &str) -> Result<()> {
        let Some(path) = metadata["path"]
            .as_str()
            .map(Path::new)
            .filter(|path| path.is_absolute())
        else {
            return Ok(());
        };
        let mut offset = self.child_offsets.get(thread).copied().unwrap_or(0);
        if offset == 0 {
            let parent = self
                .children
                .get(thread)
                .filter(|parent| self.children.contains_key(*parent));
            self.append(
                json!({"method":"thread/started","latchParentSubagentId":parent,
                    "params":{"thread":{
                        "id":thread,"parentThreadId":metadata["parentThreadId"],
                        "source":metadata["source"],"agentRole":metadata["agentRole"],
                        "agentNickname":metadata["agentNickname"]
                    }}
                }),
            )?;
        }
        // Spawn and subscribe are separate operations. Recover child items
        // that completed before the observer joined the child thread.
        let result = self.read_rollout(path, &mut offset, Some(thread));
        self.child_offsets.insert(thread.to_owned(), offset);
        result
    }

    fn notification(&mut self, mut event: Value, subscribed: &HashSet<String>) -> Result<()> {
        let method = event["method"].as_str().unwrap_or_default();
        if method == "thread/started" {
            let thread = &event["params"]["thread"];
            let Some(id) = thread["id"].as_str() else {
                return Ok(());
            };
            let parent = thread["parentThreadId"].as_str().or_else(|| {
                thread
                    .pointer("/source/subAgent/thread_spawn/parent_thread_id")
                    .and_then(Value::as_str)
            });
            if let Some(parent) = parent {
                if self.children.len() < MAX_SUBAGENTS
                    && (self.root.as_deref() == Some(parent) || self.children.contains_key(parent))
                {
                    self.children.insert(id.to_owned(), parent.to_owned());
                    if self.rollout_prefix_done {
                        let parent = self.children.contains_key(parent).then_some(parent);
                        event["latchParentSubagentId"] = json!(parent);
                        self.append(event)?;
                    }
                }
            } else if self.root.is_none() && thread.pointer("/source/subAgent").is_none() {
                self.root = Some(id.to_owned());
            }
            return Ok(());
        }
        if !self.rollout_prefix_done || !observed_method(method) {
            return Ok(());
        }
        let Some(id) = event["params"]["threadId"].as_str() else {
            return Ok(());
        };
        if self.root.as_deref() == Some(id) && subscribed.contains(id) {
            if method == "turn/started" {
                // The previous turn is now known live and persisted. Keep
                // this turn eligible for rollout recovery if the socket drops.
                if let Some(length) = self
                    .rollout
                    .as_ref()
                    .and_then(|path| fs::metadata(path).ok())
                    .map(|m| m.len())
                {
                    // A file length can point into a concurrently written line.
                    // Recover from the next complete line at or before it.
                    self.rollout_offset = self.rollout_offset.max(
                        self.rollout
                            .as_ref()
                            .and_then(|path| line_boundary(path, length).ok())
                            .unwrap_or(self.rollout_offset),
                    );
                }
            }
            let item = &event["params"]["item"];
            if item["type"] == "collabAgentToolCall" {
                for child in item["receiverThreadIds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    if self.children.len() >= MAX_SUBAGENTS {
                        break;
                    }
                    self.children
                        .entry(child.to_owned())
                        .or_insert_with(|| id.to_owned());
                }
            }
            self.append(event)?;
        } else if self.children.contains_key(id) {
            event["latchParentSubagentId"] = json!(id);
            self.append(event)?;
        }
        Ok(())
    }

    fn catch_up_rollout(&mut self) -> Result<()> {
        let Some(path) = self.rollout.clone() else {
            return Ok(());
        };
        let mut offset = self.rollout_offset;
        let result = self.read_rollout(&path, &mut offset, None);
        self.rollout_offset = offset;
        result
    }

    fn read_rollout(&mut self, path: &Path, offset: &mut u64, parent: Option<&str>) -> Result<()> {
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            bail!("Codex rollout is not a regular file");
        }
        let limit = metadata.len();
        let mut reader = BufReader::new(file);
        *offset = (*offset).min(limit);
        reader.seek(SeekFrom::Start(*offset))?;
        let mut position = *offset;
        let mut line = Vec::new();
        let mut oversized = false;
        while position < limit {
            let chunk = reader.fill_buf()?;
            let remaining = (limit - position).min(chunk.len() as u64) as usize;
            let chunk = &chunk[..remaining];
            if chunk.is_empty() {
                break;
            }
            let newline = chunk.iter().position(|byte| *byte == b'\n');
            let count = newline.map_or(chunk.len(), |index| index + 1);
            oversized |= line.len() + count > MAX_RECORD;
            if !oversized {
                line.extend_from_slice(&chunk[..count]);
            }
            reader.consume(count);
            position += count as u64;
            if newline.is_some() {
                if !oversized {
                    if let Ok(mut value) = serde_json::from_slice::<Value>(&line) {
                        if observed_rollout(&value) {
                            if let Some(parent) = parent {
                                value["latchParentSubagentId"] = json!(parent);
                            }
                            self.append(value)?;
                        }
                    }
                }
                oversized = false;
                line.clear();
                *offset = position;
            }
        }
        Ok(())
    }

    fn append(&mut self, mut value: Value) -> Result<()> {
        if value["type"] == "compacted" {
            if let Some(record) = value.as_object_mut() {
                record.remove("payload");
            }
        }
        if let Some(item) = value
            .pointer_mut("/payload/item")
            .and_then(Value::as_object_mut)
        {
            item.remove("raw_content");
        }
        if let Some(item) = value
            .pointer_mut("/params/item")
            .and_then(Value::as_object_mut)
        {
            // Payloads such as MCP results and image attachments can dwarf the
            // visible summary. The projection consumes only these fields.
            item.retain(|key, _| {
                matches!(
                    key.as_str(),
                    "type"
                        | "id"
                        | "text"
                        | "content"
                        | "summary"
                        | "plan"
                        | "command"
                        | "cwd"
                        | "exitCode"
                        | "aggregatedOutput"
                        | "status"
                        | "changes"
                        | "server"
                        | "tool"
                        | "success"
                        | "query"
                        | "path"
                        | "savedPath"
                        | "receiverThreadIds"
                        | "agentsStates"
                        | "agentThreadId"
                        | "agentPath"
                        | "kind"
                )
            });
            if item.get("type").is_some_and(|kind| kind != "userMessage") {
                item.remove("content");
            }
            if let Some(output) = item.get_mut("aggregatedOutput") {
                if let Some(text) = output.as_str() {
                    *output = json!(text.chars().take(1024).collect::<String>());
                }
            }
        }
        if let Some(turn) = value
            .pointer_mut("/params/turn")
            .and_then(Value::as_object_mut)
        {
            turn.remove("items");
        }
        if value.get("timestamp").is_none() {
            value["timestamp"] = json!(crate::engine::format_rfc3339(SystemTime::now()));
        }
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        if bytes.len() <= MAX_RECORD {
            self.write_record(&bytes)?;
        }
        Ok(())
    }

    fn write_record(&mut self, bytes: &[u8]) -> Result<()> {
        let start = self.journal.stream_position()?;
        if let Err(error) = self.journal.write_all(bytes) {
            self.journal.set_len(start)?;
            self.journal.seek(SeekFrom::Start(start))?;
            return Err(error.into());
        }
        Ok(())
    }
}

fn line_boundary(path: &Path, length: u64) -> std::io::Result<u64> {
    use std::io::Read;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "Codex rollout is not a regular file",
        ));
    }
    let start = length.saturating_sub(MAX_RECORD as u64);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(length - start).read_to_end(&mut bytes)?;
    Ok(bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| start + index as u64 + 1))
}

fn observed_rollout(value: &Value) -> bool {
    match value["type"].as_str() {
        Some("event_msg") => matches!(
            value["payload"]["type"].as_str(),
            Some(
                "item_completed"
                    | "task_started"
                    | "task_complete"
                    | "turn_aborted"
                    | "error"
                    | "model_reroute"
                    | "token_count"
                    | "plan_update"
                    | "context_compacted"
            )
        ),
        Some("compacted" | "token_usage_record") => true,
        _ => false,
    }
}

fn observed_method(method: &str) -> bool {
    matches!(
        method,
        "item/started"
            | "item/completed"
            | "item/fileChange/patchUpdated"
            | "turn/started"
            | "turn/completed"
            | "turn/plan/updated"
            | "thread/status/changed"
            | "thread/tokenUsage/updated"
            | "thread/compacted"
            | "model/rerouted"
            | "error"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::manifest::{self, AgentKind, LaunchRequest, TerminalSize};

    #[test]
    fn hosted_model_and_configuration_survive_the_private_launch_handoff() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().to_owned();
        let args = vec![
            "codex".to_owned(),
            "--model".to_owned(),
            "gpt-fixture".to_owned(),
            "-c".to_owned(),
            "model_reasoning_effort=\"high\"".to_owned(),
        ];
        let mut manifest = LaunchManifest::new(LaunchRequest {
            argv: args.clone(),
            cwd,
            size: TerminalSize::new(100, 30),
        });
        manifest.launch.agent = Some(AgentKind::Codex);
        let paths = SessionPaths::new(temp.path());
        assert!(wrap_launch(&paths, &mut manifest).unwrap());
        assert_eq!(manifest.launch.argv[4..], args);
        assert_eq!(
            server_config_flags(&args[1..]).unwrap(),
            [
                "-c",
                "model=\"gpt-fixture\"",
                "-c",
                "model_reasoning_effort=\"high\""
            ],
        );
        let mut fifo_contents = Vec::new();
        manifest::write(&mut fifo_contents, &manifest).unwrap();
        assert_eq!(manifest::read(&fifo_contents[..]).unwrap(), manifest);
    }
}
