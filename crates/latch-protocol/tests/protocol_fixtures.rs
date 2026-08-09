//! Conformance against `fixtures/protocol/`.
//!
//! Every case is asserted in both directions: `encoded` must decode to
//! `decoded`, and `decoded` must re-encode to `encoded` byte for byte. Only one
//! of those is a round trip through this crate; the other is a comparison
//! against bytes written by something that is not this crate. A codec that is
//! merely self-consistent passes the first and can still be wrong on the wire,
//! and "wrong on the wire" is not discovered until a TypeScript client at M3
//! cannot talk to a Rust worker.
//!
//! Rejection cases carry the same weight as happy paths. A decoder that
//! misreads a length does not drop one frame — it desynchronizes every frame
//! after it, and the corruption surfaces somewhere unrelated much later. So
//! each rejection asserts a *named* error, not merely a failure.

mod support;

use std::collections::{BTreeSet, HashSet};

use latch_protocol::control::{self, ControlMessage};
use latch_protocol::frame::{self, FrameDecoder, FrameType};
use latch_protocol::{ProtocolError, MAX_FRAME_PAYLOAD};
use support::{Accept, CaseKind, Disposition, Layer, Reject};

/// Cases small enough to run the quadratic split-point assertions on.
const SPLIT_LIMIT: usize = 4096;

fn accepts() -> Vec<(String, String, Accept)> {
    support::load_cases()
        .into_iter()
        .filter_map(|c| match c.kind {
            CaseKind::Accept(a) => Some((c.name, c.description, a)),
            CaseKind::Reject(_) => None,
        })
        .collect()
}

fn rejects() -> Vec<(String, String, Reject)> {
    support::load_cases()
        .into_iter()
        .filter_map(|c| match c.kind {
            CaseKind::Reject(r) => Some((c.name, c.description, r)),
            CaseKind::Accept(_) => None,
        })
        .collect()
}

// --------------------------------------------------------------- happy paths

#[test]
fn recorded_bytes_decode_to_the_recorded_value() {
    for (name, why, case) in accepts() {
        let frame = frame::decode_exact(&case.encoded)
            .unwrap_or_else(|e| panic!("{name}: {why}\n  decode failed: {e}"));

        assert_eq!(
            frame.frame_type, case.frame_type,
            "{name}: wrong frame type"
        );
        assert_eq!(
            frame.payload,
            case.payload.as_slice(),
            "{name}: payload does not match ({} vs {} bytes)",
            frame.payload.len(),
            case.payload.len()
        );

        if let Some(expected) = &case.decoded {
            let got = control::decode(frame.payload)
                .unwrap_or_else(|e| panic!("{name}: {why}\n  control decode failed: {e}"));
            assert_eq!(&got, expected, "{name}: {why}");
        }
    }
}

#[test]
fn recorded_values_re_encode_to_the_recorded_bytes() {
    for (name, why, case) in accepts() {
        // Control payloads are re-encoded from the *message*, not from the
        // recorded payload bytes. Re-serializing bytes we just read would prove
        // nothing; this asserts field order and MessagePack representation.
        if let Some(message) = &case.decoded {
            let payload = control::encode(message);
            assert_eq!(
                hex(&payload),
                hex(&case.payload),
                "{name}: {why}\n  control payload differs from the fixture"
            );
        }

        let encoded = frame::encode(case.frame_type, &case.payload)
            .unwrap_or_else(|e| panic!("{name}: encode failed: {e}"));
        assert_eq!(
            encoded.len(),
            case.encoded.len(),
            "{name}: encoded length differs"
        );
        assert!(
            encoded == case.encoded,
            "{name}: {why}\n  re-encoded frame differs from the fixture"
        );

        assert_eq!(
            frame::encoded_len(case.payload.len()),
            case.encoded.len(),
            "{name}: encoded_len disagrees with the bytes it describes"
        );
    }
}

#[test]
fn encode_into_appends_and_leaves_earlier_bytes_alone() {
    // The hot path owns one buffer per attachment and appends every frame into
    // it. A codec that assumes an empty buffer works in tests and truncates
    // output in production.
    for (name, _why, case) in accepts() {
        if case.encoded.len() > SPLIT_LIMIT {
            continue;
        }
        let prefix = b"already queued".to_vec();
        let mut out = prefix.clone();
        frame::encode_into(case.frame_type, &case.payload, &mut out)
            .unwrap_or_else(|e| panic!("{name}: encode_into failed: {e}"));

        assert_eq!(
            &out[..prefix.len()],
            &prefix[..],
            "{name}: prefix disturbed"
        );
        assert_eq!(
            &out[prefix.len()..],
            case.encoded.as_slice(),
            "{name}: appended frame differs from the fixture"
        );
    }
}

#[test]
fn decode_frame_reports_exactly_what_it_consumed() {
    for (name, _why, case) in accepts() {
        let (frame, consumed) = frame::decode_frame(&case.encoded)
            .unwrap_or_else(|e| panic!("{name}: decode_frame failed: {e}"));
        assert_eq!(
            consumed,
            case.encoded.len(),
            "{name}: a caller that trusts this offset would desynchronize"
        );
        assert_eq!(
            frame.frame_type, case.frame_type,
            "{name}: wrong frame type"
        );
    }
}

#[test]
fn a_whole_buffer_streams_to_the_same_frame() {
    for (name, _why, case) in accepts() {
        let mut decoder = FrameDecoder::new();
        decoder.feed(&case.encoded);

        let frame = decoder
            .next_frame()
            .unwrap_or_else(|e| panic!("{name}: streaming decode failed: {e}"))
            .unwrap_or_else(|| panic!("{name}: a complete frame decoded as 'need more bytes'"));
        assert_eq!(
            frame.frame_type, case.frame_type,
            "{name}: wrong frame type"
        );
        assert_eq!(
            frame.payload.len(),
            case.payload.len(),
            "{name}: wrong length"
        );
        assert!(
            frame.payload == case.payload.as_slice(),
            "{name}: wrong payload"
        );

        assert!(
            matches!(decoder.next_frame(), Ok(None)),
            "{name}: the decoder invented a second frame"
        );
        assert_eq!(decoder.buffered_len(), 0, "{name}: bytes left over");
        assert!(
            !decoder.is_poisoned(),
            "{name}: a valid frame poisoned the decoder"
        );
        decoder
            .finish()
            .unwrap_or_else(|e| panic!("{name}: stream did not end on a frame boundary: {e}"));
    }
}

#[test]
fn a_read_boundary_anywhere_decodes_the_same() {
    // A socket read boundary falls where it falls: mid-header, mid-length,
    // mid-payload. Every split must produce the same frame, and no split may
    // produce a frame early.
    for (name, _why, case) in accepts() {
        if case.encoded.len() > SPLIT_LIMIT {
            continue;
        }
        for split in 0..=case.encoded.len() {
            let mut decoder = FrameDecoder::new();
            decoder.feed(&case.encoded[..split]);

            if split < case.encoded.len() {
                assert!(
                    matches!(decoder.next_frame(), Ok(None)),
                    "{name}: split at {split} produced a frame from a partial buffer"
                );
            }

            decoder.feed(&case.encoded[split..]);
            let frame = decoder
                .next_frame()
                .unwrap_or_else(|e| panic!("{name}: split at {split}: {e}"))
                .unwrap_or_else(|| panic!("{name}: split at {split}: no frame after all bytes"));
            assert_eq!(
                frame.frame_type, case.frame_type,
                "{name}: split at {split}"
            );
            assert!(
                frame.payload == case.payload.as_slice(),
                "{name}: split at {split}: wrong payload"
            );
        }
    }
}

#[test]
fn frames_concatenated_into_one_read_all_decode() {
    // The opposite boundary problem: several frames arriving in a single read.
    let cases = accepts();
    let small: Vec<&Accept> = cases
        .iter()
        .map(|(_, _, a)| a)
        .filter(|a| a.encoded.len() <= SPLIT_LIMIT)
        .collect();

    let mut stream = Vec::new();
    for case in &small {
        stream.extend_from_slice(&case.encoded);
    }

    let mut decoder = FrameDecoder::new();
    decoder.feed(&stream);
    for (i, case) in small.iter().enumerate() {
        let frame = decoder
            .next_frame()
            .unwrap_or_else(|e| panic!("frame {i} of a batched read: {e}"))
            .unwrap_or_else(|| panic!("frame {i} of a batched read went missing"));
        assert_eq!(frame.frame_type, case.frame_type, "frame {i}: wrong type");
        assert!(
            frame.payload == case.payload.as_slice(),
            "frame {i}: wrong payload"
        );
    }
    assert!(
        matches!(decoder.next_frame(), Ok(None)),
        "the decoder invented a frame after the batch"
    );
    decoder
        .finish()
        .expect("the batch ended on a frame boundary");
}

#[test]
fn session_update_dispositions_are_what_the_fixture_says() {
    // `session.update` is a merge, so absent and null are different
    // instructions. This asserts the decoded shape against the fixture's own
    // statement of intent, so neither can drift into agreeing with a bug.
    let mut seen = BTreeSet::new();
    for (name, why, case) in accepts() {
        let Some(merge) = case.merge else { continue };
        let Some(ControlMessage::SessionUpdate {
            state,
            attachments,
            title,
            ..
        }) = case.decoded.as_ref()
        else {
            panic!("{name}: a `merge` annotation belongs only on session.update");
        };

        check_disposition(&name, &why, "state", merge.state, state.is_some(), false);
        check_disposition(
            &name,
            &why,
            "attachments",
            merge.attachments,
            attachments.is_some(),
            false,
        );
        check_disposition(
            &name,
            &why,
            "title",
            merge.title,
            title.is_some(),
            matches!(title, Some(None)),
        );

        seen.insert(merge.title);
        seen.insert(merge.state);
    }

    for required in [Disposition::Absent, Disposition::Set, Disposition::Cleared] {
        assert!(
            seen.contains(&required),
            "no fixture exercises the `{required:?}` disposition, so nothing would \
             catch a codec that cannot express it"
        );
    }
}

fn check_disposition(
    name: &str,
    why: &str,
    field: &str,
    stated: Disposition,
    present: bool,
    cleared: bool,
) {
    match stated {
        Disposition::Absent => assert!(
            !present,
            "{name}: {why}\n  `{field}` is annotated absent but decoded as present; an \
             absent field must leave the client's value alone"
        ),
        Disposition::Set => assert!(
            present && !cleared,
            "{name}: {why}\n  `{field}` is annotated set but did not decode to a value"
        ),
        Disposition::Cleared => assert!(
            cleared,
            "{name}: {why}\n  `{field}` is annotated cleared but did not decode as an \
             explicit null; conflating it with absent is how a cleared title comes back"
        ),
    }
}

// ---------------------------------------------------------------- rejections

#[test]
fn rejections_produce_the_named_error() {
    for (name, why, case) in rejects() {
        match case.layer {
            Layer::Frame => {
                if case.error == "trailing_bytes" {
                    // The two entry points differ here on purpose: a stream
                    // decoder sees the next frame's bytes, while a decoder told
                    // "these bytes are one frame" must not silently absorb or
                    // discard the remainder.
                    let (_, consumed) = frame::decode_frame(&case.encoded).unwrap_or_else(|e| {
                        panic!("{name}: {why}\n  the leading frame must still decode: {e}")
                    });
                    assert!(
                        consumed < case.encoded.len(),
                        "{name}: decode_frame claimed bytes that are not part of the frame"
                    );
                }

                let err = frame::decode_exact(&case.encoded).err().unwrap_or_else(|| {
                    panic!("{name}: {why}\n  accepted bytes that must be refused")
                });
                assert_eq!(
                    err.code(),
                    case.error,
                    "{name}: {why}\n  wrong error: {err}"
                );
                assert_eq!(
                    ProtocolError::from(err).code(),
                    case.error,
                    "{name}: the unified error name must match the frame error name"
                );
            }
            Layer::Control => {
                let frame = frame::decode_exact(&case.encoded).unwrap_or_else(|e| {
                    panic!("{name}: {why}\n  the framing is well-formed and must decode: {e}")
                });
                assert_eq!(
                    frame.frame_type,
                    FrameType::Control,
                    "{name}: wrong frame type"
                );

                let err = control::decode(frame.payload).err().unwrap_or_else(|| {
                    panic!("{name}: {why}\n  accepted a control payload that must be refused")
                });
                assert_eq!(
                    err.code(),
                    case.error,
                    "{name}: {why}\n  wrong error: {err}"
                );
                assert!(
                    err.is_fatal(),
                    "{name}: every control rejection closes the connection"
                );
                assert_eq!(
                    ProtocolError::from(err).code(),
                    case.error,
                    "{name}: the unified error name must match the control error name"
                );
            }
        }
    }
}

#[test]
fn fatal_rejections_close_the_stream_rather_than_resynchronize() {
    for (name, why, case) in rejects() {
        if case.layer != Layer::Frame || case.error == "incomplete" {
            continue;
        }
        if case.error == "trailing_bytes" {
            continue; // in a stream those bytes are the next frame, not garbage
        }

        let mut decoder = FrameDecoder::new();
        decoder.feed(&case.encoded);

        let err = decoder
            .next_frame()
            .err()
            .unwrap_or_else(|| panic!("{name}: {why}\n  the stream decoder accepted these bytes"));
        assert_eq!(err.code(), case.error, "{name}: wrong error: {err}");
        assert!(
            err.is_fatal(),
            "{name}: this error must close the connection"
        );
        assert!(
            decoder.is_poisoned(),
            "{name}: the decoder must not be reusable"
        );

        // Feeding more bytes must not clear the poison. There is no offset to
        // resume from that is better than a guess.
        decoder.feed(&frame::encode(FrameType::TerminalOutput, b"ok").unwrap_or_default());
        let again = decoder
            .next_frame()
            .err()
            .unwrap_or_else(|| panic!("{name}: a poisoned decoder resynchronized"));
        assert_eq!(again.code(), case.error, "{name}: the poison error changed");
    }
}

#[test]
fn a_truncated_frame_is_dropped_rather_than_partially_accepted() {
    for (name, why, case) in rejects() {
        if case.error != "incomplete" {
            continue;
        }

        // Mid-stream, "incomplete" means only that the next read has not
        // happened yet — so it is not fatal there...
        let mut decoder = FrameDecoder::new();
        decoder.feed(&case.encoded);
        assert!(
            matches!(decoder.next_frame(), Ok(None)),
            "{name}: {why}\n  a partial frame must not decode"
        );
        assert!(
            !decoder.is_poisoned(),
            "{name}: waiting for bytes is not a fatal error"
        );

        // ...but at end of stream the peer truncated a frame, and the frame is
        // dropped rather than half-applied.
        let err = decoder
            .finish()
            .err()
            .unwrap_or_else(|| panic!("{name}: {why}\n  a truncated frame survived to EOF"));
        assert_eq!(err.code(), "incomplete", "{name}: wrong error: {err}");

        // The one-shot entry point has no "more is coming" case at all.
        let err = frame::decode_exact(&case.encoded)
            .err()
            .unwrap_or_else(|| panic!("{name}: decode_exact accepted a partial frame"));
        assert_eq!(err.code(), "incomplete", "{name}: wrong error: {err}");
        assert!(
            !err.is_fatal(),
            "{name}: incomplete is resolvable by reading more"
        );
    }
}

#[test]
fn an_oversized_length_is_refused_on_the_header_alone() {
    // The rejection must not depend on the payload arriving, because the point
    // of the limit is to avoid allocating on a peer's word about a length.
    for (name, why, case) in rejects() {
        if case.error != "oversized" {
            continue;
        }
        assert!(
            case.encoded.len() <= frame::HEADER_LEN,
            "{name}: this case should carry no payload, or it proves nothing"
        );

        let mut decoder = FrameDecoder::new();
        decoder.feed(&case.encoded);
        let err = decoder.next_frame().err().unwrap_or_else(|| {
            panic!("{name}: {why}\n  waited for a payload it must never accept")
        });
        assert_eq!(err.code(), "oversized", "{name}: wrong error: {err}");
    }
}

// --------------------------------------------------------- fixture coverage

/// Guards the fixture set itself. Every assertion above iterates whatever is on
/// disk, so a deleted or never-written case silently reduces the suite to
/// nothing rather than failing.
#[test]
fn the_fixture_set_covers_the_protocol() {
    let cases = support::load_cases();

    let mut types = HashSet::new();
    let mut discriminators = BTreeSet::new();
    let mut errors = BTreeSet::new();
    let mut has_empty_payload = false;
    let mut has_max_payload = false;
    let mut has_multibyte_display = false;

    for case in &cases {
        assert_eq!(
            case.protocol,
            latch_protocol::PROTOCOL_VERSION,
            "{}: fixture is written for another protocol version",
            case.name
        );
        assert!(
            !case.description.trim().is_empty(),
            "{}: a case with no description cannot be reviewed",
            case.name
        );

        match &case.kind {
            CaseKind::Accept(a) => {
                types.insert(a.frame_type);
                if let Some(m) = &a.decoded {
                    discriminators.insert(m.discriminator());
                }
                if a.payload.is_empty() {
                    has_empty_payload = true;
                }
                if a.payload.len() == MAX_FRAME_PAYLOAD {
                    has_max_payload = true;
                }
                if !a.payload.is_ascii() {
                    has_multibyte_display = true;
                }
            }
            CaseKind::Reject(r) => {
                errors.insert(r.error.clone());
            }
        }
    }

    for ty in [
        FrameType::TerminalOutput,
        FrameType::TerminalInput,
        FrameType::Control,
    ] {
        assert!(types.contains(&ty), "no fixture covers frame type {ty:?}");
    }

    for t in ControlMessage::DISCRIMINATORS {
        assert!(
            discriminators.contains(t),
            "no fixture covers control message `{t}`"
        );
    }
    assert_eq!(
        discriminators.len(),
        ControlMessage::DISCRIMINATORS.len(),
        "the control vocabulary is eight messages by design; the fixtures cover {discriminators:?}"
    );

    for code in [
        "unknown_type",
        "oversized",
        "incomplete",
        "trailing_bytes",
        "malformed_payload",
        "missing_discriminator",
        "unknown_message",
        "invalid_field",
        "unsupported_protocol",
    ] {
        assert!(
            errors.contains(code),
            "no fixture covers the `{code}` rejection"
        );
    }

    assert!(has_empty_payload, "no fixture covers a zero-length payload");
    assert!(
        has_max_payload,
        "no fixture sits at exactly MAX_FRAME_PAYLOAD, the largest frame that must be accepted"
    );
    assert!(
        has_multibyte_display,
        "no fixture carries multibyte UTF-8 in a display field"
    );
}

fn hex(bytes: &[u8]) -> String {
    // Assertions compare hex rather than byte vectors so a failure reads as a
    // diff a person can find the wrong byte in.
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
