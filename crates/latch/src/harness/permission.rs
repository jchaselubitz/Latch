//! Shared permission-request derivation for `latch events` and `latch send --resolve`.

use serde_json::Value;

use super::generated::AwaitingInputKind;

const MISSING_TIMESTAMP: &str = "1970-01-01T00:00:00Z";

/// Fields both the event ledger and send-resolve must agree on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DerivedRequest {
    /// Stable request identifier advertised by `latch events`.
    pub request_id: String,
    /// Permission or structured question.
    pub kind: AwaitingInputKind,
    /// Human-readable prompt.
    pub prompt: String,
    /// Closed choice set when the harness supplied one.
    pub choices: Option<Vec<String>>,
}

/// Derives a pending input request from a Claude permission hook or transcript record.
pub(super) fn from_record(record: &Value) -> Option<DerivedRequest> {
    if !is_permission_request(record) {
        return None;
    }
    let tool_input = record
        .get("tool_input")
        .or_else(|| record.get("toolInput"))
        .unwrap_or(&Value::Null);
    let question = string(record, &["tool_name", "toolName"]) == Some("AskUserQuestion");
    Some(DerivedRequest {
        request_id: request_id(record),
        kind: if question {
            AwaitingInputKind::Question
        } else {
            AwaitingInputKind::Permission
        },
        prompt: if question {
            question_prompt(tool_input)
        } else {
            permission_prompt(record)
        },
        choices: if question {
            question_choices(tool_input)
        } else {
            permission_choices(record)
        },
    })
}

fn is_permission_request(record: &Value) -> bool {
    string(record, &["hook_event_name", "hookEventName"]) == Some("PermissionRequest")
        || string(record, &["type"]) == Some("permission_request")
}

fn request_id(record: &Value) -> String {
    string(record, &["request_id", "requestId", "tool_use_id", "uuid"])
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "permission:{}:{}",
                string(record, &["tool_name", "toolName"]).unwrap_or("tool"),
                string(record, &["timestamp", "at"]).unwrap_or(MISSING_TIMESTAMP),
            )
        })
}

pub(super) fn question_prompt(input: &Value) -> String {
    let prompt = input
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|question| string(question, &["question", "header"]))
        .collect::<Vec<_>>()
        .join("\n");
    if prompt.is_empty() {
        "Claude is waiting for an answer.".to_owned()
    } else {
        prompt
    }
}

pub(super) fn question_choices(input: &Value) -> Option<Vec<String>> {
    let choices = input
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|question| {
            question
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|option| string(option, &["label"]).map(str::to_owned))
        .collect::<Vec<_>>();
    (!choices.is_empty()).then_some(choices)
}

fn permission_prompt(record: &Value) -> String {
    record
        .pointer("/tool_input/description")
        .or_else(|| record.pointer("/toolInput/description"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            format!(
                "Allow {}?",
                string(record, &["tool_name", "toolName"]).unwrap_or("this tool")
            )
        })
}

fn permission_choices(record: &Value) -> Option<Vec<String>> {
    let suggestions = record
        .get("permission_suggestions")
        .or_else(|| record.get("permissionSuggestions"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|choice| string(choice, &["description"]).map(str::to_owned))
        .collect::<Vec<_>>();
    let mut choices = vec!["Allow once".to_owned()];
    for suggestion in suggestions {
        if !choices.iter().any(|choice| choice == &suggestion) && suggestion != "Deny" {
            choices.push(suggestion);
        }
    }
    choices.push("Deny".to_owned());
    Some(choices)
}

fn string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_timestamp_fallback_is_stable() {
        let record = serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "tool_input": { "description": "Run tests" }
        });
        let derived = from_record(&record).expect("permission record");
        assert_eq!(derived.request_id, "permission:Bash:1970-01-01T00:00:00Z");
        assert_eq!(derived.kind, AwaitingInputKind::Permission);
        assert_eq!(derived.prompt, "Run tests");
        assert_eq!(
            derived.choices,
            Some(vec!["Allow once".to_owned(), "Deny".to_owned()])
        );
    }
}
