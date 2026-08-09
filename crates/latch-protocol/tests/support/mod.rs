//! Loading `fixtures/protocol/` into Rust values.
//!
//! This module deliberately re-implements the fixture format rather than
//! deriving it from the crate under test. The JSON is interpreted here into
//! [`ControlMessage`] values by hand, so a bug in the crate's own serde shape
//! cannot make a fixture agree with it: the fixture says what the bytes mean,
//! and the crate is measured against that reading.
//!
//! The same files are read by the TypeScript codec at M3. If a change makes one
//! implementation pass and the other fail, the fixture is doing its job.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use latch_protocol::control::{
    AttachMode, Attachment, ClientInfo, ControlMessage, SessionInfo, SessionState, Size,
};
use latch_protocol::frame::FrameType;
use serde_json::Value;

/// One fixture case.
pub struct Case {
    /// File stem, which is also the `name` field.
    pub name: String,
    /// Why the case exists. Printed on failure, because a bare case name is not
    /// enough to know what a reviewer was protecting.
    pub description: String,
    /// Protocol version the case is written for.
    pub protocol: u32,
    /// What the case asserts.
    pub kind: CaseKind,
}

/// A case either specifies bytes that must decode, or bytes that must not.
pub enum CaseKind {
    /// The bytes are a frame, and must round-trip.
    Accept(Accept),
    /// The bytes must be refused with a named error.
    Reject(Reject),
}

/// A case whose bytes must decode and re-encode identically.
pub struct Accept {
    /// Expected frame type.
    pub frame_type: FrameType,
    /// The complete frame: tag, length, payload.
    pub encoded: Vec<u8>,
    /// The payload alone.
    pub payload: Vec<u8>,
    /// The control message the payload means. `None` for `terminal.*`, which
    /// carry raw bytes and are never structurally decoded.
    pub decoded: Option<ControlMessage>,
    /// For `session.update`, what each optional field is meant to instruct.
    pub merge: Option<Merge>,
}

/// A case whose bytes must be refused.
pub struct Reject {
    /// The bytes to feed.
    pub encoded: Vec<u8>,
    /// The stable error name the codec must produce.
    pub error: String,
    /// Which layer must refuse them.
    pub layer: Layer,
}

/// Which layer a rejection belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layer {
    /// The bytes are not a decodable frame.
    Frame,
    /// The framing is fine; the control payload inside it is not.
    Control,
}

/// What a `session.update` field is instructing the receiver to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Disposition {
    /// Key omitted — leave the client's current value alone.
    Absent,
    /// Key present with a value — replace.
    Set,
    /// Key present and null — clear. Only `title` can be cleared.
    Cleared,
}

/// The `merge` annotation on a `session.update` case.
#[derive(Debug, Clone, Copy)]
pub struct Merge {
    /// Disposition of `state`.
    pub state: Disposition,
    /// Disposition of `attachments`.
    pub attachments: Disposition,
    /// Disposition of `title`.
    pub title: Disposition,
}

/// Path to `fixtures/protocol/`.
pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/protocol")
        .canonicalize()
        .expect("fixtures/protocol must exist; it is the protocol's contract")
}

/// Loads every case, sorted by name so failures report in a stable order.
pub fn load_cases() -> Vec<Case> {
    let dir = fixture_dir();
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|e| e.expect("directory entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();

    assert!(
        !paths.is_empty(),
        "no fixtures in {} — the suite would pass vacuously",
        dir.display()
    );

    paths.iter().map(|p| load_case(p)).collect()
}

fn load_case(path: &Path) -> Case {
    let text =
        fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let v: Value = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{} is not valid JSON: {e}", path.display()));

    let name = str_field(&v, "name", path).to_string();
    let stem = path.file_stem().unwrap().to_string_lossy();
    assert_eq!(
        name,
        stem,
        "{}: the `name` field and the file name must agree, or a failure report \
         points at a file that does not exist",
        path.display()
    );

    let description = str_field(&v, "description", path).to_string();
    let protocol =
        v.get("protocol")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("{}: missing `protocol`", path.display())) as u32;

    let kind = match (v.get("frame"), v.get("reject")) {
        (Some(frame), None) => CaseKind::Accept(load_accept(frame, &v, path)),
        (None, Some(reject)) => CaseKind::Reject(load_reject(reject, path)),
        _ => panic!(
            "{}: a case has exactly one of `frame` or `reject`",
            path.display()
        ),
    };

    Case {
        name,
        description,
        protocol,
        kind,
    }
}

fn load_accept(frame: &Value, case: &Value, path: &Path) -> Accept {
    let frame_type = match str_field(frame, "type", path) {
        "terminal.output" => FrameType::TerminalOutput,
        "terminal.input" => FrameType::TerminalInput,
        "control" => FrameType::Control,
        other => panic!("{}: unknown frame type {other:?}", path.display()),
    };

    let encoded = bytes_field(frame, "encoded", "encoded_parts", path);
    let payload = bytes_field(frame, "payload", "payload_parts", path);

    let decoded = frame.get("decoded").map(|d| control_from_json(d, path));
    if frame_type == FrameType::Control {
        assert!(
            decoded.is_some(),
            "{}: a control case must state its decoded message",
            path.display()
        );
    } else {
        assert!(
            decoded.is_none(),
            "{}: terminal.* frames carry raw bytes and have no structured decode",
            path.display()
        );
    }

    let merge = case.get("merge").map(|m| load_merge(m, path));

    Accept {
        frame_type,
        encoded,
        payload,
        decoded,
        merge,
    }
}

fn load_reject(reject: &Value, path: &Path) -> Reject {
    let layer = match str_field(reject, "layer", path) {
        "frame" => Layer::Frame,
        "control" => Layer::Control,
        other => panic!("{}: unknown rejection layer {other:?}", path.display()),
    };
    Reject {
        encoded: bytes_field(reject, "encoded", "encoded_parts", path),
        error: str_field(reject, "error", path).to_string(),
        layer,
    }
}

fn load_merge(v: &Value, path: &Path) -> Merge {
    let disposition = |key: &str| match str_field(v, key, path) {
        "absent" => Disposition::Absent,
        "set" => Disposition::Set,
        "cleared" => Disposition::Cleared,
        other => panic!("{}: unknown disposition {other:?}", path.display()),
    };
    let merge = Merge {
        state: disposition("state"),
        attachments: disposition("attachments"),
        title: disposition("title"),
    };
    for (field, d) in [("state", merge.state), ("attachments", merge.attachments)] {
        assert_ne!(
            d,
            Disposition::Cleared,
            "{}: `{field}` has no cleared form — only `title` distinguishes null from absent",
            path.display()
        );
    }
    merge
}

// ------------------------------------------------------------------ decoding

fn str_field<'a>(v: &'a Value, key: &str, path: &Path) -> &'a str {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{}: missing string field `{key}`", path.display()))
}

/// Reads a byte field in either the inline-hex or the parts form.
fn bytes_field(v: &Value, inline: &str, parts: &str, path: &Path) -> Vec<u8> {
    match (v.get(inline), v.get(parts)) {
        (Some(hex), None) => unhex(
            hex.as_str()
                .unwrap_or_else(|| panic!("{}: `{inline}` must be a hex string", path.display())),
            path,
        ),
        (None, Some(list)) => {
            let list = list
                .as_array()
                .unwrap_or_else(|| panic!("{}: `{parts}` must be an array", path.display()));
            let mut out = Vec::new();
            for part in list {
                if let Some(hex) = part.get("hex").and_then(Value::as_str) {
                    out.extend_from_slice(&unhex(hex, path));
                } else if let Some(rep) = part.get("repeat") {
                    let hex = rep
                        .get("hex")
                        .and_then(Value::as_str)
                        .unwrap_or_else(|| panic!("{}: repeat needs `hex`", path.display()));
                    let count = rep
                        .get("count")
                        .and_then(Value::as_u64)
                        .unwrap_or_else(|| panic!("{}: repeat needs `count`", path.display()));
                    let unit = unhex(hex, path);
                    out.reserve(unit.len() * count as usize);
                    for _ in 0..count {
                        out.extend_from_slice(&unit);
                    }
                } else {
                    panic!("{}: a part is either `hex` or `repeat`", path.display());
                }
            }
            out
        }
        _ => panic!(
            "{}: exactly one of `{inline}` or `{parts}` must be present",
            path.display()
        ),
    }
}

fn unhex(s: &str, path: &Path) -> Vec<u8> {
    assert!(
        s.len() % 2 == 0,
        "{}: hex string has an odd length",
        path.display()
    );
    assert!(
        s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{}: hex must be lowercase and contain no separators",
        path.display()
    );
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("validated hex"))
        .collect()
}

// -------------------------------------------------- JSON -> ControlMessage

fn control_from_json(v: &Value, path: &Path) -> ControlMessage {
    let obj = v
        .as_object()
        .unwrap_or_else(|| panic!("{}: `decoded` must be an object", path.display()));
    let t = obj
        .get("t")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{}: `decoded` has no `t`", path.display()));

    let ctx = |field: &str| format!("{}: `{t}`.`{field}`", path.display());

    match t {
        "attach" => ControlMessage::Attach {
            protocol: u32_at(v, "protocol", &ctx("protocol")),
            mode: mode_at(v, "mode", &ctx("mode")),
            steal: bool_at(v, "steal", &ctx("steal")),
            client: client_at(v.get("client"), &ctx("client")),
            size: size_at(v.get("size"), &ctx("size")),
        },
        "attached" => ControlMessage::Attached {
            protocol: u32_at(v, "protocol", &ctx("protocol")),
            session: session_at(v.get("session"), &ctx("session")),
            controller: opt_string_at(v, "controller", &ctx("controller")),
            attachments: attachments_at(v.get("attachments"), &ctx("attachments")),
        },
        "resize" => ControlMessage::Resize {
            cols: u16_at(v, "cols", &ctx("cols")),
            rows: u16_at(v, "rows", &ctx("rows")),
            pin: obj.get("pin").map(|_| bool_at(v, "pin", &ctx("pin"))),
        },
        "control.request" => ControlMessage::ControlRequest {
            steal: bool_at(v, "steal", &ctx("steal")),
        },
        "control.state" => ControlMessage::ControlState {
            controller_id: opt_string_at(v, "controller_id", &ctx("controller_id")),
            controller_label: opt_string_at(v, "controller_label", &ctx("controller_label")),
        },
        // The only place where "key missing" and "key present and null" must
        // decode differently, so it is spelled out rather than folded into a
        // helper that would quietly treat them alike.
        "session.update" => ControlMessage::SessionUpdate {
            state: obj.get("state").map(|s| state_from(s, &ctx("state"))),
            attachments: obj
                .get("attachments")
                .map(|a| attachments_at(Some(a), &ctx("attachments"))),
            title: obj.get("title").map(|t| match t {
                Value::Null => None,
                other => Some(
                    other
                        .as_str()
                        .unwrap_or_else(|| panic!("{}: expected a string or null", ctx("title")))
                        .to_string(),
                ),
            }),
            force: obj.get("force").map(|_| bool_at(v, "force", &ctx("force"))),
        },
        "session.exited" => ControlMessage::SessionExited {
            code: match obj.get("code") {
                Some(Value::Null) | None => None,
                Some(c) => Some(
                    c.as_i64()
                        .unwrap_or_else(|| panic!("{}: expected an integer", ctx("code")))
                        as i32,
                ),
            },
            signal: opt_string_at(v, "signal", &ctx("signal")),
            at: string_at(v, "at", &ctx("at")),
        },
        "error" => ControlMessage::Error {
            code: string_at(v, "code", &ctx("code")),
            message: string_at(v, "message", &ctx("message")),
        },
        other => panic!(
            "{}: fixture names unknown message {other:?}",
            path.display()
        ),
    }
}

fn u32_at(v: &Value, key: &str, ctx: &str) -> u32 {
    v.get(key)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("{ctx}: expected an unsigned integer")) as u32
}

fn u16_at(v: &Value, key: &str, ctx: &str) -> u16 {
    let n = v
        .get(key)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("{ctx}: expected an unsigned integer"));
    u16::try_from(n).unwrap_or_else(|_| panic!("{ctx}: {n} does not fit in u16"))
}

fn bool_at(v: &Value, key: &str, ctx: &str) -> bool {
    v.get(key)
        .and_then(Value::as_bool)
        .unwrap_or_else(|| panic!("{ctx}: expected a boolean"))
}

fn string_at(v: &Value, key: &str, ctx: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{ctx}: expected a string"))
        .to_string()
}

fn opt_string_at(v: &Value, key: &str, ctx: &str) -> Option<String> {
    match v.get(key) {
        None | Some(Value::Null) => None,
        Some(other) => Some(
            other
                .as_str()
                .unwrap_or_else(|| panic!("{ctx}: expected a string or null"))
                .to_string(),
        ),
    }
}

fn mode_at(v: &Value, key: &str, ctx: &str) -> AttachMode {
    let s = v
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{ctx}: expected a string"));
    AttachMode::from_wire(s).unwrap_or_else(|| panic!("{ctx}: unknown attach mode {s:?}"))
}

fn state_from(v: &Value, ctx: &str) -> SessionState {
    let s = v
        .as_str()
        .unwrap_or_else(|| panic!("{ctx}: expected a string"));
    SessionState::from_wire(s).unwrap_or_else(|| panic!("{ctx}: unknown session state {s:?}"))
}

fn client_at(v: Option<&Value>, ctx: &str) -> ClientInfo {
    let v = v.unwrap_or_else(|| panic!("{ctx}: missing"));
    ClientInfo {
        kind: string_at(v, "kind", ctx),
        name: string_at(v, "name", ctx),
    }
}

fn size_at(v: Option<&Value>, ctx: &str) -> Size {
    let v = v.unwrap_or_else(|| panic!("{ctx}: missing"));
    Size {
        cols: u16_at(v, "cols", ctx),
        rows: u16_at(v, "rows", ctx),
    }
}

fn session_at(v: Option<&Value>, ctx: &str) -> SessionInfo {
    let v = v.unwrap_or_else(|| panic!("{ctx}: missing"));
    SessionInfo {
        id: string_at(v, "id", ctx),
        name: string_at(v, "name", ctx),
        title: opt_string_at(v, "title", ctx),
        state: state_from(
            v.get("state").unwrap_or_else(|| panic!("{ctx}: no state")),
            ctx,
        ),
        size: size_at(v.get("size"), ctx),
    }
}

fn attachments_at(v: Option<&Value>, ctx: &str) -> Vec<Attachment> {
    let list = v
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{ctx}: expected an array"));
    list.iter()
        .map(|a| Attachment {
            id: string_at(a, "id", ctx),
            mode: mode_at(a, "mode", ctx),
            client: client_at(a.get("client"), ctx),
        })
        .collect()
}
