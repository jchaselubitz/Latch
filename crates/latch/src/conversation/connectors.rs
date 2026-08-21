//! Agent-specific adapters.  Nothing outside this module knows transcript
//! fields, hook keys, or terminal conventions for Claude or Codex.

mod jsonl;

pub use jsonl::connector_for_session;
