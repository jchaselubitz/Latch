//! Claude Code Mode (`CLAUDE_CODE_EXPERIMENTAL_CODE_MODE`): the model's one
//! tool is `REPL`, whose JavaScript makes the real Bash, Read and Edit calls.
//!
//! What the transcript holds for one script (captured live on Claude Code
//! 2.1.330, `fixtures/conversation/claude/code-mode/`):
//! - the assistant `tool_use` named `REPL`, with `input.code` and an optional
//!   `input.description`;
//! - `host-row` records written while the script runs, one per in-script call
//!   started or finished, each with `ranByToolUseId` naming the REPL call;
//! - the REPL `tool_result`, whose `toolUseResult` has `code`, `result`,
//!   `stdout`, `stderr`, `toolsUsed`, and `error` or `refusedCalls` when they
//!   apply;
//! - after it, a virtual assistant/user pair per in-script call
//!   (`isVirtual`, `ranByToolUseId` on the call, ids `toolu_cc_…`).
//!
//! The script is shown as one Tool item named "Code" whose summary says which
//! tools it used and which calls were refused. The code itself is never
//! shown. The in-script calls get no items of their own: they still feed the
//! task list and subagent links, under the Code item.
use super::*;

/// The tool name Claude gives a Code Mode script.
pub(super) const SCRIPT_TOOL: &str = "REPL";
/// The name the conversation shows for it.
pub(super) const CODE_ITEM_NAME: &str = "Code";
/// Scripts remembered at once, for their progress and in-script calls.
const MAX_SCRIPTS: usize = 16;
/// In-script calls remembered per script, for their results.
const MAX_SCRIPT_CALLS: usize = 128;
/// Tool names a summary lists before it counts the rest.
const MAX_LISTED_TOOLS: usize = 6;
const MAX_DESCRIPTION_CHARS: usize = 160;
const MAX_ERROR_CHARS: usize = 160;

/// One REPL call, from its `tool_use` until it falls out of the window. The
/// engine can write the first `host-row` records of a script before the
/// script's own `tool_use`; until that arrives, `item_id` is empty.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(super) struct CodeScript {
    call_id: String,
    item_id: String,
    /// The assistant record that made the call.
    message_id: String,
    created_at: String,
    description: String,
    /// Tools the script has started so far, in first-use order.
    used: Vec<String>,
    /// In-script calls by id, with their tool name.
    calls: Vec<(String, String)>,
    finished: bool,
}

/// Whether a call id is one the engine gives a call made inside a script
/// (`repl_`, `toolu_cc_`), or the copy of a script's error (`repl-error-`).
fn is_script_call_id(id: &str) -> bool {
    id.starts_with("repl_") || id.starts_with("repl-") || id.starts_with("toolu_cc_")
}

/// A record the engine wrote for a call a script made, after the script.
pub(super) fn is_script_record(object: &serde_json::Map<String, Value>) -> bool {
    if object.get("isVirtual").and_then(Value::as_bool) != Some(true) {
        return false;
    }
    if object.contains_key("ranByToolUseId") || object.contains_key("scriptErrorCopy") {
        return true;
    }
    object
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                string_value(block, "tool_use_id")
                    .or_else(|| string_value(block, "id"))
                    .is_some_and(|id| is_script_call_id(&id))
            })
        })
}

/// What the script is for, in the model's words, or a plain stand-in. The
/// code itself is never described.
pub(super) fn script_description(input: Option<&Value>) -> String {
    input
        .and_then(|input| input.get("description"))
        .and_then(Value::as_str)
        .map(|text| sanitize_part(text, MAX_DESCRIPTION_CHARS))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "Script".to_owned())
}

fn tool_list(names: &[String]) -> String {
    let mut listed = names
        .iter()
        .take(MAX_LISTED_TOOLS)
        .map(|name| sanitize_part(name, 64))
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > MAX_LISTED_TOOLS {
        listed.push_str(&format!(" and {} more", names.len() - MAX_LISTED_TOOLS));
    }
    listed
}

/// The summary of a script still running.
fn running_summary(description: &str, used: &[String]) -> String {
    if used.is_empty() {
        description.to_owned()
    } else {
        sanitize_summary(&format!("{description} · running {}", tool_list(used)))
    }
}

/// The tools named in the engine's refusal note, one per refused call:
/// `- "Bash", refused: "…"`.
fn refused_tools(note: &str) -> Vec<String> {
    note.lines()
        .filter_map(|line| line.trim().strip_prefix("- \""))
        .filter_map(|rest| rest.split_once('"').map(|(name, _)| name.to_owned()))
        .filter(|name| !name.is_empty())
        .collect()
}

/// The text between `<tag>` and `</tag>` in the script's `tool_result`.
fn tagged<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let start = text.find(&format!("<{tag}>"))? + tag.len() + 2;
    let end = text[start..].find(&format!("</{tag}>"))? + start;
    Some(&text[start..end])
}

fn result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| string_value(part, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The status and summary of a finished script: what it used, what was
/// refused, and the first line of its error. `outcome` is the record's
/// `toolUseResult`, when the record carries only this result; otherwise the
/// same facts are read from the tagged `tool_result` text.
pub(super) fn script_outcome(
    description: &str,
    block: &Value,
    outcome: Option<&Value>,
) -> (ToolStatus, String) {
    let text = result_text(block);
    let names = |value: Option<&Value>| -> Vec<String> {
        value
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    let used = match outcome.filter(|outcome| outcome.get("toolsUsed").is_some()) {
        Some(outcome) => names(outcome.get("toolsUsed")),
        None => tagged(&text, "tools-used-before-summary")
            .map(|list| {
                list.split(',')
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    };
    let refused = outcome
        .and_then(|outcome| outcome.get("refusedCalls"))
        .and_then(Value::as_str)
        .or_else(|| tagged(&text, "refused"))
        .map(refused_tools)
        .unwrap_or_default();
    let error = outcome
        .and_then(|outcome| outcome.get("error"))
        .and_then(Value::as_str)
        .or_else(|| tagged(&text, "error"))
        .map(|error| {
            error
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(|line| sanitize_part(line, MAX_ERROR_CHARS))
                .unwrap_or_default()
        });
    let failed = error.is_some() || block.get("is_error").and_then(Value::as_bool) == Some(true);

    let mut parts = vec![description.to_owned()];
    if used.is_empty() {
        parts.push("no tool calls".to_owned());
    } else {
        parts.push(format!("used {}", tool_list(&used)));
    }
    if !refused.is_empty() {
        let count = refused.len();
        parts.push(format!(
            "{count} call{} refused ({})",
            if count == 1 { "" } else { "s" },
            tool_list(&refused)
        ));
    }
    if failed {
        parts.push(match error.filter(|error| !error.is_empty()) {
            Some(error) => format!("failed: {error}"),
            None => "failed".to_owned(),
        });
    }
    let status = if failed {
        ToolStatus::Failed
    } else {
        ToolStatus::Succeeded
    };
    (status, sanitize_summary(&parts.join(" · ")))
}

impl JsonlConnector {
    /// A REPL `tool_use` of the main transcript: the Code item, running.
    pub(super) fn code_script_started(
        &mut self,
        call_id: &str,
        item_id: &str,
        message_id: &str,
        input: Option<&Value>,
        at: &str,
    ) -> ConversationItemKind {
        let description = script_description(input);
        let scripts = &mut self.activity.scripts;
        // Calls the script started before its `tool_use` was written.
        let used = scripts
            .iter()
            .position(|script| script.call_id == call_id)
            .map(|index| scripts.remove(index).used)
            .unwrap_or_default();
        if scripts.len() >= MAX_SCRIPTS {
            scripts.remove(0);
        }
        let summary = running_summary(&description, &used);
        scripts.push(CodeScript {
            call_id: call_id.to_owned(),
            item_id: item_id.to_owned(),
            message_id: message_id.to_owned(),
            created_at: at.to_owned(),
            description,
            used,
            ..CodeScript::default()
        });
        ConversationItemKind::Tool {
            name: CODE_ITEM_NAME.to_owned(),
            summary,
            status: ToolStatus::Running,
            parent_message_id: Some(ConversationItemId::native(message_id)),
        }
    }

    /// The REPL `tool_result`: the Code item, settled.
    pub(super) fn code_script_finished(
        &mut self,
        call_id: &str,
        block: &Value,
        outcome: Option<&Value>,
    ) -> ConversationItemKind {
        let script = self
            .activity
            .scripts
            .iter_mut()
            .find(|script| script.call_id == call_id);
        let description = script
            .as_ref()
            .map(|script| script.description.clone())
            .unwrap_or_else(|| "Script".to_owned());
        if let Some(script) = script {
            script.finished = true;
        }
        let (status, summary) = script_outcome(&description, block, outcome);
        ConversationItemKind::Tool {
            name: CODE_ITEM_NAME.to_owned(),
            summary,
            status,
            parent_message_id: None,
        }
    }

    /// A `host-row` record: an in-script call started or finished while the
    /// script runs. A newly used tool is added to the Code item's summary.
    pub(super) fn code_host_row(
        &mut self,
        object: &serde_json::Map<String, Value>,
    ) -> Vec<ConnectorMutation> {
        // A subagent's scripts belong to its own transcript.
        if object.contains_key("agentId") {
            return Vec::new();
        }
        let Some(script_id) = string(object, "ranByToolUseId") else {
            return Vec::new();
        };
        let Some(block) = object.get("block") else {
            return Vec::new();
        };
        if string_value(block, "type").as_deref() != Some("tool_use") {
            return Vec::new();
        }
        let Some(name) = string_value(block, "name") else {
            return Vec::new();
        };
        let scripts = &mut self.activity.scripts;
        let index = match scripts
            .iter()
            .position(|script| script.call_id == script_id)
        {
            Some(index) => index,
            None => {
                // The script's own `tool_use` is not written yet.
                if scripts.len() >= MAX_SCRIPTS {
                    scripts.remove(0);
                }
                scripts.push(CodeScript {
                    call_id: script_id,
                    ..CodeScript::default()
                });
                scripts.len() - 1
            }
        };
        let script = &mut scripts[index];
        if script.finished || script.used.contains(&name) {
            return Vec::new();
        }
        script.used.push(name);
        if script.item_id.is_empty() {
            return Vec::new();
        }
        vec![upsert(
            &script.item_id,
            script.created_at.clone(),
            ConversationItemKind::Tool {
                name: CODE_ITEM_NAME.to_owned(),
                summary: running_summary(&script.description, &script.used),
                status: ToolStatus::Running,
                parent_message_id: Some(ConversationItemId::native(script.message_id.clone())),
            },
        )]
    }

    /// A virtual record of an in-script call, written after its script. It
    /// adds no item: the Code item already speaks for it. A task or subagent
    /// call still updates the task list or links the subagent, under the
    /// Code item. Returns `None` for any other record.
    pub(super) fn code_script_record(
        &mut self,
        object: &serde_json::Map<String, Value>,
        event: &str,
        at: &str,
    ) -> Option<Vec<ConnectorMutation>> {
        if !is_script_record(object) {
            return None;
        }
        let blocks = object
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut mutations = Vec::new();
        match event {
            "assistant" => {
                let script_id = string(object, "ranByToolUseId");
                for block in &blocks {
                    if string_value(block, "type").as_deref() != Some("tool_use") {
                        continue;
                    }
                    let (Some(call_id), Some(name)) =
                        (string_value(block, "id"), string_value(block, "name"))
                    else {
                        continue;
                    };
                    let Some(script) = self
                        .activity
                        .scripts
                        .iter_mut()
                        .find(|script| script_id.as_deref() == Some(script.call_id.as_str()))
                    else {
                        continue;
                    };
                    if script.calls.len() >= MAX_SCRIPT_CALLS {
                        script.calls.remove(0);
                    }
                    script.calls.push((call_id.clone(), name.clone()));
                    let item_id = script.item_id.clone();
                    mutations.extend(self.activity_tool_use(
                        &name,
                        &call_id,
                        &item_id,
                        block.get("input"),
                        at,
                        None,
                    ));
                }
            }
            "user" => {
                for block in &blocks {
                    if string_value(block, "type").as_deref() != Some("tool_result") {
                        continue;
                    }
                    let Some(call_id) = string_value(block, "tool_use_id") else {
                        continue;
                    };
                    let name = self.activity.scripts.iter_mut().find_map(|script| {
                        let index = script.calls.iter().position(|(id, _)| id == &call_id)?;
                        Some(script.calls.remove(index).1)
                    });
                    if let Some(name) = name {
                        let is_error = block
                            .get("is_error")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        mutations.extend(self.activity_tool_result(&name, object, is_error, at));
                    }
                }
            }
            _ => {}
        }
        Some(mutations)
    }
}
