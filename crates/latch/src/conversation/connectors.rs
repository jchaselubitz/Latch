//! Agent-specific adapters.  Nothing outside this module knows transcript
//! fields, hook keys, or terminal conventions for Claude, Codex, or Cursor.

mod jsonl;

pub use jsonl::{connector_for_session, connector_kind};

pub(crate) use jsonl::{codex_request_text, validate_form};

#[cfg(test)]
pub(crate) use jsonl::JsonlConnector;
