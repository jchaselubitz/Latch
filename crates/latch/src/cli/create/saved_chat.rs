//! Durable, device-owned chat identity and resume generation journal.
//! Only the launch observer can establish a native binding. The gateway never
//! takes a native ID, path, or executable from the caller.
use super::*;
use crate::cli::serve::{SavedChatStatus, SavedChatTarget};
use std::fs;
use std::os::unix::fs::MetadataExt;

const MAX_ATTEMPTS: usize = 128;

#[derive(Debug, Serialize, Deserialize)]
struct Chat {
    device: String,
    cwd: PathBuf,
    device_number: u64,
    inode: u64,
    agent: SessionAgent,
    original: Option<String>,
    attempts: Vec<Attempt>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Attempt {
    request_id: String,
    session_id: Option<String>,
    failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Binding {
    native_id: String,
    history: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
enum Observation {
    Bound(Binding),
    Ambiguous,
}

#[derive(Debug)]
/// A denied, unavailable or uncertain device-owned chat operation.
pub(crate) enum ResumeError {
    Unknown,
    Unavailable(&'static str),
    Failed(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for ResumeError {
    fn from(error: E) -> Self {
        Self::Failed(error.into())
    }
}

/// This table is the hosted resume gate. A version joins it only after an
/// installed-harness close-and-resume fixture proves identity and Hub replay.
const QUALIFIED: &[(SessionAgent, &str)] = &[(SessionAgent::Claude, "2.1.330")];

fn resume_args(agent: SessionAgent, native_id: &str) -> Vec<String> {
    let prefix = match agent {
        SessionAgent::Claude => "--resume",
        SessionAgent::Codex => "resume",
    };
    vec![prefix.to_owned(), native_id.to_owned()]
}

fn private_dir(home: &LatchHome, name: &str) -> anyhow::Result<PathBuf> {
    let dir = home.root().join("saved-chats").join(name);
    fs::create_dir_all(&dir)?;
    let permissions = fs::Permissions::from_mode(crate::session::paths::DIR_MODE);
    fs::set_permissions(home.root().join("saved-chats"), permissions.clone())?;
    fs::set_permissions(&dir, permissions)?;
    Ok(dir)
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> anyhow::Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn write<T: Serialize>(path: &Path, data: &T) -> anyhow::Result<()> {
    let temporary = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(FILE_MODE)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec(data)?)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn chat_path(home: &LatchHome, id: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(is_request_id(id), "invalid saved chat id");
    Ok(private_dir(home, "records")?.join(format!("{}.json", id.to_ascii_lowercase())))
}

fn reservation(home: &LatchHome, id: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(is_request_id(id), "invalid resume operation id");
    Ok(private_dir(home, "operations")?.join(format!("{}.json", id.to_ascii_lowercase())))
}

pub(super) fn request_reserved(home: &LatchHome, id: &str) -> anyhow::Result<bool> {
    Ok(reservation(home, id)?.exists())
}

fn observation_path(home: &LatchHome, id: &SessionId) -> anyhow::Result<PathBuf> {
    Ok(private_dir(home, "observations")?.join(format!("{id}.json")))
}

/// Called with the creation lock held, before the daemon can start.
pub(super) fn register(request: &RemoteSessionRequest, cwd: &Path) -> anyhow::Result<()> {
    let (Some(device), Some(agent)) = (&request.device, request.agent) else {
        return Ok(());
    };
    let path = chat_path(&request.home, &request.request_id)?;
    if let Some(existing) = read::<Chat>(&path)? {
        anyhow::ensure!(
            existing.device == *device && existing.cwd == cwd && existing.agent == agent,
            "saved chat owner or launch changed"
        );
        return Ok(());
    }
    let directory = fs::metadata(cwd)?;
    write(
        &path,
        &Chat {
            device: device.clone(),
            cwd: cwd.to_owned(),
            device_number: directory.dev(),
            inode: directory.ino(),
            agent,
            original: None,
            attempts: Vec::new(),
        },
    )
}

pub(super) fn record_original(
    home: &LatchHome,
    request_id: &str,
    session_id: &str,
) -> anyhow::Result<()> {
    let path = chat_path(home, request_id)?;
    if let Some(mut chat) = read::<Chat>(&path)? {
        if chat.original.is_none() {
            chat.original = Some(session_id.to_owned());
            write(&path, &chat)?;
        }
    }
    Ok(())
}

pub(super) fn has_original(home: &LatchHome, request_id: &str) -> anyhow::Result<bool> {
    Ok(read::<Chat>(&chat_path(home, request_id)?)?.is_some_and(|chat| chat.original.is_some()))
}

/// Each terminal can witness one exact native conversation. A second identity
/// or transcript is permanently ambiguous for that terminal.
pub(crate) fn observe(
    home: &LatchHome,
    session: &SessionId,
    harness: &str,
    id: Option<&str>,
    source: &Path,
) -> anyhow::Result<bool> {
    let meta = meta::read(&home.session(session))?;
    if meta.source.kind != MOBILE_SOURCE_KIND
        || meta.harness.as_deref() != Some(harness)
        || !matches!(harness, "claude" | "codex")
    {
        return Ok(true);
    }
    let expected = match meta.source.external_run_id {
        Some(ref request_id) if is_request_id(request_id) => {
            read::<String>(&reservation(home, request_id)?)?
        }
        _ => None,
    };
    let is_resumed = expected.is_some();
    let Some(id) = id else {
        return Ok(!is_resumed);
    };
    let binding = Binding {
        native_id: id.to_owned(),
        history: source.to_owned(),
    };
    let accepted = if let Some(chat_id) = expected {
        match read::<Chat>(&chat_path(home, &chat_id)?)? {
            Some(chat) => original_binding(home, &chat, &chat_id)?.as_ref() == Some(&binding),
            None => false,
        }
    } else {
        true
    };
    let path = observation_path(home, session)?;
    let lock = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(FILE_MODE)
        .open(path.with_extension("lock"))?;
    // Observer callbacks can overlap. This lock is independent of creation.
    anyhow::ensure!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } == 0,
        "cannot lock chat observation"
    );
    let previous = read::<Observation>(&path)?;
    if matches!(previous, Some(Observation::Ambiguous)) {
        return Ok(!is_resumed);
    }
    let valid = accepted && is_request_id(id) && source.is_absolute();
    match previous {
        Some(Observation::Bound(current)) if valid && current == binding => {}
        None if valid => write(&path, &Observation::Bound(binding))?,
        Some(_) => write(&path, &Observation::Ambiguous)?,
        None if is_resumed => write(&path, &Observation::Ambiguous)?,
        None => {}
    }
    Ok(!is_resumed || valid)
}

fn observation(home: &LatchHome, session_id: &str) -> anyhow::Result<Option<Observation>> {
    let id = SessionId::parse(session_id)?;
    read(&observation_path(home, &id)?)
}

fn latest_session(home: &LatchHome, chat: &Chat, id: &str) -> anyhow::Result<Option<String>> {
    for attempt in chat.attempts.iter().rev().filter(|a| !a.failed) {
        if let Some(session) = &attempt.session_id {
            return Ok(Some(session.clone()));
        }
        if let Some(session) = find_remote_session(home, &attempt.request_id)? {
            return Ok(Some(session.id));
        }
        // The id was accepted durably and its outcome still cannot be proven.
        // Starting another process here could produce a second active chat.
        anyhow::bail!("a previous resume launch has no recoverable outcome");
    }
    if chat.original.is_some() {
        return Ok(chat.original.clone());
    }
    Ok(find_remote_session(home, id)?.map(|session| session.id))
}

fn original_binding(home: &LatchHome, chat: &Chat, id: &str) -> anyhow::Result<Option<Binding>> {
    let session = match &chat.original {
        Some(session) => session.clone(),
        None => match find_remote_session(home, id)? {
            Some(session) => session.id,
            None => return Ok(None),
        },
    };
    Ok(match observation(home, &session)? {
        Some(Observation::Bound(binding)) => Some(binding),
        _ => None,
    })
}

fn original_directory(chat: &Chat) -> bool {
    chat.cwd.canonicalize().is_ok_and(|path| path == chat.cwd)
        && fs::metadata(&chat.cwd).is_ok_and(|dir| {
            dir.is_dir() && dir.dev() == chat.device_number && dir.ino() == chat.inode
        })
}

/// A filename and UUID alone do not prove that a usable native record remains.
/// Check one bounded prefix for the exact transcript identity and a prior turn.
fn history_available(agent: SessionAgent, binding: &Binding) -> bool {
    let id = &binding.native_id;
    let name = binding
        .history
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default();
    let named = match agent {
        SessionAgent::Claude => name == format!("{id}.jsonl"),
        SessionAgent::Codex => {
            name.starts_with("rollout-") && name.ends_with(&format!("-{id}.jsonl"))
        }
    };
    if !named {
        return false;
    }

    let Ok(file) = File::open(&binding.history) else {
        return false;
    };
    if !file.metadata().is_ok_and(|meta| meta.is_file()) {
        return false;
    }
    use std::io::BufRead;
    let records = io::BufReader::new(file.take(8 * 1024 * 1024))
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(&line).ok());
    let mut codex_identity = false;
    for record in records {
        let kind = record["type"].as_str();
        match agent {
            SessionAgent::Claude => {
                if kind == Some("user") && record["sessionId"].as_str() == Some(id) {
                    return true;
                }
            }
            SessionAgent::Codex => {
                if kind == Some("session_meta") {
                    if record["payload"]["id"].as_str() != Some(id) {
                        return false;
                    }
                    codex_identity = true;
                } else if codex_identity
                    && kind == Some("event_msg")
                    && record["payload"]["type"] == "user_message"
                {
                    return true;
                }
            }
        }
    }
    false
}

fn qualified_program(agent: SessionAgent) -> Option<PathBuf> {
    let versions = QUALIFIED
        .iter()
        .filter(|(candidate, _)| *candidate == agent)
        .peekable();
    let mut versions = versions;
    versions.peek()?;
    let program = resolve_agent_program(agent)?;
    let mut child = Command::new(&program)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + AGENT_LOOKUP_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut output = String::new();
    child
        .stdout
        .take()?
        .take(128)
        .read_to_string(&mut output)
        .ok()?;
    let mut parts = output.split_whitespace();
    let version = match agent {
        SessionAgent::Claude => parts.next()?,
        SessionAgent::Codex => parts.nth(1)?,
    };
    versions
        .any(|(_, known)| *known == version)
        .then_some(program)
}

fn owned_chat(
    home: &LatchHome,
    id: &str,
    device: Option<&str>,
) -> Result<(PathBuf, Chat), ResumeError> {
    let Some(device) = device else {
        return Err(ResumeError::Unknown);
    };
    let path = chat_path(home, id)?;
    let Some(chat) = read::<Chat>(&path)? else {
        return Err(ResumeError::Unknown);
    };
    if chat.device != device {
        return Err(ResumeError::Unknown);
    }
    Ok((path, chat))
}

fn ended(home: &LatchHome, id: &str) -> anyhow::Result<bool> {
    let id = SessionId::parse(id)?;
    // The kernel list must answer successfully. Lost state with metadata
    // retained is unconfirmed; a removed session with no kernel entry ended.
    let state = engine::list(home)?
        .into_iter()
        .find(|entry| entry.id == id.as_str());
    Ok(match state {
        Some(entry) => entry.state == engine::SessionState::Exited,
        None => !home.session(&id).meta().exists(),
    })
}

pub(crate) fn status(
    home: &LatchHome,
    id: &str,
    device: Option<&str>,
) -> Result<SavedChatStatus, ResumeError> {
    let (_, chat) = owned_chat(home, id, device)?;
    let session_id = latest_session(home, &chat, id)?;
    let mut result = SavedChatStatus {
        target: None,
        session_id,
        verified: false,
        can_resume: false,
        reason: None,
    };
    let Some(original) = original_binding(home, &chat, id)? else {
        result.reason = Some(
            "The host could not verify an exact native conversation for this chat.".to_owned(),
        );
        return Ok(result);
    };
    let Some(session_id) = &result.session_id else {
        return Ok(result);
    };
    if let Some(Observation::Bound(actual)) = observation(home, session_id)? {
        result.verified = actual == original;
    }
    if ended(home, session_id)? {
        result.reason = if !original_directory(&chat) {
            Some("Restore the original working directory or worktree on the host.")
        } else if !history_available(chat.agent, &original) {
            Some("The original native history is unavailable. Check it on the host.")
        } else if qualified_program(chat.agent).is_none() {
            Some("This host does not have a verified resume version for this harness.")
        } else {
            result.can_resume = true;
            None
        }
        .map(str::to_owned);
    }
    result.target = Some(SavedChatTarget {
        native_id: original.native_id,
        cwd: chat
            .cwd
            .to_str()
            .context("non-UTF-8 saved chat directory")?
            .to_owned(),
        agent: chat.agent,
    });
    Ok(result)
}

pub(crate) fn resume(
    home: &LatchHome,
    id: &str,
    request_id: &str,
    device: Option<&str>,
) -> Result<RemoteSessionOutcome, ResumeError> {
    let _lock = CreationLock::acquire(home)?;
    let (path, mut chat) = owned_chat(home, id, device)?;
    if let Some(attempt) = chat.attempts.iter().find(|a| a.request_id == request_id) {
        return match find_remote_session(home, request_id)? {
            Some(session) => Ok(RemoteSessionOutcome {
                id: session.id,
                name: session.name,
                created_at: session.created_at,
                reused: true,
            }),
            None => Err(ResumeError::Unavailable(if attempt.failed {
                "This resume attempt could not start. Try again."
            } else {
                "The earlier resume attempt could not be recovered. Check the chat on its host."
            })),
        };
    }
    if request_id == id
        || chat.attempts.len() >= MAX_ATTEMPTS
        || request_reserved(home, request_id)?
        || read_receipt(home, request_id)?.is_some()
        || find_remote_session(home, request_id)?.is_some()
    {
        return Err(ResumeError::Unavailable(
            "This resume request cannot be used. Check the chat on its host.",
        ));
    }
    let Some(session) = latest_session(home, &chat, id)? else {
        return Err(ResumeError::Unknown);
    };
    if !ended(home, &session)? {
        return Err(ResumeError::Unavailable(
            "The previous terminal has not confirmed it ended. Check it on the host.",
        ));
    }
    if !original_directory(&chat) {
        return Err(ResumeError::Unavailable("The original working directory is unavailable. Restore the original directory or worktree on the host."));
    }
    let Some(binding) =
        original_binding(home, &chat, id)?.filter(|b| history_available(chat.agent, b))
    else {
        return Err(ResumeError::Unavailable("The original native conversation could not be verified. Check its history on the host."));
    };
    let Some(program) = qualified_program(chat.agent) else {
        return Err(ResumeError::Unavailable(
            "The host does not have a verified harness version for this chat.",
        ));
    };
    let mut argv = vec![program.display().to_string()];
    argv.extend(resume_args(chat.agent, &binding.native_id));
    let mut manifest = run_manifest(
        argv,
        ManifestOptions {
            cwd: chat.cwd.clone(),
            size: REMOTE_INITIAL_SIZE,
            display: DisplayMetadata {
                source: crate::session::manifest::SourceInfo {
                    kind: MOBILE_SOURCE_KIND.to_owned(),
                    external_run_id: Some(request_id.to_owned()),
                    request_id: None,
                },
                ..DisplayMetadata::default()
            },
        },
    );
    manifest.launch.agent = Some(hosted_agent_kind(chat.agent));
    manifest.launch.login_shell = Some(LoginShell {
        path: login_shell_path(),
        prelude: None,
    });
    write(&reservation(home, request_id)?, &id)?;
    chat.attempts.push(Attempt {
        request_id: request_id.to_owned(),
        session_id: None,
        failed: false,
    });
    write(&path, &chat)?;

    let outcome = create_session_detached(CreateOptions {
        home: home.clone(),
        manifest,
    });
    let attempt = chat.attempts.last_mut().expect("accepted attempt");
    match outcome {
        Ok(created) => {
            let meta = meta::read(&created.paths)?;
            attempt.session_id = Some(created.id.to_string());
            write(&path, &chat)?;
            Ok(RemoteSessionOutcome {
                id: meta.id,
                name: meta.name,
                created_at: meta.created_at,
                reused: false,
            })
        }
        Err(error) => {
            if find_remote_session(home, request_id)?.is_none() {
                attempt.failed = true;
                write(&path, &chat)?;
            }
            Err(ResumeError::Failed(error))
        }
    }
}
