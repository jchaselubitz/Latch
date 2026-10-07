//! Bounds and redacts provider text before it reaches a summary: secret-shaped
//! tokens, environment assignments, and home directories never leave here.

/// Upper bound, in characters, of any tool summary a connector emits. The
/// contract allows 16384 and the Hub rejects items over 32 KiB; a summary is a
/// one-line description, so it stays far below both.
pub(super) const MAX_TOOL_SUMMARY_CHARS: usize = 320;

/// Only this much of a provider string is ever scanned for a summary, so an
/// unexpectedly large record costs a bounded amount of work.
const MAX_SUMMARY_SCAN_BYTES: usize = 4096;

const REDACTED: &str = "[redacted]";

/// Bounds and sanitizes a complete tool summary.
pub(super) fn sanitize_summary(text: &str) -> String {
    sanitize_part(text, MAX_TOOL_SUMMARY_CHARS)
}

/// Collapses `text` to one line of at most `max_chars` characters with
/// secret-shaped tokens, environment assignments, and home directories
/// redacted. Idempotent, so an already-sanitized part may be sanitized again.
pub(super) fn sanitize_part(text: &str, max_chars: usize) -> String {
    let mut end = text.len().min(MAX_SUMMARY_SCAN_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::new();
    let mut redact_next = false;
    for token in text[..end].split(|c: char| c.is_whitespace() || c.is_control()) {
        if token.is_empty() {
            continue;
        }
        let token = if redact_next {
            REDACTED.to_owned()
        } else {
            redact_token(token)
        };
        redact_next = token.eq_ignore_ascii_case("bearer") || token.eq_ignore_ascii_case("basic");
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&token);
    }
    if out.chars().count() > max_chars || end < text.len() {
        let mut bounded: String = out.chars().take(max_chars.saturating_sub(1)).collect();
        bounded.push('…');
        return bounded;
    }
    out
}

fn redact_token(token: &str) -> String {
    let token = redact_home(token);
    if let Some((key, value)) = token.split_once(['=', ':']) {
        let key_name = key.trim_start_matches(['-', '"', '\'', '{', '(']);
        let key_name = key_name.trim_end_matches(['"', '\'']);
        if !value.is_empty()
            && value != REDACTED
            && (is_env_name(key_name) || is_secret_key(key_name))
        {
            let separator = &token[key.len()..key.len() + 1];
            return format!("{key}{separator}{REDACTED}");
        }
    }
    let bare = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if is_secret_shaped(bare) {
        return token.replace(bare, REDACTED);
    }
    token
}

/// `/Users/<name>` and `/home/<name>` become `~`, so paths keep their useful
/// tail without naming the account that owns them.
fn redact_home(token: &str) -> String {
    let mut out = token.to_owned();
    for root in ["/Users/", "/home/"] {
        while let Some(start) = out.find(root) {
            let rest = &out[start + root.len()..];
            let user_len = rest.find('/').unwrap_or(rest.len());
            if user_len == 0 {
                break;
            }
            out.replace_range(start..start + root.len() + user_len, "~");
        }
    }
    out
}

/// `NAME=value` with an upper-case shell-variable name.
fn is_env_name(key: &str) -> bool {
    let key = key.strip_prefix('$').unwrap_or(key);
    key.len() >= 2
        && key.starts_with(|c: char| c.is_ascii_uppercase() || c == '_')
        && key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "secret",
        "token",
        "apikey",
        "api_key",
        "api-key",
        "authorization",
        "credential",
        "private_key",
    ]
    .iter()
    .any(|marker| key.contains(marker))
}

/// Provider credentials with a recognizable prefix, and long opaque strings
/// that are more likely to be a key than anything a person would read.
fn is_secret_shaped(token: &str) -> bool {
    const PREFIXES: [&str; 13] = [
        "sk-",
        "sk_live_",
        "sk_test_",
        "ghp_",
        "gho_",
        "ghs_",
        "ghu_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "glpat-",
        "AKIA",
        "eyJ",
    ];
    if token.len() >= 16 && PREFIXES.iter().any(|prefix| token.starts_with(prefix)) {
        return true;
    }
    token.len() >= 32
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '=' | '.'))
        && token.chars().any(|c| c.is_ascii_digit())
        && token.chars().any(|c| c.is_ascii_alphabetic())
}
