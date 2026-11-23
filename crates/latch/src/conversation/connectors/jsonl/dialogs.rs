//! Claude's dialogs beyond a plain tool permission: plan approval, permission
//! suggestions, MCP elicitations, and the dialogs only the terminal can answer.
//!
//! The decision table is `planning/CLAUDE_REQUESTS_V2.md`. In short: plan
//! approval and suggestions are answered by the bridge module from its
//! `PermissionRequest` hook; an elicitation by the blocked `Elicitation`
//! command hook; the rest are shown with `answerable: false` and a reason,
//! and never answered by typing.
use super::*;
use crate::conversation::{RequestMetadata, RequestOption, ResolutionSource};

const MAX_PLAN_CHARS: usize = 8192;
const MAX_OPTIONS: usize = 8;
const MAX_FORM_FIELDS: usize = 32;

/// The order options are offered in: the plain yes first, refusals last.
pub(super) fn option_rank(id: &str) -> u8 {
    match id {
        "allow_once" | "approve_plan" => 0,
        "approve_plan_accept_edits" | "allow_session" => 1,
        "allow_always" => 2,
        "submit_form" | "open_url" => 0,
        "deny" => 8,
        "deny_with_feedback" | "reject_plan_with_feedback" => 9,
        "cancel" => 10,
        _ => 5,
    }
}

/// An option id the bridge may offer for a permission it can settle from its
/// own hook. Anything else is refused rather than shown.
fn is_hook_option(id: &str) -> bool {
    matches!(
        id,
        "allow_session"
            | "allow_always"
            | "deny_with_feedback"
            | "approve_plan"
            | "approve_plan_accept_edits"
            | "reject_plan_with_feedback"
    ) || id.strip_prefix("allow_mode_").is_some_and(|mode| {
        !mode.is_empty() && mode.len() <= 32 && mode.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

fn option(id: &str, label: &str, requires_text: bool) -> RequestOption {
    RequestOption {
        id: id.to_owned(),
        label: label.to_owned(),
        requires_text,
    }
}

fn bounded(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// What a `permission.open` bridge record adds to a permission: the plan of
/// an ExitPlanMode and the options the bridge's own hook can carry out.
pub(super) fn apply_permission_extras(
    tool: &str,
    object: &serde_json::Map<String, Value>,
    prompt: &mut String,
    metadata: &mut RequestMetadata,
) {
    if tool == "ExitPlanMode" {
        metadata.request_kind = Some("plan_approval".into());
        metadata.details.clear();
        if let Some(plan) = string(object, "plan").filter(|plan| !plan.is_empty()) {
            metadata.details.insert(
                "planMarkdown".into(),
                Value::String(bounded(&plan, MAX_PLAN_CHARS)),
            );
        }
        if let Some(path) = string(object, "plan_path").filter(|path| !path.is_empty()) {
            metadata
                .details
                .insert("planPath".into(), Value::String(bounded(&path, 1024)));
        }
        *prompt = "Approve this plan?".to_owned();
    }
    let offered = object
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let id = entry.get("id")?.as_str()?;
            let label = entry.get("label")?.as_str()?;
            (is_hook_option(id) && !label.is_empty()).then(|| {
                option(
                    id,
                    &bounded(label, 256),
                    entry
                        .get("requires_text")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                )
            })
        })
        .take(MAX_OPTIONS);
    for offered in offered {
        if !metadata.options.iter().any(|held| held.id == offered.id) {
            metadata.options.push(offered);
        }
    }
}

/// The `app_access` and other requests the bridge raises for a dialog only
/// the terminal can answer, under the call's id.
pub(super) fn terminal_request_from_bridge(
    object: &serde_json::Map<String, Value>,
) -> Option<PendingRequest> {
    let id = string(object, "tool_use_id")?;
    let kind = match string(object, "kind").as_deref() {
        Some("app_access") => "app_access",
        Some("sign_in") => "sign_in",
        _ => "other",
    };
    let mut details = serde_json::Map::new();
    if kind == "app_access" {
        let apps: Vec<String> = object
            .get("apps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|app| sanitize_part(app, 64))
            .filter(|app| !app.is_empty())
            .take(8)
            .collect();
        if !apps.is_empty() {
            details.insert(
                "appName".into(),
                Value::String(bounded(&apps.join(", "), 256)),
            );
        }
    }
    if let Some(reason) = string(object, "request_reason") {
        let reason = sanitize_part(&reason, 1024);
        if !reason.is_empty() {
            details.insert("reason".into(), Value::String(reason));
        }
    }
    Some(PendingRequest {
        id,
        request_type: RequestType::Permission,
        prompt: match kind {
            "app_access" => "Claude is asking for access to apps on this Mac.".to_owned(),
            _ => "Claude is waiting for a dialog at the terminal.".to_owned(),
        },
        choices: Vec::new(),
        questions: Vec::new(),
        screen_seen: false,
        announced_at: string(object, "timestamp"),
        bridge_call: true,
        metadata: RequestMetadata {
            request_kind: Some(kind.into()),
            details,
            answerable: Some(false),
            reason: Some(
                match kind {
                    "app_access" => "Allow or deny app access at the Mac.",
                    _ => "Answer this dialog at the terminal.",
                }
                .into(),
            ),
            ..Default::default()
        },
    })
}

/// Classifies a Claude `Notification` message that has no answer hook.
fn classify_notification(
    message: &str,
    notification_type: Option<&str>,
) -> Option<(&'static str, &'static str)> {
    if matches!(
        notification_type,
        Some("idle_prompt" | "elicitation_response" | "auth_success" | "elicitation_complete")
    ) {
        return None;
    }
    let text = message.to_ascii_lowercase();
    if text.contains("session paused") {
        Some((
            "other",
            "Choose how to continue (a fallback model, editing the prompt, or usage credits) at the terminal.",
        ))
    } else if text.contains("network access") {
        Some(("network", "Allow or deny network access at the terminal."))
    } else if text.contains("approval for the plan") {
        Some(("plan_approval", "Approve the plan at the terminal."))
    } else if text.contains("sign in") || text.contains("authenticate") || text.contains("oauth") {
        Some(("sign_in", "Finish signing in at the Mac."))
    } else if text.contains("permission")
        || text.contains("needs your")
        || text.contains("approval")
    {
        Some(("other", "Answer this prompt at the terminal."))
    } else {
        None
    }
}

impl JsonlConnector {
    /// An MCP server asked for input. The observer's `Elicitation` hook is
    /// blocked, waiting for the phone's answer.
    pub(super) fn claude_elicitation(
        &mut self,
        object: &serde_json::Map<String, Value>,
    ) -> Vec<ConnectorMutation> {
        let Some(id) = string(object, "latch_elicitation_id") else {
            return Vec::new();
        };
        let is_url = string(object, "mode").as_deref() == Some("url");
        let server = string(object, "mcp_server_name").unwrap_or_default();
        let message = string(object, "message").unwrap_or_default();
        let mut details = serde_json::Map::new();
        details.insert("server".into(), Value::String(sanitize_part(&server, 256)));
        details.insert("message".into(), Value::String(bounded(&message, 4096)));
        let options = if is_url {
            if let Some(url) = string(object, "url").filter(|url| url.len() <= 2048) {
                details.insert("url".into(), Value::String(url.clone()));
                if let Some(host) = url
                    .split("://")
                    .nth(1)
                    .and_then(|rest| rest.split('/').next())
                {
                    details.insert("origin".into(), Value::String(bounded(host, 1024)));
                }
            }
            vec![
                option("open_url", "Open link", false),
                option("deny", "Decline", false),
                option("cancel", "Cancel", false),
            ]
        } else {
            if let Some(schema) = object
                .get("requested_schema")
                .filter(|schema| schema.is_object())
            {
                details.insert("schema".into(), schema.clone());
            }
            vec![
                option("submit_form", "Submit", false),
                option("deny", "Decline", false),
                option("cancel", "Cancel", false),
            ]
        };
        let request = PendingRequest {
            id,
            request_type: RequestType::Permission,
            prompt: if message.is_empty() {
                format!("{server} is asking for input.")
            } else {
                bounded(&message, 4096)
            },
            choices: Vec::new(),
            questions: Vec::new(),
            screen_seen: false,
            announced_at: string(object, "timestamp"),
            bridge_call: false,
            metadata: RequestMetadata {
                request_kind: Some(
                    if is_url {
                        "elicitation_url"
                    } else {
                        "elicitation_form"
                    }
                    .into(),
                ),
                details,
                options,
                answerable: Some(true),
                ..Default::default()
            },
        };
        self.show_dialog(request)
    }

    /// The hook waited its full time: the form now opens at the terminal.
    /// Never accepted for the person.
    pub(super) fn claude_elicitation_timeout(
        &mut self,
        object: &serde_json::Map<String, Value>,
    ) -> Vec<ConnectorMutation> {
        let Some(id) = string(object, "latch_elicitation_id") else {
            return Vec::new();
        };
        let Some(waiting) = self
            .pending_request
            .as_ref()
            .filter(|request| request.id == id)
            .cloned()
        else {
            return Vec::new();
        };
        let mut mutations = self.settle_request(&id, Some(ResolutionSource::Timeout), None);
        let mut metadata = waiting.metadata;
        metadata.options.clear();
        metadata.answerable = Some(false);
        metadata.reason = Some("Answer this form at the terminal.".into());
        let terminal = PendingRequest {
            id: format!("{id}:terminal"),
            metadata,
            announced_at: string(object, "timestamp"),
            ..waiting
        };
        mutations.extend(self.show_dialog(terminal));
        mutations
    }

    /// The elicitation was answered, wherever. Closes it if still shown.
    pub(super) fn claude_elicitation_result(
        &mut self,
        object: &serde_json::Map<String, Value>,
    ) -> Vec<ConnectorMutation> {
        let Some(id) = self
            .pending_request
            .as_ref()
            .filter(|request| is_elicitation_kind(request))
            .map(|request| request.id.clone())
        else {
            return Vec::new();
        };
        let decision = match string(object, "action").as_deref() {
            Some("accept") => Some("submit_form"),
            Some("decline") => Some("deny"),
            Some("cancel") => Some("cancel"),
            _ => None,
        };
        self.settle_request(&id, Some(ResolutionSource::Terminal), decision)
    }

    /// A Notification names a dialog that has no answer hook. It is shown,
    /// unanswerable, only when nothing else is shown for the same moment.
    pub(super) fn claude_notification(
        &mut self,
        object: &serde_json::Map<String, Value>,
    ) -> Vec<ConnectorMutation> {
        if self.pending_request.is_some() {
            return Vec::new();
        }
        let message = string(object, "message").unwrap_or_default();
        let Some((kind, reason)) =
            classify_notification(&message, string(object, "notification_type").as_deref())
        else {
            return Vec::new();
        };
        let at = string(object, "timestamp").unwrap_or_default();
        let digest = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            (&at, &message).hash(&mut hasher);
            hasher.finish()
        };
        let request = PendingRequest {
            id: format!("notification:{digest:016x}"),
            request_type: RequestType::Permission,
            prompt: sanitize_part(&message, 1024),
            choices: Vec::new(),
            questions: Vec::new(),
            screen_seen: false,
            announced_at: Some(at),
            bridge_call: false,
            metadata: RequestMetadata {
                request_kind: Some(kind.into()),
                answerable: Some(false),
                reason: Some(reason.into()),
                ..Default::default()
            },
        };
        self.show_dialog(request)
    }

    /// The turn ended: a dialog nobody can answer from here is gone with it.
    pub(super) fn dismiss_terminal_only(&mut self) -> Vec<ConnectorMutation> {
        match self
            .pending_request
            .as_ref()
            .filter(|request| request.metadata.answerable == Some(false))
            .map(|request| request.id.clone())
        {
            Some(id) => self.settle_request(&id, None, None),
            None => Vec::new(),
        }
    }

    fn show_dialog(&mut self, request: PendingRequest) -> Vec<ConnectorMutation> {
        let mut mutations = Vec::new();
        if let Some(previous) = self
            .pending_request
            .replace(request.clone())
            .filter(|previous| previous.id != request.id)
        {
            mutations.push(request_mutation(&previous, RequestStatus::Dismissed));
        }
        mutations.push(request_mutation(&request, RequestStatus::Pending));
        mutations
    }

    /// Answers the shown dialog when it is one of the new kinds. `None` leaves
    /// the action to the generic paths.
    pub(super) fn answer_dialog(
        &mut self,
        payload: &Value,
        deadline: Duration,
    ) -> Result<Option<ApplyResult>> {
        let Some(request) = self.pending_request.clone() else {
            return Ok(None);
        };
        if payload.get("requestId").and_then(Value::as_str) != Some(request.id.as_str()) {
            return Ok(None);
        }
        if request.metadata.answerable == Some(false) {
            return Ok(Some(ApplyResult::Refused {
                reason: request
                    .metadata
                    .reason
                    .clone()
                    .unwrap_or_else(|| "answer at the terminal".to_owned()),
            }));
        }
        if is_elicitation(&request) {
            return Ok(Some(self.answer_elicitation(&request, payload)));
        }
        let option_id = payload.get("optionId").and_then(Value::as_str);
        let offered = option_id.and_then(|id| {
            request
                .metadata
                .options
                .iter()
                .find(|option| option.id == id && is_hook_option(id))
        });
        let (Some(offered), true) = (
            offered,
            request.bridge_call && request.request_type == RequestType::Permission,
        ) else {
            return Ok(None);
        };
        if self.bridge_version.is_none() {
            return Ok(Some(ApplyResult::Refused {
                reason: "the bridge is no longer live; answer at the terminal".into(),
            }));
        }
        let text = payload
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty());
        if offered.requires_text && text.is_none() {
            return Ok(Some(ApplyResult::Refused {
                reason: "this option needs a message".into(),
            }));
        }
        let mut command = serde_json::Map::new();
        command.insert("tool_use_id".into(), Value::from(request.id.clone()));
        command.insert("option".into(), Value::from(offered.id.clone()));
        if let Some(text) = text.filter(|_| offered.requires_text) {
            command.insert("text".into(), Value::from(bounded(text, 4096)));
        }
        Ok(Some(
            match self.ask_bridge("answer_permission", command, deadline)? {
                BridgeAnswer::Accepted => {
                    self.close_request(&request.id);
                    self.screen_can_send = Some(false);
                    self.last_screen_refresh = None;
                    ApplyResult::Accepted { correlation: None }
                }
                BridgeAnswer::Refused(reason) => ApplyResult::Refused { reason },
                BridgeAnswer::NotTaken => {
                    self.bridge_version = None;
                    ApplyResult::Refused {
                        reason: "the bridge is no longer live; answer at the terminal".into(),
                    }
                }
                BridgeAnswer::Queued => ApplyResult::Refused {
                    reason: "the bridge did not answer the request".into(),
                },
            },
        ))
    }

    fn answer_elicitation(&mut self, request: &PendingRequest, payload: &Value) -> ApplyResult {
        let Some(option_id) = payload.get("optionId").and_then(Value::as_str) else {
            return ApplyResult::Refused {
                reason: "choose an option".into(),
            };
        };
        if !request
            .metadata
            .options
            .iter()
            .any(|option| option.id == option_id)
        {
            return ApplyResult::Refused {
                reason: "this option is not offered".into(),
            };
        }
        let (action, content) = match option_id {
            "submit_form" => {
                let schema = request.metadata.details.get("schema");
                match validate_form(schema, payload.get("form")) {
                    Ok(content) => ("accept", Some(content)),
                    Err(reason) => return ApplyResult::Refused { reason },
                }
            }
            "open_url" => ("accept", None),
            "deny" => ("decline", None),
            "cancel" => ("cancel", None),
            _ => {
                return ApplyResult::Refused {
                    reason: "this option is not offered".into(),
                }
            }
        };
        let paths = self.home.session(&self.session);
        if let Err(error) =
            crate::observer::queue_elicitation_answer(&paths, &request.id, action, content)
        {
            return ApplyResult::Refused {
                reason: format!("the answer could not be handed to Claude: {error}"),
            };
        }
        self.close_request(&request.id);
        self.screen_can_send = Some(false);
        self.last_screen_refresh = None;
        ApplyResult::Accepted { correlation: None }
    }
}

/// An elicitation, whether the phone can still answer it or it has stepped
/// aside to the terminal.
fn is_elicitation_kind(request: &PendingRequest) -> bool {
    !request.bridge_call
        && matches!(
            request.metadata.request_kind.as_deref(),
            Some("elicitation_form" | "elicitation_url")
        )
}

fn is_elicitation(request: &PendingRequest) -> bool {
    is_elicitation_kind(request) && request.metadata.answerable != Some(false)
}

/// The form the phone sent, checked against the schema the server asked with:
/// only named fields, every required one present, each of the right type.
pub(crate) fn validate_form(schema: Option<&Value>, form: Option<&Value>) -> Result<Value, String> {
    let form = form
        .and_then(Value::as_object)
        .ok_or_else(|| "submit the form's values".to_owned())?;
    if form.len() > MAX_FORM_FIELDS {
        return Err("the form has too many fields".into());
    }
    let properties = schema
        .and_then(|schema| schema.get("properties"))
        .and_then(Value::as_object);
    if let Some(properties) = properties {
        for (name, value) in form {
            let Some(field) = properties.get(name) else {
                return Err(format!("the form has no field {name}"));
            };
            check_field(name, field, value)?;
        }
    }
    let required = schema
        .and_then(|schema| schema.get("required"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str);
    for name in required {
        if !form.contains_key(name) {
            return Err(format!("{name} is required"));
        }
    }
    Ok(Value::Object(form.clone()))
}

fn check_field(name: &str, field: &Value, value: &Value) -> Result<(), String> {
    let wrong = || Err(format!("{name} has the wrong type"));
    match field.get("type").and_then(Value::as_str) {
        Some("string") => {
            let Some(text) = value.as_str() else {
                return wrong();
            };
            let length = text.chars().count() as u64;
            if field
                .get("minLength")
                .and_then(Value::as_u64)
                .is_some_and(|min| length < min)
                || field
                    .get("maxLength")
                    .and_then(Value::as_u64)
                    .is_some_and(|max| length > max)
            {
                return Err(format!("{name} is the wrong length"));
            }
        }
        Some("integer") if !(value.is_i64() || value.is_u64()) => return wrong(),
        Some("number") if !value.is_number() => return wrong(),
        Some("boolean") if !value.is_boolean() => return wrong(),
        Some("array") if !value.is_array() => return wrong(),
        _ => {}
    }
    if let Some(number) = value.as_f64().filter(|_| value.is_number()) {
        if field
            .get("minimum")
            .and_then(Value::as_f64)
            .is_some_and(|min| number < min)
            || field
                .get("maximum")
                .and_then(Value::as_f64)
                .is_some_and(|max| number > max)
        {
            return Err(format!("{name} is out of range"));
        }
    }
    if let Some(choices) = field.get("enum").and_then(Value::as_array) {
        if !choices.contains(value) {
            return Err(format!("{name} is not one of the offered values"));
        }
    }
    Ok(())
}
