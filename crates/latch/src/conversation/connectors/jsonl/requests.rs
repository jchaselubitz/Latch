//! The pending-request queue: one request is shown to clients at a time,
//! with bridge-announced dialogs waiting behind it in the order raised.
use super::*;

impl JsonlConnector {
    pub(super) fn has_waiting_requests(&self) -> bool {
        !self.queued_requests.is_empty() || !self.displaced_requests.is_empty()
    }

    /// The shown request, then every request waiting behind it.
    pub(super) fn waiting_requests<'a>(
        &'a self,
        shown: &'a PendingRequest,
    ) -> impl Iterator<Item = &'a PendingRequest> {
        std::iter::once(shown)
            .chain(&self.queued_requests)
            .chain(&self.displaced_requests)
    }

    /// Shows a request the bridge announced under its call id. One request
    /// is shown at a time: while another bridge request is shown, this one
    /// waits behind it in the order the engine raised them, because a
    /// parallel batch raises every dialog at once and Claude paints the
    /// oldest.
    pub(super) fn announce_bridge_request(
        &mut self,
        request: PendingRequest,
    ) -> Vec<ConnectorMutation> {
        if let Some(waiting) = self
            .queued_requests
            .iter_mut()
            .find(|waiting| waiting.id == request.id)
        {
            *waiting = request;
            return Vec::new();
        }
        if self
            .displaced_requests
            .iter()
            .any(|waiting| waiting.id == request.id)
        {
            return Vec::new();
        }
        match self.pending_request.take() {
            Some(shown) if shown.bridge_call && shown.id != request.id => {
                self.pending_request = Some(shown);
                let at = self
                    .queued_requests
                    .iter()
                    .position(|waiting| announced_before(&request, waiting))
                    .unwrap_or(self.queued_requests.len());
                self.queued_requests.insert(at, request);
                Vec::new()
            }
            previous => {
                let mut mutations = Vec::new();
                // The hook may have announced the same dialog first, under a
                // synthetic id; the real one replaces it.
                if let Some(previous) = previous.filter(|previous| previous.id != request.id) {
                    mutations.push(request_mutation(&previous, RequestStatus::Dismissed));
                }
                self.pending_request = Some(request.clone());
                mutations.push(request_mutation(&request, RequestStatus::Pending));
                mutations
            }
        }
    }

    /// Closes the request with this id wherever it waits. Closing the shown
    /// request surfaces the next one.
    pub(super) fn close_request(&mut self, id: &str) -> Vec<ConnectorMutation> {
        self.settle_request(id, None, None)
    }

    pub(super) fn dismiss_screen_request(
        &mut self,
        request: PendingRequest,
    ) -> Vec<ConnectorMutation> {
        let mutations = self.close_request(&request.id);
        if request.bridge_call && self.bridge_version.is_some() {
            // A screen refresh can beat the bridge's authoritative close
            // record. Keep enough history to enrich the existing settled row.
            const MAX_DISMISSED_REQUESTS: usize = 8;
            if self.screen_dismissed_requests.len() == MAX_DISMISSED_REQUESTS {
                self.screen_dismissed_requests.remove(0);
            }
            self.screen_dismissed_requests.push(request);
        }
        mutations
    }

    pub(super) fn settle_request(
        &mut self,
        id: &str,
        source: Option<crate::conversation::ResolutionSource>,
        decision: Option<&str>,
    ) -> Vec<ConnectorMutation> {
        let settled = |mut request: PendingRequest| {
            request.metadata.resolved_by = source;
            request.metadata.decision = decision.map(str::to_owned);
            request_mutation(
                &request,
                if source
                    .is_some_and(|source| source != crate::conversation::ResolutionSource::Timeout)
                {
                    RequestStatus::Resolved
                } else {
                    RequestStatus::Dismissed
                },
            )
        };
        if self
            .pending_request
            .as_ref()
            .is_some_and(|shown| shown.id == id)
        {
            let closed = self.pending_request.take().expect("request was present");
            let mut mutations = vec![settled(closed)];
            mutations.extend(self.surface_next_request());
            return mutations;
        }
        if let Some(index) = self
            .displaced_requests
            .iter()
            .position(|waiting| waiting.id == id)
        {
            let closed = self.displaced_requests.remove(index);
            return vec![settled(closed)];
        }
        if let Some(index) = self
            .screen_dismissed_requests
            .iter()
            .position(|request| request.id == id)
        {
            let request = self.screen_dismissed_requests.remove(index);
            return if source.is_some() {
                vec![settled(request)]
            } else {
                Vec::new()
            };
        }
        // Never shown, so clients hold nothing to dismiss.
        self.queued_requests.retain(|waiting| waiting.id != id);
        Vec::new()
    }

    /// Shows the request next in line once the shown one has closed: the
    /// last one the screen displaced, which clients still hold as their
    /// newest pending request, else the oldest the bridge announced.
    pub(super) fn surface_next_request(&mut self) -> Vec<ConnectorMutation> {
        if self.bridge_version.is_none() && self.id != "codex" {
            // Only the bridge says when a waiting request closes.
            return self.drop_waiting_requests();
        }
        if let Some(request) = self.displaced_requests.pop() {
            self.pending_request = Some(request);
            return Vec::new();
        }
        if self.queued_requests.is_empty() {
            return Vec::new();
        }
        let request = self.queued_requests.remove(0);
        self.pending_request = Some(request.clone());
        vec![request_mutation(&request, RequestStatus::Pending)]
    }

    /// Forgets the requests waiting behind the shown one; they remain the
    /// terminal's to answer.
    pub(super) fn drop_waiting_requests(&mut self) -> Vec<ConnectorMutation> {
        self.queued_requests.clear();
        self.displaced_requests
            .drain(..)
            .map(|request| request_mutation(&request, RequestStatus::Dismissed))
            .collect()
    }

    /// Shows the waiting request whose dialog Claude is painting when that
    /// is not the shown one. Only a request clients have not seen yet can
    /// take its place: the Hub presents the newest pending item, so one they
    /// already hold could not be brought back in front.
    pub(super) fn follow_painted_request(&mut self, screen: &str) -> Vec<ConnectorMutation> {
        if self.queued_requests.is_empty() {
            return Vec::new();
        }
        let Some(shown) = self
            .pending_request
            .as_ref()
            .filter(|shown| shown.bridge_call)
        else {
            return Vec::new();
        };
        let Some(painted) =
            painted_request(screen, self.waiting_requests(shown)).map(|painted| painted.id.clone())
        else {
            return Vec::new();
        };
        let Some(index) = self
            .queued_requests
            .iter()
            .position(|waiting| waiting.id == painted)
        else {
            return Vec::new();
        };
        let mut request = self.queued_requests.remove(index);
        request.screen_seen = true;
        let choices = visible_choices(screen, &request.prompt);
        if !choices.is_empty() {
            request.choices = choices;
        }
        let displaced = self
            .pending_request
            .replace(request.clone())
            .expect("a request was shown");
        self.displaced_requests.push(displaced);
        vec![request_mutation(&request, RequestStatus::Pending)]
    }
}

pub(super) fn request_mutation(
    request: &PendingRequest,
    status: RequestStatus,
) -> ConnectorMutation {
    let mut metadata = request.metadata.clone();
    if metadata.request_kind.is_some() && status == RequestStatus::Pending {
        // A request that arrived with its own verdict (a dialog only the
        // terminal can answer, or a form the phone can) keeps it.
        let answerable = metadata
            .answerable
            .unwrap_or(request.bridge_call || !request.choices.is_empty());
        metadata.answerable = Some(answerable);
        metadata.reason = if answerable {
            None
        } else {
            metadata
                .reason
                .take()
                .or_else(|| Some("This request can only be answered at the terminal.".into()))
        };
        if request.bridge_call && request.request_type == RequestType::Permission {
            for label in &request.choices {
                if let Some(id) = permission_option_id(label) {
                    if !metadata.options.iter().any(|option| option.id == id) {
                        metadata.options.push(crate::conversation::RequestOption {
                            id: id.into(),
                            label: label.clone(),
                            requires_text: false,
                        });
                    }
                }
            }
            metadata
                .options
                .sort_by_key(|option| super::dialogs::option_rank(&option.id));
        }
    }
    upsert(
        &format!("request:{}", request.id),
        "1970-01-01T00:00:00Z".to_owned(),
        ConversationItemKind::Request {
            request_id: request.id.clone(),
            request_type: request.request_type.clone(),
            prompt: if metadata.request_kind.is_some()
                && request.request_type == RequestType::Permission
            {
                sanitize_part(&request.prompt, 4096)
            } else {
                request.prompt.clone()
            },
            choices: request.choices.clone(),
            questions: request.questions.clone(),
            status,
            metadata: Box::new(metadata),
        },
    )
}

/// Whether the bridge announced `request` before `other`. Bridge records
/// carry millisecond timestamps taken as each dialog was raised; they may
/// reach the sidecar out of order, since each is its own run of `latch`.
fn announced_before(request: &PendingRequest, other: &PendingRequest) -> bool {
    match (
        request.announced_at.as_deref(),
        other.announced_at.as_deref(),
    ) {
        (Some(request), Some(other)) if request.len() > 20 && other.len() > 20 => request < other,
        _ => false,
    }
}

pub(super) fn claude_question_metadata() -> crate::conversation::RequestMetadata {
    crate::conversation::RequestMetadata {
        request_kind: Some("question".into()),
        ..Default::default()
    }
}

pub(super) fn claude_permission_metadata(
    tool: &str,
    input: Option<&Value>,
    cwd: Option<&str>,
) -> crate::conversation::RequestMetadata {
    let mut data = crate::conversation::RequestMetadata::default();
    let mut detail = |key: &str, value: Option<&str>, limit| {
        if let Some(value) = value
            .map(|text| sanitize_part(text, limit))
            .filter(|s| !s.is_empty())
        {
            data.details.insert(key.into(), Value::String(value));
        }
    };
    let field = |key: &str| {
        input
            .and_then(|input| input.get(key))
            .and_then(Value::as_str)
    };
    let kind = match tool {
        "Bash" | "PowerShell" => {
            detail("command", field("command"), 4096);
            detail("cwd", field("cwd").or(cwd), 1024);
            detail("sandbox", field("sandbox"), 1024);
            detail("reason", field("description"), 1024);
            "command"
        }
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => {
            if let Some(path) = field("file_path").or_else(|| field("notebook_path")) {
                data.details.insert(
                    "paths".into(),
                    serde_json::json!([sanitize_part(path, 1024)]),
                );
            }
            // Claude supplies edit inputs rather than a unified diff. Keep a
            // bounded per-file preview and omit raw MCP and file contents.
            if let Some(path) = field("file_path").or_else(|| field("notebook_path")) {
                data.details.insert(
                    "changes".into(),
                    serde_json::json!([{
                        "path": sanitize_part(path, 1024),
                        "summary": format!("{} file", sanitize_part(tool, 128)),
                    }]),
                );
            }
            "file_change"
        }
        "AskUserQuestion" => "question",
        _ if tool.starts_with("mcp__") => {
            if let Some((server, name)) = tool.trim_start_matches("mcp__").split_once("__") {
                detail("server", Some(server), 256);
                detail("tool", Some(name), 256);
            } else {
                detail("tool", Some(tool), 256);
            }
            let arguments = input.and_then(Value::as_object).map(|input| {
                input
                    .keys()
                    .filter(|key| key.as_str() != "description")
                    .take(16)
                    .map(|key| sanitize_part(key, 60))
                    .collect::<Vec<_>>()
                    .join(", ")
            });
            detail(
                "argumentsSummary",
                field("description").or(arguments.as_deref()),
                1024,
            );
            "mcp_tool"
        }
        _ => "other",
    };
    data.request_kind = Some(kind.into());
    data
}

pub(super) fn permission_option_id(label: &str) -> Option<&'static str> {
    match bridge_permission_decision(label) {
        Some("allow") => Some("allow_once"),
        Some("deny") => Some("deny"),
        _ => None,
    }
}
