//! Translation of Codex ServerRequests. Only the display projection is
//! persisted; the original request and its allowed replies stay in memory.
use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::conversation::{QuestionOption, RequestMetadata, RequestOption, RequestQuestion};

const MAX_PRESENTATION_BYTES: usize = 14 * 1024;
const TERMINAL: &str = "Answer this request at the Mac.";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Presentation {
    pub id: String,
    pub prompt: String,
    pub metadata: RequestMetadata,
    pub questions: Vec<RequestQuestion>,
}

pub(super) struct Request {
    pub rpc_id: Value,
    pub thread: String,
    pub item: Option<String>,
    pub view: Presentation,
    replies: HashMap<String, Value>,
    question_ids: HashMap<String, String>,
}

impl Request {
    pub fn new(event: &Value, id: String) -> Option<Self> {
        let rpc_id = event["id"].clone();
        if !rpc_id.is_i64() && !rpc_id.is_u64() && !rpc_id.as_str().is_some_and(|s| s.len() <= 128)
        {
            return None;
        }
        let p = &event["params"];
        let thread = p["threadId"]
            .as_str()
            .or_else(|| p["conversationId"].as_str())?;
        let mut request = Self {
            rpc_id,
            thread: thread.to_owned(),
            item: p["itemId"].as_str().map(str::to_owned),
            view: Presentation {
                id,
                prompt: String::new(),
                metadata: RequestMetadata::default(),
                questions: Vec::new(),
            },
            replies: HashMap::new(),
            question_ids: HashMap::new(),
        };
        let (kind, title) = match event["method"].as_str()? {
            "item/commandExecution/requestApproval" => {
                request.detail("command", p["command"].as_str(), 2048);
                request.detail("cwd", p["cwd"].as_str(), 512);
                request.detail("action", p["kind"].as_str(), 64);
                request.detail("host", p["networkApprovalContext"]["host"].as_str(), 256);
                request.detail(
                    "protocol",
                    p["networkApprovalContext"]["protocol"].as_str(),
                    80,
                );
                request.permission_scope(&p["additionalPermissions"]);
                let network = p["networkApprovalContext"].is_object();
                if let Some(choices) = p["availableDecisions"].as_array() {
                    for decision in choices.iter().take(16) {
                        request.decision(decision);
                    }
                } else if p["availableDecisions"].is_null() {
                    for decision in ["accept", "decline", "cancel"] {
                        request.decision(&json!(decision));
                    }
                    if let Some(rule) = p["proposedExecpolicyAmendment"].as_array() {
                        request.decision(
                            &json!({"acceptWithExecpolicyAmendment":{"execpolicy_amendment":rule}}),
                        );
                    }
                    if let Some(rules) = p["proposedNetworkPolicyAmendments"].as_array() {
                        for rule in rules.iter().take(8) {
                            request.decision(&json!({"applyNetworkPolicyAmendment":{"network_policy_amendment":rule}}));
                        }
                    }
                }
                if network {
                    ("network", "Allow Codex network access?")
                } else {
                    ("command", "Allow Codex to run this command?")
                }
            }
            "item/fileChange/requestApproval" | "applyPatchApproval" => {
                request.detail("grantRoot", p["grantRoot"].as_str(), 512);
                if let Some(changes) = p["fileChanges"].as_object() {
                    request.changes(&Value::Array(changes.iter().take(8).map(|(path, change)| {
                        json!({"path":path, "diff":change["unified_diff"], "kind":change["type"]})
                    }).collect()));
                }
                if event["method"] == "applyPatchApproval" {
                    for decision in ["approved", "approved_for_session", "denied", "abort"] {
                        request.decision(&json!(decision));
                    }
                } else {
                    for decision in ["accept", "acceptForSession", "decline", "cancel"] {
                        request.decision(&json!(decision));
                    }
                }
                ("file_change", "Allow Codex to change files?")
            }
            "execCommandApproval" => {
                request.detail(
                    "command",
                    p["command"]
                        .as_array()
                        .map(|_| p["command"].to_string())
                        .as_deref(),
                    2048,
                );
                request.detail("cwd", p["cwd"].as_str(), 512);
                for decision in ["approved", "approved_for_session", "denied", "abort"] {
                    request.decision(&json!(decision));
                }
                ("command", "Allow Codex to run this command?")
            }
            "item/permissions/requestApproval" => {
                request.detail("cwd", p["cwd"].as_str(), 512);
                request.permission_scope(&p["permissions"]);
                // Grant only the exact scope displayed. Larger or unfamiliar
                // profiles go to the TUI, which can show the complete request.
                if complete_permission_scope(&p["permissions"]) {
                    request.option(
                        "allow_once",
                        "Allow for this turn",
                        json!({"permissions":p["permissions"],"scope":"turn"}),
                    );
                    request.option(
                        "allow_session",
                        "Allow for this session",
                        json!({"permissions":p["permissions"],"scope":"session"}),
                    );
                }
                request.option("deny", "Deny", json!({"permissions":{},"scope":"turn"}));
                request.detail("scope", Some("This turn or this session"), 80);
                ("permissions", "Grant Codex additional permissions?")
            }
            "item/tool/requestUserInput" => {
                request.questions(&p["questions"]);
                ("question", "Codex has a question")
            }
            "mcpServer/elicitation/request" => request.elicitation(p),
            "item/tool/call" => {
                request.detail("tool", p["tool"].as_str(), 128);
                ("mcp_tool", "Codex requested a dynamic tool")
            }
            // Account/attestation requests are client infrastructure, not dialogs.
            "account/chatgptAuthTokens/refresh" | "attestation/generate" => return None,
            _ => ("other", "Codex requested an action"),
        };
        request.detail("reason", p["reason"].as_str(), 768);
        request.view.prompt = display(
            p["message"]
                .as_str()
                .or_else(|| p["description"].as_str())
                .unwrap_or(title),
            1024,
        );
        request.view.metadata.request_kind = Some(kind.into());
        request.view.metadata.answerable =
            Some(!request.replies.is_empty() || !request.question_ids.is_empty());
        if request.view.metadata.answerable == Some(false) {
            request
                .view
                .metadata
                .reason
                .get_or_insert_with(|| TERMINAL.into());
        }
        request.limit_view();
        Some(request)
    }

    fn limit_view(&mut self) {
        if serde_json::to_vec(&self.view).is_ok_and(|bytes| bytes.len() <= MAX_PRESENTATION_BYTES) {
            return;
        }
        let metadata = &mut self.view.metadata;
        metadata.details.clear();
        metadata.options.clear();
        self.view.questions.clear();
        metadata.answerable = Some(false);
        metadata.reason =
            Some("This request is too large for the phone. Review it at the Mac.".into());
    }

    fn detail(&mut self, name: &str, value: Option<&str>, limit: usize) {
        if let Some(value) = value {
            self.view
                .metadata
                .details
                .insert(name.into(), json!(display(value, limit)));
        }
    }

    pub fn changes(&mut self, changes: &Value) {
        let values = changes.as_array().into_iter().flatten().take(6).filter_map(|c| {
            let path = display(c["path"].as_str()?, 512);
            let diff: Vec<_> = c["diff"].as_str().unwrap_or_default().lines().take(12)
                .map(|line| display(line, 100)).collect();
            Some(json!({"path":path,"diff":diff.join("\n"),"summary":display(&c["kind"].as_str().unwrap_or_default(), 64)}))
        }).collect::<Vec<_>>();
        if !values.is_empty() {
            self.view
                .metadata
                .details
                .insert("changes".into(), json!(values));
            self.limit_view();
        }
    }

    fn permission_scope(&mut self, profile: &Value) {
        if let Some(enabled) = profile["network"]["enabled"].as_bool() {
            self.view.metadata.details.insert(
                "networkScope".into(),
                json!([if enabled {
                    "Network enabled"
                } else {
                    "Network disabled"
                }]),
            );
        }
        let fs = &profile["fileSystem"];
        let mut paths = Vec::new();
        for mode in ["read", "write"] {
            paths.extend(
                fs[mode]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(12)
                    .filter_map(|path| Some(format!("{mode}: {}", display(path.as_str()?, 256)))),
            );
        }
        paths.extend(
            fs["entries"]
                .as_array()
                .into_iter()
                .flatten()
                .take(12)
                .map(|entry| {
                    format!(
                        "{}: {}",
                        display(entry["access"].as_str().unwrap_or_default(), 32),
                        display(&entry["path"].to_string(), 256)
                    )
                }),
        );
        if !paths.is_empty() {
            self.view
                .metadata
                .details
                .insert("fileSystemScope".into(), json!(paths));
        }
    }

    fn option(&mut self, id: &str, label: &str, result: Value) {
        if self.replies.values().any(|existing| *existing == result) {
            return;
        }
        let mut option_id = id.to_owned();
        let mut variant = 2;
        while self.replies.contains_key(&option_id) {
            option_id = format!("{id}-{variant}");
            variant += 1;
        }
        self.view.metadata.options.push(RequestOption {
            id: option_id.clone(),
            label: display(label, 640),
            requires_text: false,
        });
        self.replies.insert(option_id, result);
    }

    fn decision(&mut self, decision: &Value) {
        let response = json!({"decision":decision});
        match decision.as_str() {
            Some("accept" | "approved") => self.option("allow_once", "Allow once", response),
            Some("acceptForSession" | "approved_for_session") => {
                self.option("allow_session", "Allow for this session", response)
            }
            Some("decline" | "denied") => self.option("deny", "Deny", response),
            Some("cancel" | "abort") => self.option("cancel", "Cancel", response),
            _ => {
                if let Some(rule) = decision
                    .pointer("/acceptWithExecpolicyAmendment/execpolicy_amendment")
                    .and_then(Value::as_array)
                {
                    let rule = rule.iter().filter_map(Value::as_str).collect::<Vec<_>>();
                    let label = format!("Always allow command prefix {}", json!(rule));
                    if !rule.is_empty()
                        && fully_displayed(&label, 600)
                        && display(&label, 640) == label
                    {
                        self.option("allow_always", &label, response);
                    }
                } else if let Some(rule) =
                    decision.pointer("/applyNetworkPolicyAmendment/network_policy_amendment")
                {
                    if let Some(host) = rule["host"].as_str().filter(|host| host.len() <= 256) {
                        match rule["action"].as_str() {
                            Some("allow") => self.option(
                                "allow_always",
                                &format!("Always allow network access to {host}"),
                                response,
                            ),
                            Some("deny") => self.option(
                                "deny_always",
                                &format!("Always deny network access to {host}"),
                                response,
                            ),
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    fn questions(&mut self, input: &Value) {
        let Some(questions) = input
            .as_array()
            .filter(|rows| !rows.is_empty() && rows.len() <= 8)
        else {
            return;
        };
        let mut ids = HashSet::new();
        let mut valid = true;
        let mut mapping = HashMap::new();
        for question in questions {
            if question["isSecret"] == true {
                self.view.metadata.reason =
                    Some("This request includes a secret question. Answer it at the Mac.".into());
                valid = false;
                continue;
            }
            let Some(prompt) = question["question"].as_str() else {
                valid = false;
                continue;
            };
            let prompt = display(prompt, 640);
            let options = question["options"]
                .as_array()
                .into_iter()
                .flatten()
                .take(8)
                .filter_map(|o| {
                    Some(QuestionOption {
                        label: display(o["label"].as_str()?, 160),
                        description: display(o["description"].as_str().unwrap_or_default(), 160),
                    })
                })
                .collect();
            self.view.questions.push(RequestQuestion {
                question: prompt.clone(),
                header: question["header"].as_str().map(|s| display(s, 100)),
                options,
                multi_select: false,
            });
            match question["id"]
                .as_str()
                .filter(|key| !key.is_empty() && key.len() <= 128)
            {
                Some(id) if ids.insert(id) && !mapping.contains_key(&prompt) => {
                    if prompt.is_empty() {
                        valid = false;
                    }
                    mapping.insert(prompt, id.into());
                }
                _ => valid = false,
            }
        }
        if valid {
            self.question_ids = mapping;
        }
    }

    fn elicitation(&mut self, params: &Value) -> (&'static str, &'static str) {
        let meta = &params["_meta"];
        let connector = meta["connector_id"].as_str();
        let tool = meta["tool_name"].as_str();
        self.detail("server", params["serverName"].as_str(), 120);
        self.detail("connectorName", meta["connector_name"].as_str(), 120);
        self.detail("tool", tool, 120);
        if params["mode"] == "openai/userVerification" {
            return ("elicitation_form", "Codex requested device verification");
        }
        if meta["codex_approval_kind"] == "mcp_tool_call" {
            let persist = |scope: &str| {
                meta["persist"] == scope
                    || meta["persist"]
                        .as_array()
                        .is_some_and(|values| values.iter().any(|v| v == scope))
            };
            let approval = || json!({"action":"accept","content":{}});
            let (kind, title, target) = match connector {
                Some("computer-use") => {
                    let app = meta["tool_params_display"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .find(|p| p["name"] == "app")
                        .and_then(|p| p["value"].as_str())
                        .or_else(|| meta["tool_params"]["app"].as_str());
                    self.detail("appName", app, 120);
                    self.detail("appId", meta["tool_params"]["app"].as_str(), 256);
                    self.detail(
                        "scope",
                        Some(if persist("always") {
                            "This session or always"
                        } else {
                            "This session"
                        }),
                        120,
                    );
                    ("app_access", "Allow Codex to use this app?", app)
                }
                Some("browser-use") => {
                    let origin = meta["origin"]
                        .as_str()
                        .or_else(|| meta["tool_params"]["origin"].as_str());
                    self.detail("origin", origin, 256);
                    self.detail("action", meta["tool_title"].as_str().or(tool), 140);
                    self.detail(
                        "scope",
                        Some(match (persist("session"), persist("always")) {
                            (true, true) => "This request, this session, or always",
                            (true, false) => "This request or this session",
                            (false, true) => "This request or always",
                            (false, false) => "This request",
                        }),
                        120,
                    );
                    ("browser", "Allow Codex to use the browser?", origin)
                }
                _ => ("mcp_tool", "Allow Codex to use this tool?", tool),
            };
            // The installed plugins request confirmation with an empty object.
            // Leave required input and nonstandard consent schemas to the Mac.
            if matches!(
                params["mode"].as_str(),
                Some("form" | "openai/form" | "openaiForm")
            ) && params["requestedSchema"]["type"] == "object"
                && params["requestedSchema"]["properties"]
                    .as_object()
                    .is_some_and(Map::is_empty)
                && params["requestedSchema"]["required"]
                    .as_array()
                    .is_none_or(Vec::is_empty)
            {
                if kind != "app_access" {
                    self.option("allow_once", "Allow this request", approval());
                }
                if persist("session") {
                    self.option(
                        "allow_session",
                        "Allow for this session",
                        json!({"action":"accept","content":{},"_meta":{"persist":"session"}}),
                    );
                }
                if let Some(target) = target
                    .filter(|target| target.len() <= 256)
                    .filter(|_| persist("always"))
                {
                    self.option(
                        "allow_always",
                        &format!("Always allow {target}"),
                        json!({"action":"accept","content":{},"_meta":{"persist":"always"}}),
                    );
                }
            }
            self.elicitation_declines();
            return (kind, title);
        }
        // Other Browser Use forms can ask for account data or sign-in codes.
        if connector == Some("browser-use") {
            self.detail("origin", meta["origin"].as_str(), 256);
            return ("browser", "Codex needs input in the browser");
        }
        match params["mode"].as_str() {
            Some("url") => {
                self.elicitation_declines();
                if let Some(url) = params["url"].as_str().filter(|s| s.len() <= 2048) {
                    if let Ok(uri) = url.parse::<tungstenite::http::Uri>() {
                        if uri.scheme_str() == Some("https")
                            && uri.authority().is_some_and(|a| !a.as_str().contains('@'))
                        {
                            self.view.metadata.details.insert("url".into(), json!(url));
                            self.option(
                                "open_url",
                                "Open link",
                                json!({"action":"accept","content":null}),
                            );
                        }
                    }
                }
                ("elicitation_url", "Codex requested a link")
            }
            Some("form" | "openai/form" | "openaiForm") => {
                if supported_form(&params["requestedSchema"]) {
                    self.view
                        .metadata
                        .details
                        .insert("schema".into(), params["requestedSchema"].clone());
                    self.option("submit_form", "Submit form", Value::Null);
                }
                self.elicitation_declines();
                ("elicitation_form", "Codex requested information")
            }
            _ => ("elicitation_form", "Codex requested information"),
        }
    }

    fn elicitation_declines(&mut self) {
        self.option(
            "deny",
            "Decline",
            json!({"action":"decline","content":null}),
        );
        self.option(
            "cancel",
            "Cancel",
            json!({"action":"cancel","content":null}),
        );
    }

    pub fn answer(&self, input: &Value) -> Option<(Option<String>, Value)> {
        if input["requestId"] != self.view.id || self.view.metadata.answerable != Some(true) {
            return None;
        }
        if !self.question_ids.is_empty() {
            let values = input["answers"].as_object()?;
            if values.len() != self.question_ids.len() {
                return None;
            }
            let mut answers = Map::new();
            for (prompt, id) in &self.question_ids {
                let answer = values.get(prompt)?.as_str()?;
                if answer.trim().is_empty() || answer.len() > 4096 {
                    return None;
                }
                answers.insert(id.clone(), json!({"answers":[answer]}));
            }
            return Some((None, json!({"answers":answers})));
        }
        let option = input["optionId"].as_str()?;
        let mut response = self.replies.get(option)?.clone();
        if option == "submit_form" {
            let form = crate::conversation::validate_form(
                self.view.metadata.details.get("schema"),
                input.get("form"),
            )
            .ok()?;
            response = json!({"action":"accept","content":form});
        }
        Some((Some(option.into()), response))
    }
}

fn complete_permission_scope(profile: &Value) -> bool {
    profile.as_object().is_some_and(|p| {
        !p.is_empty()
            && p.iter().all(|(key, value)| {
                value.is_null()
                    || value.as_object().is_some_and(|v| match key.as_str() {
                        "network" => {
                            v.len() == 1 && v.get("enabled").is_some_and(Value::is_boolean)
                        }
                        "fileSystem" => {
                            !v.is_empty()
                                && v.iter().all(|(key, value)| {
                                    value.is_null()
                                        || match key.as_str() {
                                            "read" | "write" => {
                                                value.as_array().is_some_and(|paths| {
                                                    paths.len() <= 12
                                                        && paths.iter().all(|p| {
                                                            p.as_str().is_some_and(|s| {
                                                                fully_displayed(s, 256)
                                                            })
                                                        })
                                                })
                                            }
                                            "entries" => value.as_array().is_some_and(|entries| {
                                                entries.len() <= 12
                                                    && entries.iter().all(|entry| {
                                                        fully_displayed(
                                                            &entry["path"].to_string(),
                                                            256,
                                                        ) && matches!(
                                                            entry["access"].as_str(),
                                                            Some("read" | "write" | "deny")
                                                        )
                                                    })
                                            }),
                                            _ => false,
                                        }
                                })
                        }
                        _ => false,
                    })
            })
    })
}

fn supported_form(schema: &Value) -> bool {
    schema.to_string().len() <= 8192
        && schema["type"] == "object"
        && schema.as_object().is_some_and(|s| {
            s.keys().all(|k| {
                matches!(
                    k.as_str(),
                    "$schema"
                        | "type"
                        | "properties"
                        | "required"
                        | "additionalProperties"
                        | "title"
                        | "description"
                )
            })
        })
        && schema["properties"].as_object().is_some_and(|properties| {
            properties.len() <= 24
                && schema["required"].as_array().is_none_or(|names| {
                    names
                        .iter()
                        .all(|n| n.as_str().is_some_and(|n| properties.contains_key(n)))
                })
                && properties.iter().all(|(key, field)| {
                    !key.is_empty()
                        && key.len() <= 256
                        && matches!(
                            field["type"].as_str(),
                            Some("string" | "boolean" | "integer" | "number")
                        )
                        && field.as_object().is_some_and(|f| {
                            f.keys().all(|k| {
                                matches!(
                                    k.as_str(),
                                    "type"
                                        | "title"
                                        | "description"
                                        | "enum"
                                        | "minLength"
                                        | "maxLength"
                                        | "minimum"
                                        | "maximum"
                                )
                            })
                        })
                        && field["enum"].as_array().is_none_or(|options| {
                            !options.is_empty()
                                && options.len() <= 32
                                && options.iter().all(|option| {
                                    !option.as_str().is_some_and(str::is_empty)
                                        && valid_form_value(field, option)
                                })
                        })
                })
        })
}

fn valid_form_value(field: &Value, value: &Value) -> bool {
    if field["enum"]
        .as_array()
        .is_some_and(|options| !options.contains(value))
    {
        return false;
    }
    match field["type"].as_str() {
        Some("string") => value.as_str().is_some_and(|s| {
            let len = s.chars().count() as u64;
            s.len() <= 4096
                && len >= field["minLength"].as_u64().unwrap_or(0)
                && len <= field["maxLength"].as_u64().unwrap_or(4096)
        }),
        Some("boolean") => value.is_boolean(),
        Some("integer" | "number") => {
            (field["type"] == "number" || value.is_i64() || value.is_u64())
                && value.as_f64().is_some_and(|n| {
                    field["minimum"].as_f64().is_none_or(|min| n >= min)
                        && field["maximum"].as_f64().is_none_or(|max| n <= max)
                })
        }
        _ => false,
    }
}

fn fully_displayed(s: &str, limit: usize) -> bool {
    s.len() <= limit && !s.chars().any(char::is_control)
}

fn display(input: &str, limit: usize) -> String {
    let mut text = crate::conversation::codex_request_text(input, limit);
    if text.len() > limit {
        let mut end = limit.saturating_sub('…'.len_utf8());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
    text
}

pub(crate) fn decode_presentation(value: &Value) -> Option<Presentation> {
    serde_json::from_value(value.clone()).ok()
}
