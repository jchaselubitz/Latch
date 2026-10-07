//! Codex rollout records. Only completed conversation items are user-visible;
//! raw response items carry injected setup context and stay out.
use super::*;

impl JsonlConnector {
    /// Codex's completed conversation item is the user-visible source. Raw
    /// response items also contain injected setup context and must stay out.
    pub(super) fn codex_conversation_item(
        &mut self,
        object: &serde_json::Map<String, Value>,
        ordinal: u64,
    ) -> Vec<ConnectorMutation> {
        let Some(payload) = object.get("payload").and_then(Value::as_object) else {
            return Vec::new();
        };
        if string(payload, "type").as_deref() != Some("item_completed") {
            return Vec::new();
        }
        let Some(item) = payload.get("item").and_then(Value::as_object) else {
            return Vec::new();
        };
        let role = match string(item, "type").as_deref() {
            Some("UserMessage") => MessageRole::User,
            Some("AgentMessage") => MessageRole::Assistant,
            _ => return Vec::new(),
        };
        let text = item
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|part| {
                let part = part.as_object()?;
                matches!(string(part, "type").as_deref(), Some("text" | "Text"))
                    .then(|| string(part, "text"))
                    .flatten()
            })
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            return Vec::new();
        }
        vec![ConnectorMutation::Upsert(ObservedItem {
            id: item
                .get("id")
                .and_then(Value::as_str)
                .map(ConversationItemId::native)
                .unwrap_or_else(|| {
                    ConversationItemId::derived("codex", "message", &ordinal.to_string())
                }),
            created_at: string(object, "timestamp")
                .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
            kind: ConversationItemKind::Message {
                role,
                text: bounded_message_text(text),
                status: MessageStatus::Observed,
            },
        })]
    }
}
