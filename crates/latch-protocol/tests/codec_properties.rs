//! Properties the codec must hold for inputs nobody wrote down.
//!
//! The fixtures pin the bytes a reviewer thought of. These pin the ones nobody
//! did — which matters most for the decoder, because it is the only part of
//! Latch that reads bytes chosen by someone else. A worker that can be panicked
//! by a malformed frame is a worker that can be killed by any client, and that
//! takes the agent process down with it.

use latch_protocol::control::{
    self, AttachMode, Attachment, ClientInfo, ControlMessage, SessionInfo, SessionState, Size,
};
use latch_protocol::frame::{self, FrameDecoder, FrameType};
use latch_protocol::MAX_FRAME_PAYLOAD;
use proptest::prelude::*;

// ------------------------------------------------------------------ strategies

fn any_frame_type() -> impl Strategy<Value = FrameType> {
    prop_oneof![
        Just(FrameType::TerminalOutput),
        Just(FrameType::TerminalInput),
        Just(FrameType::Control),
    ]
}

/// Text with a real chance of multibyte characters, because display fields
/// carry names and titles that people actually type.
fn any_text() -> impl Strategy<Value = String> {
    prop_oneof![
        "[ -~]{0,24}",
        Just("日本語".to_string()),
        Just("café ☕".to_string()),
        Just("🙂🙂🙂".to_string()),
        Just(String::new()),
    ]
}

fn any_mode() -> impl Strategy<Value = AttachMode> {
    prop_oneof![Just(AttachMode::Watch), Just(AttachMode::Control)]
}

fn any_state() -> impl Strategy<Value = SessionState> {
    prop_oneof![
        Just(SessionState::Creating),
        Just(SessionState::Running),
        Just(SessionState::Stopping),
        Just(SessionState::Exited),
        Just(SessionState::Lost),
    ]
}

fn any_client() -> impl Strategy<Value = ClientInfo> {
    (any_text(), any_text()).prop_map(|(kind, name)| ClientInfo { kind, name })
}

fn any_size() -> impl Strategy<Value = Size> {
    (any::<u16>(), any::<u16>()).prop_map(|(cols, rows)| Size { cols, rows })
}

fn any_attachment() -> impl Strategy<Value = Attachment> {
    (any_text(), any_mode(), any_client()).prop_map(|(id, mode, client)| Attachment {
        id,
        mode,
        client,
    })
}

fn any_attachments() -> impl Strategy<Value = Vec<Attachment>> {
    prop::collection::vec(any_attachment(), 0..4)
}

fn any_session() -> impl Strategy<Value = SessionInfo> {
    (
        any_text(),
        any_text(),
        prop::option::of(any_text()),
        any_state(),
        any_size(),
    )
        .prop_map(|(id, name, title, state, size)| SessionInfo {
            id,
            name,
            title,
            state,
            size,
        })
}

/// Every message in the vocabulary, including the shapes that are easy to get
/// wrong: a null controller, an empty presence list, and each of the three
/// `session.update` dispositions.
fn any_control_message() -> impl Strategy<Value = ControlMessage> {
    prop_oneof![
        (any_mode(), any::<bool>(), any_client(), any_size()).prop_map(
            |(mode, steal, client, size)| ControlMessage::Attach {
                protocol: latch_protocol::PROTOCOL_VERSION,
                mode,
                steal,
                client,
                size,
            }
        ),
        (
            any_session(),
            prop::option::of(any_text()),
            any_attachments()
        )
            .prop_map(
                |(session, controller, attachments)| ControlMessage::Attached {
                    protocol: latch_protocol::PROTOCOL_VERSION,
                    session,
                    controller,
                    attachments,
                }
            ),
        (any::<u16>(), any::<u16>(), any::<Option<bool>>())
            .prop_map(|(cols, rows, pin)| ControlMessage::Resize { cols, rows, pin }),
        any::<bool>().prop_map(|steal| ControlMessage::ControlRequest { steal }),
        (prop::option::of(any_text()), prop::option::of(any_text())).prop_map(
            |(controller_id, controller_label)| ControlMessage::ControlState {
                controller_id,
                controller_label,
            }
        ),
        (
            prop::option::of(any_state()),
            prop::option::of(any_attachments()),
            prop::option::of(prop::option::of(any_text())),
            any::<Option<bool>>(),
        )
            .prop_map(
                |(state, attachments, title, force)| ControlMessage::SessionUpdate {
                    state,
                    attachments,
                    title,
                    force,
                }
            ),
        (
            prop::option::of(any::<i32>()),
            prop::option::of(any_text()),
            any_text()
        )
            .prop_map(|(code, signal, at)| ControlMessage::SessionExited {
                code,
                signal,
                at
            }),
        (any_text(), any_text())
            .prop_map(|(code, message)| ControlMessage::Error { code, message }),
    ]
}

// ------------------------------------------------------------------ properties

proptest! {
    /// Arbitrary bytes never panic the decoder.
    ///
    /// Every byte here arrives from a peer. An index out of bounds, a subtract
    /// with overflow, or a capacity overflow on an announced length is a remote
    /// kill of the worker — and the worker owns the agent's PTY, so it takes
    /// the session down with it.
    #[test]
    fn arbitrary_bytes_never_panic_the_one_shot_decoder(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let _ = frame::decode_frame(&bytes);
        let _ = frame::decode_exact(&bytes);
    }

    /// The same, through the streaming decoder, with the bytes arriving in
    /// chunks the way a socket delivers them.
    #[test]
    fn arbitrary_bytes_never_panic_the_streaming_decoder(
        bytes in prop::collection::vec(any::<u8>(), 0..2048),
        chunk in 1usize..64,
    ) {
        let mut decoder = FrameDecoder::new();
        for piece in bytes.chunks(chunk) {
            decoder.feed(piece);
            // Drain whatever completed. An error stops the drain but must not
            // stop the feeding: the point is that no later byte can panic it.
            while let Ok(Some(_)) = decoder.next_frame() {}
        }
        let _ = decoder.finish();
    }

    /// Bytes that begin with a valid header are the interesting shape: the
    /// decoder has agreed to read a length before it can know whether the rest
    /// is meaningful. Fully random bytes almost never reach that path.
    #[test]
    fn plausible_frames_never_panic_the_decoder(
        tag in prop_oneof![Just(0x01u8), Just(0x02), Just(0x10), any::<u8>()],
        len in prop_oneof![0u32..64, Just(u32::MAX), Just(MAX_FRAME_PAYLOAD as u32), Just(MAX_FRAME_PAYLOAD as u32 + 1)],
        body in prop::collection::vec(any::<u8>(), 0..96),
    ) {
        let mut bytes = vec![tag];
        bytes.extend_from_slice(&len.to_be_bytes());
        bytes.extend_from_slice(&body);

        let mut decoder = FrameDecoder::new();
        decoder.feed(&bytes);
        let _ = decoder.next_frame();
        let _ = decoder.finish();
        let _ = frame::decode_exact(&bytes);
    }

    /// An arbitrary control payload never panics the control decoder either.
    /// A `control` frame's payload is attacker-controlled MessagePack.
    #[test]
    fn arbitrary_control_payloads_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = control::decode(&bytes);
    }

    /// Any valid control message survives an encode/decode round trip.
    #[test]
    fn control_messages_round_trip(message in any_control_message()) {
        let payload = control::encode(&message);
        let decoded = control::decode(&payload)
            .map_err(|e| TestCaseError::fail(format!("{message:?} did not decode: {e}")))?;
        prop_assert_eq!(decoded, message);
    }

    /// And survives being wrapped in a frame, which is how it actually travels.
    #[test]
    fn framed_control_messages_round_trip(message in any_control_message()) {
        let payload = control::encode(&message);
        let encoded = frame::encode(FrameType::Control, &payload)
            .map_err(|e| TestCaseError::fail(format!("frame encode failed: {e}")))?;

        let frame = frame::decode_exact(&encoded)
            .map_err(|e| TestCaseError::fail(format!("frame decode failed: {e}")))?;
        prop_assert_eq!(frame.frame_type, FrameType::Control);

        let decoded = control::decode(frame.payload)
            .map_err(|e| TestCaseError::fail(format!("control decode failed: {e}")))?;
        prop_assert_eq!(decoded, message);
    }

    /// Encoding is deterministic. Re-encoding is compared byte for byte against
    /// recorded fixtures, so an encoder that is merely valid — one that picks a
    /// different-width integer on a whim — breaks the cross-language contract
    /// even though both encodings decode correctly.
    #[test]
    fn control_encoding_is_deterministic(message in any_control_message()) {
        prop_assert_eq!(control::encode(&message), control::encode(&message));
    }

    /// Raw payloads round trip unexamined, whatever the bytes are. The hot path
    /// must not care whether output is valid UTF-8: a read boundary splits
    /// multibyte characters, and plenty of children emit binary.
    #[test]
    fn raw_payloads_round_trip_untouched(
        frame_type in any_frame_type(),
        payload in prop::collection::vec(any::<u8>(), 0..4096),
    ) {
        let encoded = frame::encode(frame_type, &payload)
            .map_err(|e| TestCaseError::fail(format!("encode failed: {e}")))?;
        prop_assert_eq!(encoded.len(), frame::encoded_len(payload.len()));

        let frame = frame::decode_exact(&encoded)
            .map_err(|e| TestCaseError::fail(format!("decode failed: {e}")))?;
        prop_assert_eq!(frame.frame_type, frame_type);
        prop_assert_eq!(frame.payload, payload.as_slice());
    }

    /// A stream of frames decodes to the same frames however the reads are cut
    /// up. This is the property the streaming decoder exists for; a socket read
    /// boundary is not something the codec gets to choose.
    #[test]
    fn stream_framing_is_independent_of_read_boundaries(
        payloads in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..64), 0..8),
        chunk in 1usize..32,
    ) {
        let mut stream = Vec::new();
        for payload in &payloads {
            let encoded = frame::encode(FrameType::TerminalOutput, payload)
                .map_err(|e| TestCaseError::fail(format!("encode failed: {e}")))?;
            stream.extend_from_slice(&encoded);
        }

        let mut decoder = FrameDecoder::new();
        let mut got: Vec<Vec<u8>> = Vec::new();
        for piece in stream.chunks(chunk.max(1)) {
            decoder.feed(piece);
            while let Some(frame) = decoder
                .next_frame()
                .map_err(|e| TestCaseError::fail(format!("stream decode failed: {e}")))?
            {
                got.push(frame.payload.to_vec());
            }
        }
        decoder
            .finish()
            .map_err(|e| TestCaseError::fail(format!("stream did not end cleanly: {e}")))?;

        prop_assert_eq!(got, payloads);
    }
}
