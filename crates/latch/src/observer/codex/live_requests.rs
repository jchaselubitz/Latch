//! Request lifetimes are scoped to one app-server connection. The private
//! answer endpoint can only reach the current connection's exact request.
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

use anyhow::Result;
use serde_json::{json, Value};

use super::requests::Request;
use super::{answers, send, Relay, Socket};

const MAX_PENDING: usize = 12;
const MAX_PATCH_BYTES: usize = 24 * 1024;

struct Pending {
    request: Request,
    submitted: Option<(Option<String>, UnixStream)>,
}

pub(super) struct LiveRequests {
    nonce: String,
    next_request: u64,
    pending: HashMap<Value, Pending>,
    patches: HashMap<(String, String), Value>,
}

impl LiveRequests {
    pub fn new() -> Result<Self> {
        let mut nonce = [0; 16];
        File::open("/dev/urandom")?.read_exact(&mut nonce)?;
        Ok(Self {
            nonce: crate::hex::encode(&nonce),
            next_request: 0,
            pending: HashMap::new(),
            patches: HashMap::new(),
        })
    }

    pub fn open(&mut self, relay: &mut Relay, event: &Value, answerable: bool) -> Result<()> {
        if self.pending.len() >= MAX_PENDING || self.pending.contains_key(&event["id"]) {
            return Ok(());
        }
        let id = format!("codex:{}:{}", self.nonce, self.next_request);
        let Some(mut request) = Request::new(event, id) else {
            return Ok(());
        };
        if let Some(item) = &request.item {
            if let Some(changes) = self.patches.get(&(request.thread.clone(), item.clone())) {
                request.changes(changes);
            }
        }
        if !answerable && request.view.metadata.answerable == Some(true) {
            request.view.metadata.answerable = Some(false);
            request.view.metadata.reason = Some("Answer this request at the Mac.".into());
        }
        self.next_request += 1;
        project(relay, &request)?;
        self.pending.insert(
            request.rpc_id.clone(),
            Pending {
                request,
                submitted: None,
            },
        );
        Ok(())
    }

    pub fn activity(&mut self, relay: &mut Relay, event: &Value) -> Result<()> {
        let p = &event["params"];
        let Some(thread) = p["threadId"].as_str() else {
            return Ok(());
        };
        let (item, changes) = match event["method"].as_str() {
            Some("item/started") if p["item"]["type"] == "fileChange" => {
                (&p["item"]["id"], &p["item"]["changes"])
            }
            Some("item/fileChange/patchUpdated") => (&p["itemId"], &p["changes"]),
            Some("item/completed") if p["item"]["type"] == "fileChange" => {
                if let Some(item) = p["item"]["id"].as_str() {
                    self.patches.remove(&(thread.to_owned(), item.to_owned()));
                }
                return Ok(());
            }
            _ => return Ok(()),
        };
        let Some(item) = item.as_str() else {
            return Ok(());
        };
        if !changes.is_array() {
            return Ok(());
        }
        if self.patches.len() < MAX_PENDING && changes.to_string().len() <= MAX_PATCH_BYTES {
            self.patches
                .insert((thread.into(), item.into()), changes.clone());
        }
        for pending in self.pending.values_mut().filter(|pending| {
            pending.request.thread == thread
                && pending.request.item.as_deref() == Some(item)
                && pending.request.view.metadata.request_kind.as_deref() == Some("file_change")
        }) {
            pending.request.changes(changes);
            project(relay, &pending.request)?;
        }
        Ok(())
    }

    pub fn resolved(&mut self, relay: &mut Relay, data: &Value) -> Result<()> {
        let id = &data["requestId"];
        if self
            .pending
            .get(id)
            .is_none_or(|pending| data["threadId"] != pending.request.thread)
        {
            return Ok(());
        }
        let Some(pending) = self.pending.remove(id) else {
            return Ok(());
        };
        let (source, decision, stream) = match pending.submitted {
            Some((decision, stream)) => ("phone", decision, Some(stream)),
            None => ("terminal", None, None),
        };
        close(relay, &pending.request.view.id, source, decision)?;
        if let Some(mut stream) = stream {
            let _ = stream.write_all(b"Y");
        }
        Ok(())
    }

    pub fn disconnected(self, relay: &mut Relay) -> Result<()> {
        for pending in self.pending.into_values() {
            close(relay, &pending.request.view.id, "timeout", None)?;
        }
        Ok(())
    }

    pub fn accept(&mut self, listener: Option<&UnixListener>, codex: &mut Socket) -> Result<()> {
        let Some(listener) = listener else {
            return Ok(());
        };
        let Ok((mut stream, _)) = listener.accept() else {
            return Ok(());
        };
        let Ok(input) = answers::read_selection(&mut stream) else {
            return Ok(());
        };
        self.reply(input, stream, codex)
    }

    fn reply(&mut self, input: Value, mut stream: UnixStream, codex: &mut Socket) -> Result<()> {
        stream.set_write_timeout(Some(super::IO_TIMEOUT))?;
        let Some((pending, decision, result)) = self.pending.values_mut().find_map(|pending| {
            if pending.submitted.is_some() {
                return None;
            }
            let (decision, result) = pending.request.answer(&input)?;
            Some((pending, decision, result))
        }) else {
            let _ = stream.write_all(b"N");
            return Ok(());
        };
        send(codex, json!({"id":pending.request.rpc_id,"result":result}))?;
        pending.submitted = Some((decision, stream));
        Ok(())
    }
}

fn project(relay: &mut Relay, request: &Request) -> Result<()> {
    relay.append(json!({"method":"latch/codex/request/open","params":request.view}))
}

fn close(relay: &mut Relay, id: &str, source: &str, decision: Option<String>) -> Result<()> {
    relay.append(json!({"method":"latch/codex/request/closed","params":{"id":id,"source":source,"decision":decision}}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tungstenite::protocol::Role;

    use crate::conversation::{
        Connector, ConnectorMutation, ConversationItemKind, JsonlConnector, PollBudget,
        RequestStatus, ResolutionSource,
    };
    use crate::session::paths::{LatchHome, SessionId};

    fn fixture() -> Vec<Value> {
        include_str!("../../../../../fixtures/conversation/codex/app-server/requests.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn relay(dir: &std::path::Path) -> Relay {
        let journal_path = dir.join("journal.jsonl");
        Relay {
            home: LatchHome::new(dir),
            session: SessionId::parse("ses_fixture").unwrap(),
            binding: dir.join("binding.json"),
            journal: File::create(&journal_path).unwrap(),
            journal_path,
            root: Some("root".into()),
            children: HashMap::new(),
            child_offsets: HashMap::new(),
            rollout: None,
            rollout_offset: 0,
            rollout_prefix_done: true,
        }
    }

    fn answer(request: &Request, option: &str) -> Option<Value> {
        request
            .answer(&json!({"requestId":request.view.id, "optionId":option}))
            .map(|(_, reply)| reply)
    }

    #[test]
    fn fixture_requests_translate_to_phone_cards_and_exact_server_replies() {
        let cases = fixture();
        let expectations = [
            ("command", true),
            ("file_change", true),
            ("network", true),
            ("permissions", true),
            ("question", true),
            ("question", false),
            ("elicitation_form", true),
            ("elicitation_url", true),
            ("mcp_tool", true),
            ("browser", true),
            ("app_access", true),
            ("elicitation_form", false),
            ("mcp_tool", false),
            ("command", true),
            ("file_change", true),
        ];
        let dir = tempfile::tempdir().unwrap();
        let mut relay = relay(dir.path());
        for (index, (event, (kind, answerable))) in cases.iter().zip(expectations).enumerate() {
            let request = Request::new(event, format!("req-{index}")).unwrap();
            assert_eq!(request.view.metadata.request_kind.as_deref(), Some(kind));
            assert_eq!(request.view.metadata.answerable, Some(answerable));
            let mut requests = LiveRequests::new().unwrap();
            requests.open(&mut relay, event, true).unwrap();
            requests
                .resolved(
                    &mut relay,
                    &json!({"threadId":"root","requestId":event["id"]}),
                )
                .unwrap();
            match index {
                0 => {
                    assert!(answer(&request, "deny").is_none());
                    assert!(answer(&request, "allow_session").is_none());
                    assert_eq!(
                        answer(&request, "allow_always").unwrap()["decision"]
                            ["acceptWithExecpolicyAmendment"]["execpolicy_amendment"],
                        json!(["touch", "report.txt"])
                    );
                }
                1 => assert_eq!(request.view.metadata.details["grantRoot"], "/work"),
                2 => {
                    assert_eq!(request.view.metadata.details["host"], "example.test");
                    assert_eq!(
                        answer(&request, "deny_always").unwrap()["decision"]
                            ["applyNetworkPolicyAmendment"]["network_policy_amendment"]["action"],
                        "deny"
                    );
                }
                3 => {
                    assert_eq!(answer(&request, "allow_once").unwrap()["scope"], "turn");
                    assert_eq!(
                        answer(&request, "allow_session").unwrap()["permissions"],
                        event["params"]["permissions"]
                    );
                    assert_eq!(answer(&request, "deny").unwrap()["permissions"], json!({}));
                }
                4 => {
                    let input = json!({"requestId":request.view.id,"answers":{"Which language?":"English"}});
                    assert_eq!(
                        request.answer(&input).unwrap().1,
                        json!({"answers":{"language":{"answers":["English"]}}})
                    );
                }
                5 => assert!(request.view.questions.is_empty()),
                6 => {
                    assert!(request.answer(&json!({"requestId":request.view.id,"optionId":"submit_form","form":{"count":0}})).is_none());
                    assert_eq!(request.answer(&json!({"requestId":request.view.id,"optionId":"submit_form","form":{"count":2}})).unwrap().1, json!({"action":"accept","content":{"count":2}}));
                }
                7 => assert_eq!(answer(&request, "open_url").unwrap()["action"], "accept"),
                8 => assert_eq!(request.view.metadata.details["connectorName"], "Notes"),
                9 => assert_eq!(
                    request.view.metadata.details["origin"],
                    "https://example.test"
                ),
                10 => {
                    assert_eq!(request.view.metadata.details["appName"], "TextEdit");
                    assert!(answer(&request, "allow_once").is_none());
                    assert_eq!(
                        answer(&request, "allow_session").unwrap()["_meta"]["persist"],
                        "session"
                    );
                }
                13 => assert_eq!(
                    answer(&request, "allow_session").unwrap()["decision"],
                    "approved_for_session"
                ),
                14 => assert_eq!(
                    request.view.metadata.details["changes"][0]["diff"],
                    "-old\n+new"
                ),
                _ => {}
            }
        }
        let journal = std::fs::read_to_string(&relay.journal_path).unwrap();
        assert!(!journal.contains("fixture-challenge-private"));
        assert!(!journal.contains("Secret fixture prompt"));
        assert!(!journal.contains("do not project"));

        let mut connector = JsonlConnector::fixture("codex", relay.journal_path);
        let poll = connector
            .poll(PollBudget {
                max_records: 100,
                max_bytes: 256 * 1024,
                deadline: Duration::from_secs(1),
            })
            .unwrap();
        let mut pending = Vec::new();
        let mut closed = 0;
        for item in poll.mutations {
            let ConnectorMutation::Upsert(item) = item else {
                continue;
            };
            match item.kind {
                ConversationItemKind::Request {
                    status: RequestStatus::Pending,
                    metadata,
                    ..
                } => {
                    pending.push((metadata.request_kind.unwrap(), metadata.answerable.unwrap()));
                }
                ConversationItemKind::Request {
                    status: RequestStatus::Resolved,
                    metadata,
                    ..
                } => {
                    assert_eq!(metadata.resolved_by, Some(ResolutionSource::Terminal));
                    closed += 1;
                }
                _ => {}
            }
        }
        assert_eq!(
            pending,
            expectations.map(|(kind, answerable)| (kind.to_owned(), answerable))
        );
        assert_eq!(closed, expectations.len());
    }

    #[test]
    fn stale_answer_cannot_reach_a_new_callback_and_the_phone_waits_for_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let mut relay = relay(dir.path());
        let mut requests = LiveRequests::new().unwrap();
        let mut first = fixture().remove(0);
        requests.open(&mut relay, &first, true).unwrap();
        let old = requests.pending[&json!(1)].request.view.id.clone();
        requests
            .resolved(&mut relay, &json!({"threadId":"root","requestId":1}))
            .unwrap();

        // Simulate a different request even if the app-server reused an RPC id.
        first["params"]["approvalId"] = json!("another-callback");
        requests.open(&mut relay, &first, true).unwrap();
        let next = requests.pending[&json!(1)].request.view.id.clone();

        let (client, server) = UnixStream::pair().unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut socket = Socket::from_raw_socket(client, Role::Client, None);
        let mut server = Socket::from_raw_socket(server, Role::Server, None);
        let (tx, mut rx) = UnixStream::pair().unwrap();
        requests
            .reply(
                json!({"requestId":old,"optionId":"allow_once"}),
                tx,
                &mut socket,
            )
            .unwrap();
        let mut signal = [0u8; 1];
        rx.read_exact(&mut signal).unwrap();
        assert_eq!(signal, [b'N']);

        let (tx, mut rx) = UnixStream::pair().unwrap();
        requests
            .reply(
                json!({"requestId":next,"optionId":"allow_once"}),
                tx,
                &mut socket,
            )
            .unwrap();
        let reply: Value = serde_json::from_str(server.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(reply, json!({"id":1,"result":{"decision":"accept"}}));
        requests
            .resolved(
                &mut relay,
                &json!({"threadId":"another-thread","requestId":1}),
            )
            .unwrap();
        assert!(requests.pending.contains_key(&json!(1)));
        requests
            .resolved(&mut relay, &json!({"threadId":"root","requestId":1}))
            .unwrap();
        rx.read_exact(&mut signal).unwrap();
        assert_eq!(signal, [b'Y']);

        requests.open(&mut relay, &first, true).unwrap();
        requests.disconnected(&mut relay).unwrap();
        let journal = std::fs::read_to_string(&relay.journal_path).unwrap();
        let sources: Vec<_> = journal
            .lines()
            .filter_map(|line| {
                let event: Value = serde_json::from_str(line).unwrap();
                event["params"]["source"].as_str().map(str::to_owned)
            })
            .collect();
        assert_eq!(sources, ["terminal", "phone", "timeout"]);
    }
}
