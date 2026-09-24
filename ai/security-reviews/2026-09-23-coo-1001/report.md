# Security Review: Latch

## Scope

The scan was configured for the include paths and exclusions listed below.

- Scan mode: repository
- Target kind: git_worktree
- Target ID: latch-local-repository
- Revision: 4b7e46354ef0a7097f0f64e3a9ebd808718c7576
- Snapshot digest: codex-security-snapshot/v1:sha256:a06cdf6d81c1bcc96335ab7ef62ec03cccf03bd2062aab3e9b851cd058c84212
- Inventory strategy: repository
- Included paths: .
- Excluded paths: none
- Runtime or test status: No application code, exploit, or production test executed.

Limitations and exclusions:
- Excluded production deployment and external services: Offline source review only; no network probes, production configuration or secret values accessed.
- Excluded third-party implementation beyond named parser/error paths: No comprehensive dependency vulnerability or native extractor audit.

### Scan Summary

| Field | Value |
| --- | --- |
| Scan outcome | completed |
| Reportable findings | 4 |
| Severity mix | high: 2, low: 2 |
| Confidence mix | high: 4 |
| Coverage | partial |
| Validation mode | Offline static source validation with independent review. |

Canonical artifacts: `scan-manifest.json`, `findings.json`, and `coverage.json`. The four findings below are the first scan. Transport and native-client findings from `coo:1001.z2qn` are in `transport-native-follow-up.md`, and `coverage.json` includes the files that follow-up fully read.

## Threat Model

Latch provides persistent user-owned terminal sessions through a Rust CLI and per-session latchd daemon, macOS Desktop, iOS Mobile, HTTP/WebSocket clients, and paired Remote Link access. Local clients use private Unix sockets. The gateway offers discovery, creation, directory browsing, preview, stop, terminal and conversation routes (crates/latch/src/cli/serve/routes.rs:77-142). Paired endpoints authenticate keys with Noise; the host authority proxy injects the local gateway credential and grants. The TypeScript control plane manages identity/admission, and the independently deployed relay carries encrypted records (crates/latch-remote/src/link.rs:262-275; crates/latch-transport/src/link.rs:752-784; crates/latch/src/cli/remote_access.rs:645-688; services/control-plane/src/api.ts:165-183; services/relay/src/server.ts:141-165).

### Assets

- Hosted processes, terminal input/output, filesystem access and agent conversations under the owner OS identity; Observe, Interact and Control are distinct grants (crates/latch/src/cli/serve/routes.rs:14-42).
- Local state: LATCH_HOME or $HOME/.latch, sessions/\<session-id\>, serve.token and remote-access state; owner CLI/daemon and same-UID processes are recipients (crates/latch/src/session/paths.rs:112-156).
- Remote identity/grants: remote-access/identity.json, devices.json, settings.json and audit.jsonl. macOS private keys use Keychain service co.cooperativ.latch.remote-access and device-id account; non-macOS/test keys use remote-access/identity.key with private file storage (crates/latch/src/cli/remote_access.rs:137-159,893-921,955-964,981-1001,1062-1084).
- Supervised gateway bearer: \<home\>/remote-access/runtime/remote-link-gateway.token; readiness: remote-link-gateway-ready.json. The local helper launches latch serve on 127.0.0.1:0 with explicit token/ready paths and parent watchdog. A private owner lock and local-only address validation protect this resource (crates/latch/src/cli/remote_access.rs:413-433,491-507,538-593).
- Control-plane account/device digests, pairings, admissions, leases and revocations in DATABASE_URL PostgreSQL; operator secret, admission key, relay service/invalidation credentials and optional APNs signing material are environment references, not inspected values (services/control-plane/src/config.ts:116-173; main.ts:25-44; api.ts:92-124,155-183).
- Executable integrity and release signing authority. CLI updater checks checksum, payload manifest and matching signing team when the current installation has a recognized team; initial installer lacks publisher pinning (crates/latch/src/cli/update/mod.rs:368-390,568-615; scripts/install-cli.sh:25-41).

### Trust Boundaries

- Other local UID -\> daemon: socket directory LATCHD_SOCKET_DIR else /tmp/latchd-\<uid\>, with \<8-hex-low32-FNV-home\>-\<session-id\>.sock. Owner/private modes and O_NOFOLLOW directory inspection apply; daemon and client require same-user peer credentials. Same-UID hosted processes are not isolated tenants (crates/latchd/src/paths.rs:112-188; daemon.rs:440-478; client.rs:27-31; peer.rs:21-95).
- HTTP/WebSocket client -\> gateway: --token-file else \<home\>/serve.token; default bind 127.0.0.1:4610. A 32-byte random token is stored privately and loaded each request. Origin and token checks precede route authorization. Loopback without device headers receives Control; non-loopback application calls are rejected even with a bearer. OPTIONS is unauthenticated (crates/latch/src/main.rs:273-282,719-738; cli/serve/auth.rs:13-43; cli/serve/http.rs:255-335).
- Paired controller -> host: Noise XX pins the expected static peer and purpose in the handshake. Local authorization is keyed by peer public key and grant revision. Enrollment matches proposal identity/key/permission and waits for Desktop approval before committing local authority (crates/latch-transport/src/link.rs:752-784; crates/latch-remote/src/link.rs:262-270,637-674). The transport/FFI follow-up confirmed those pins and recorded remaining buffer, LAN, and approval-prompt issues in `transport-native-follow-up.md`.
- Authenticated stream -\> loopback bearer authority: proxy rejects forbidden headers and transfer encoding, checks route grant and initial pipelining, injects bearer/grant/device and periodically checks revocation/revision. The reported parsing discrepancy breaks this boundary for conversation grants and identity. Conversation GET itself requires Observe; Hub mutation checks use the subscriber grant (crates/latch/src/cli/remote_access.rs:645-831; cli/serve/routes.rs:137-142; conversation/hub.rs:739-769).
- Internet client/device -\> control plane: domain-separated account/device credential digests, role/account pairing guards and admission rate limits; operator and relay redemption use separate secrets. Signed admissions bind issuer, audience, kid, room, role, purpose, generation, ticket, validity window and limits (services/control-plane/src/api.ts:75-183). Provisional enrollment credentials do not resolve as regular devices before host completion.
- Internet endpoint -\> relay: signed admission plus private control-plane redemption; role generation, lease deadlines, record/buffer/bandwidth bounds; invalidation uses its own secret. Relay TLS uses certificate/key paths or deployment termination. Missing error containment and pending-slot reservations are reported (services/relay/src/server.ts:9-14,92-114,141-208).
- Relay -\> redemption service: CONTROL_PLANE_URL with trailing slash removed, then /private/v1/relay/redemptions; RELAY_SERVICE_TOKEN is sent as bearer. Endpoint transport security depends on operator URL; no production URL was inspected (services/relay/src/main.ts:9-14,35-42).
- Agent/source data -\> observer and Hub: source bindings/raw hooks are stored beside sessions; launched Claude receives a generated private plugin. Such data is not inherently trusted, but same-UID agent execution already possesses user filesystem authority (crates/latch/src/observer.rs:41-60,112-134,191-193,213-256; session/paths.rs:235-245).
- Release infrastructure -\> installed execution: downloaded binaries must be authenticated before execution. Tag-triggered CI signs/notarizes using configured credentials; initial install signature checks do not pin the publisher (scripts/install-cli.sh:25-41; .github/workflows/release-cli.yml:28-69).

### Attacker Capabilities

- Unauthenticated network clients can send malformed traffic to externally deployed control-plane/relay listeners; actual edge exposure remains deployment-dependent.
- Paired Observe/Interact controllers possess their own key and admissions but should not gain stronger grants or another device/account identity.
- Relay infrastructure/traffic attackers may disrupt ciphertext delivery but are not assumed to possess pinned endpoint keys or the enrollment secret.
- Other Unix users may attempt socket or temporary-directory access but lack owner UID/private state. Same-UID workloads already share ambient OS authority.
- Terminal output, agent transcripts and source bindings can contain attacker-influenced content.
- A release-asset attacker is not automatically a holder of the Developer ID signing key; initial-install and signed-update guarantees differ.

### Security Objectives

- Authenticate local daemon peers as the owning UID and prevent foreign-user daemon impersonation.
- Preserve endpoint confidentiality and exact key binding independently of relay admission.
- Enforce Observe/Interact/Control and device identity consistently across stream parsing, gateway upgrade and later conversation actions; invalidate revoked grants.
- Keep host gateway bearer and private keys outside cloud application payloads.
- Prevent cross-account pairing/admission and unauthorized operator/relay administration.
- Contain malformed-client errors and enforce capacity budgets across concurrent requests.
- Authenticate executable publisher before execution and treat terminal/transcript data as lower-trust input.

### Assumptions

- User requested repository-wide review and a findings report; no supplied threat model or knowledge base. Root and crates/services/apps SECURITY.md resolution returned no policy.
- Production exposure, concrete database URL, secret values, TLS termination, proxy trust and signing-infrastructure permissions were not inspected.
- PostgreSQL configuration flows process.env -\> loadConfig -\> PostgresStore -\> pg Pool. Pool defaults to 10; migrations default enabled. TLS is disabled if a regex over the full DATABASE_URL matches localhost, 127.0.0.1 or .railway.internal; otherwise rejectUnauthorized defaults false. Actual transport risk depends on deployment (services/control-plane/src/config.ts:144-147; main.ts:25-35; store/postgres.ts:140-147).
- Non-loopback --allow-remote comments describe bearer access, but middleware currently denies non-loopback application requests; this is a documentation/implementation discrepancy, not an access bypass (crates/latch/src/cli/serve/mod.rs:7-9,101-115; auth.rs:92-95; http.rs:275-299).
- Local process plane is POSIX; peer checks support macOS/BSD/Linux and fail closed elsewhere (crates/latchd/src/peer.rs:14-95).
- Unsigned/ad-hoc/development updater installations skip matching-team verification (crates/latch/src/cli/update/mod.rs:568-615).
- No dynamic exploitation or dependency vulnerability database checks were performed. Native archive extraction and exact deployment transport remain open questions.
- The first pass fully audited 60 files. Follow-up `coo:1001.z2qn` fully read 15 more transport, FFI, and native-client files (75 total in `coverage.json`). Conversation rendering, generated bindings, tests, and other deferred paths remain unaudited. Token usage measurement unavailable.

## Findings

| Finding | Severity | Confidence | Detailed write-up |
| --- | --- | --- | --- |
| [Read-only paired devices can gain conversation write permissions](#finding-1) | high | high | inline below |
| [An admitted client can crash the shared relay](#finding-2) | high | high | inline below |
| [Initial installer does not verify the release publisher before executing binaries](#finding-3) | low | high | inline below |
| [Concurrent upgrades bypass relay connection budgets](#finding-4) | low | high | inline below |

### Confidence Scale

| Label | Meaning |
| --- | --- |
| high | Direct evidence supports the finding with no material unresolved blocker. |
| medium | Evidence supports a plausible issue, but material runtime or reachability proof remains. |
| low | Evidence is incomplete and the item is retained only for explicit follow-up. |

<a id="finding-1"></a>

### [1] Read-only paired devices can gain conversation write permissions

| Field | Value |
| --- | --- |
| Severity | high |
| Confidence | high |
| Confidence rationale | Independently reviewed source dataflow and effective controls; no runtime reproduction. |
| Category | authorization |
| CWE | CWE-436 |
| Affected lines | crates/latch/src/cli/remote_access.rs:760-802, crates/latch/src/cli/remote_access.rs:809-831, crates/latch/src/cli/serve/http.rs:278-290, crates/latch/src/cli/serve/conversation.rs:125-132, crates/latch/src/conversation/hub.rs:739-769, crates/latch/src/conversation/connectors/jsonl.rs:1526-1533 |

#### Summary

A paired Observe device can exploit inconsistent HTTP header parsing to supply a stronger conversation grant. The Conversation Hub then permits Interact actions, including sending messages and resolving available requests.

#### Root Cause

authorize_and_inject splits on CRLF but preserves bare LF inside values. Locked httparse 1.10.1 accepts bare LF as a header delimiter (src/lib.rs:1204-1210); hyper 1.11.0 appends parsed headers in wire order (src/proto/h1/role.rs:340); http 1.5.0 HeaderMap::get returns the first value (src/header/map.rs:786-790,814-821). Thus a hidden grant or identity header precedes the proxy-appended value. Conversation GET requires Observe, so the real route grant check passes. The gateway stores the stronger first grant and the Hub uses it for Interact actions.

**Proxy checks CRLF-delimited header names** — `crates/latch/src/cli/remote_access.rs:760-802`

The controller supplies request bytes. Bare LF within a header value is not recognized as a second header by this check.

```
fn authorize_and_inject(
    request: Vec<u8>,
    permission: DevicePermission,
    device_id: &str,
    token: &str,
) -> anyhow::Result<(Vec<u8>, DevicePermission)> {
    let end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("missing HTTP headers"))?;
    let headers = std::str::from_utf8(&request[..end]).context("request headers are not UTF-8")?;
    let mut lines = headers.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow!("missing HTTP request line"))?;
    let mut words = request_line.split_whitespace();
    let method = words.next().ok_or_else(|| anyhow!("missing HTTP method"))?;
    let target = words.next().ok_or_else(|| anyhow!("missing HTTP target"))?;
    let version = words
        .next()
        .ok_or_else(|| anyhow!("missing HTTP version"))?;
    if words.next().is_some()
        || version != "HTTP/1.1"
        || !target.starts_with("/v2/")
        || target.contains("..")
        || target.to_ascii_lowercase().contains("%2e")
    {
        bail!("request target is not permitted");
    }
    let mut websocket_upgrade = false;
    for line in lines {
        if line.starts_with(' ') || line.starts_with('\t') || !line.contains(':') {
            bail!("malformed HTTP header");
        }
        let (name, value) = line.split_once(':').expect("header delimiter checked");
        if name.eq_ignore_ascii_case("authorization")
            || name.eq_ignore_ascii_case("proxy-authorization")
            || name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case(DEVICE_GRANT_HEADER)
            || name.eq_ignore_ascii_case(DEVICE_ID_HEADER)
        {
            bail!("remote request contains a forbidden HTTP header");
        }
```

**Original bytes precede trusted headers** — `crates/latch/src/cli/remote_access.rs:809-831`

The proxy checks the real device grant for the route, then copies original header bytes before its own grant and identity headers.

```
    }
    let (_, required) = route_for(method, target)
        .ok_or_else(|| anyhow!("HTTP operation is not permitted through Remote Link"))?;
    if !permission.permits(required) {
        bail!("device permission does not allow this operation");
    }
    let mut injected = Vec::with_capacity(request.len() + token.len() + 64);
    injected.extend_from_slice(&request[..end]);
    injected.extend_from_slice(b"\r\nAuthorization: Bearer ");
    injected.extend_from_slice(token.as_bytes());
    injected.extend_from_slice(b"\r\n");
    injected.extend_from_slice(DEVICE_GRANT_HEADER.as_bytes());
    injected.extend_from_slice(b": ");
    injected.extend_from_slice(permission.as_header_value().as_bytes());
    injected.extend_from_slice(b"\r\n");
    injected.extend_from_slice(DEVICE_ID_HEADER.as_bytes());
    injected.extend_from_slice(b": ");
    injected.extend_from_slice(device_id.as_bytes());
    if !websocket_upgrade {
        injected.extend_from_slice(b"\r\nConnection: close");
    }
    injected.extend_from_slice(&request[end..]);
    Ok((injected, required))
```

**Gateway takes the first grant header** — `crates/latch/src/cli/serve/http.rs:278-290`

The loopback request is trusted and HeaderMap::get takes the first value, which can be attacker-supplied after downstream parsing.

```
        .map(|ConnectInfo(address)| address.ip().is_loopback())
        .unwrap_or(state.bind_is_loopback);
    let grant =
        match request.headers().get(DEVICE_GRANT_HEADER) {
            Some(value) if peer_is_loopback => value
                .to_str()
                .ok()
                .and_then(Grant::from_header_value)
                .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "invalid device grant"))?,
            Some(_) => {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "device grant header is trusted only from the loopback proxy",
```

**Conversation subscription carries the chosen grant** — `crates/latch/src/cli/serve/conversation.rs:125-132`

The grant selected at upgrade becomes the subscriber grant used for later actions.

```
        return;
    }
    let Some((subscriber, outcome)) =
        hub.subscribe_device(&id, grant, device.clone(), query.position())
    else {
        let _ = send(
            &mut socket,
            ConversationServerMessage::Error {
```

**Hub trusts the stored subscriber grant** — `crates/latch/src/conversation/hub.rs:739-769`

Action authorization checks the spoofed subscriber grant, not the underlying device store.

```
        let subscriber_record = actor
            .subscribers
            .get(&subscriber)
            .ok_or_else(|| anyhow::anyhow!("unknown subscriber"))?;
        let grant = subscriber_record.grant;
        let device = subscriber_record.device.clone();
        let payload_digest = Some(action_digest(action));
        if let Some(old) = actor.operations.iter().find(|r| r.id == operation_id) {
            if old.device != device || old.payload_digest != payload_digest {
                return Ok(OperationOutcome::Refused {
                    reason: "operation id conflict: another device or payload already used this id"
                        .into(),
                });
            }
            return Ok(match &old.outcome {
                OperationOutcome::Started => OperationOutcome::Ambiguous,
                v => v.clone(),
            });
        }
        let descriptor = actor
            .actions
            .iter()
            .find(|d| d.id == action.id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown action"))?;
        // Authorization is decided before availability so an observe-only
        // device is refused for the same reason whatever the connector's state.
        if !grant.permits(descriptor.required_grant) {
            return Ok(OperationOutcome::Refused {
                reason: "device grant does not permit this action".into(),
            });
```

**Allowed action writes into the hosted composer** — `crates/latch/src/conversation/connectors/jsonl.rs:1526-1533`

When live composer checks pass, the action submits text to the hosted process.

```
        let screen = self.current_screen(remaining()?)?;
        if action.id == ACTION_SEND_MESSAGE {
            if !screen.lines().any(|line| is_empty_composer(self.id, line)) {
                return Ok(ApplyResult::Refused {
                    reason: format!("the {} composer is no longer empty", self.id),
                });
            }
            self.control()?.submit(text, remaining()?)?;
```

#### Validation

authorize_and_inject splits on CRLF but preserves bare LF inside values. Locked httparse 1.10.1 accepts bare LF as a header delimiter (src/lib.rs:1204-1210); hyper 1.11.0 appends parsed headers in wire order (src/proto/h1/role.rs:340); http 1.5.0 HeaderMap::get returns the first value (src/header/map.rs:786-790,814-821). Thus a hidden grant or identity header precedes the proxy-appended value. Conversation GET requires Observe, so the real route grant check passes. The gateway stores the stronger first grant and the Hub uses it for Interact actions.

Validation method: offline static source trace

**Proxy checks CRLF-delimited header names** — `crates/latch/src/cli/remote_access.rs:760-802`

The controller supplies request bytes. Bare LF within a header value is not recognized as a second header by this check.

```
fn authorize_and_inject(
    request: Vec<u8>,
    permission: DevicePermission,
    device_id: &str,
    token: &str,
) -> anyhow::Result<(Vec<u8>, DevicePermission)> {
    let end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("missing HTTP headers"))?;
    let headers = std::str::from_utf8(&request[..end]).context("request headers are not UTF-8")?;
    let mut lines = headers.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow!("missing HTTP request line"))?;
    let mut words = request_line.split_whitespace();
    let method = words.next().ok_or_else(|| anyhow!("missing HTTP method"))?;
    let target = words.next().ok_or_else(|| anyhow!("missing HTTP target"))?;
    let version = words
        .next()
        .ok_or_else(|| anyhow!("missing HTTP version"))?;
    if words.next().is_some()
        || version != "HTTP/1.1"
        || !target.starts_with("/v2/")
        || target.contains("..")
        || target.to_ascii_lowercase().contains("%2e")
    {
        bail!("request target is not permitted");
    }
    let mut websocket_upgrade = false;
    for line in lines {
        if line.starts_with(' ') || line.starts_with('\t') || !line.contains(':') {
            bail!("malformed HTTP header");
        }
        let (name, value) = line.split_once(':').expect("header delimiter checked");
        if name.eq_ignore_ascii_case("authorization")
            || name.eq_ignore_ascii_case("proxy-authorization")
            || name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case(DEVICE_GRANT_HEADER)
            || name.eq_ignore_ascii_case(DEVICE_ID_HEADER)
        {
            bail!("remote request contains a forbidden HTTP header");
        }
```

**Original bytes precede trusted headers** — `crates/latch/src/cli/remote_access.rs:809-831`

The proxy checks the real device grant for the route, then copies original header bytes before its own grant and identity headers.

```
    }
    let (_, required) = route_for(method, target)
        .ok_or_else(|| anyhow!("HTTP operation is not permitted through Remote Link"))?;
    if !permission.permits(required) {
        bail!("device permission does not allow this operation");
    }
    let mut injected = Vec::with_capacity(request.len() + token.len() + 64);
    injected.extend_from_slice(&request[..end]);
    injected.extend_from_slice(b"\r\nAuthorization: Bearer ");
    injected.extend_from_slice(token.as_bytes());
    injected.extend_from_slice(b"\r\n");
    injected.extend_from_slice(DEVICE_GRANT_HEADER.as_bytes());
    injected.extend_from_slice(b": ");
    injected.extend_from_slice(permission.as_header_value().as_bytes());
    injected.extend_from_slice(b"\r\n");
    injected.extend_from_slice(DEVICE_ID_HEADER.as_bytes());
    injected.extend_from_slice(b": ");
    injected.extend_from_slice(device_id.as_bytes());
    if !websocket_upgrade {
        injected.extend_from_slice(b"\r\nConnection: close");
    }
    injected.extend_from_slice(&request[end..]);
    Ok((injected, required))
```

**Gateway takes the first grant header** — `crates/latch/src/cli/serve/http.rs:278-290`

The loopback request is trusted and HeaderMap::get takes the first value, which can be attacker-supplied after downstream parsing.

```
        .map(|ConnectInfo(address)| address.ip().is_loopback())
        .unwrap_or(state.bind_is_loopback);
    let grant =
        match request.headers().get(DEVICE_GRANT_HEADER) {
            Some(value) if peer_is_loopback => value
                .to_str()
                .ok()
                .and_then(Grant::from_header_value)
                .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "invalid device grant"))?,
            Some(_) => {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "device grant header is trusted only from the loopback proxy",
```

**Conversation subscription carries the chosen grant** — `crates/latch/src/cli/serve/conversation.rs:125-132`

The grant selected at upgrade becomes the subscriber grant used for later actions.

```
        return;
    }
    let Some((subscriber, outcome)) =
        hub.subscribe_device(&id, grant, device.clone(), query.position())
    else {
        let _ = send(
            &mut socket,
            ConversationServerMessage::Error {
```

**Hub trusts the stored subscriber grant** — `crates/latch/src/conversation/hub.rs:739-769`

Action authorization checks the spoofed subscriber grant, not the underlying device store.

```
        let subscriber_record = actor
            .subscribers
            .get(&subscriber)
            .ok_or_else(|| anyhow::anyhow!("unknown subscriber"))?;
        let grant = subscriber_record.grant;
        let device = subscriber_record.device.clone();
        let payload_digest = Some(action_digest(action));
        if let Some(old) = actor.operations.iter().find(|r| r.id == operation_id) {
            if old.device != device || old.payload_digest != payload_digest {
                return Ok(OperationOutcome::Refused {
                    reason: "operation id conflict: another device or payload already used this id"
                        .into(),
                });
            }
            return Ok(match &old.outcome {
                OperationOutcome::Started => OperationOutcome::Ambiguous,
                v => v.clone(),
            });
        }
        let descriptor = actor
            .actions
            .iter()
            .find(|d| d.id == action.id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown action"))?;
        // Authorization is decided before availability so an observe-only
        // device is refused for the same reason whatever the connector's state.
        if !grant.permits(descriptor.required_grant) {
            return Ok(OperationOutcome::Refused {
                reason: "device grant does not permit this action".into(),
            });
```

**Allowed action writes into the hosted composer** — `crates/latch/src/conversation/connectors/jsonl.rs:1526-1533`

When live composer checks pass, the action submits text to the hosted process.

```
        let screen = self.current_screen(remaining()?)?;
        if action.id == ACTION_SEND_MESSAGE {
            if !screen.lines().any(|line| is_empty_composer(self.id, line)) {
                return Ok(ApplyResult::Refused {
                    reason: format!("the {} composer is no longer empty", self.id),
                });
            }
            self.control()?.submit(text, remaining()?)?;
```

Counterevidence and remaining uncertainty:
- Not unauthenticated access. The endpoint Noise handshake and paired key are still required.
- Direct terminal, session-create, and stop routes independently require Control at the outer proxy; this finding does not prove access to them.
- Operation epochs and live connector availability remain enforced. Periodic proxy checks compare the real grant to the Observe route requirement, so they do not repair the spoofed subscriber grant.
- The same parsing discrepancy permits a forged device identity; receipt impersonation additionally requires relevant target identifiers.

Limitations:
- No application code or exploit was executed; production exposure was not tested.

#### Dataflow

authorize_and_inject splits on CRLF but preserves bare LF inside values. Locked httparse 1.10.1 accepts bare LF as a header delimiter (src/lib.rs:1204-1210); hyper 1.11.0 appends parsed headers in wire order (src/proto/h1/role.rs:340); http 1.5.0 HeaderMap::get returns the first value (src/header/map.rs:786-790,814-821). Thus a hidden grant or identity header precedes the proxy-appended value. Conversation GET requires Observe, so the real route grant check passes. The gateway stores the stronger first grant and the Hub uses it for Interact actions.

- **Source:** Remote Link conversation WebSocket upgrade

- **Sink:** Unauthorized conversation messages and resolution of available agent requests

- **Outcome:** Unauthorized conversation messages and resolution of available agent requests

**Proxy checks CRLF-delimited header names** — `crates/latch/src/cli/remote_access.rs:760-802`

The controller supplies request bytes. Bare LF within a header value is not recognized as a second header by this check.

```
fn authorize_and_inject(
    request: Vec<u8>,
    permission: DevicePermission,
    device_id: &str,
    token: &str,
) -> anyhow::Result<(Vec<u8>, DevicePermission)> {
    let end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| anyhow!("missing HTTP headers"))?;
    let headers = std::str::from_utf8(&request[..end]).context("request headers are not UTF-8")?;
    let mut lines = headers.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow!("missing HTTP request line"))?;
    let mut words = request_line.split_whitespace();
    let method = words.next().ok_or_else(|| anyhow!("missing HTTP method"))?;
    let target = words.next().ok_or_else(|| anyhow!("missing HTTP target"))?;
    let version = words
        .next()
        .ok_or_else(|| anyhow!("missing HTTP version"))?;
    if words.next().is_some()
        || version != "HTTP/1.1"
        || !target.starts_with("/v2/")
        || target.contains("..")
        || target.to_ascii_lowercase().contains("%2e")
    {
        bail!("request target is not permitted");
    }
    let mut websocket_upgrade = false;
    for line in lines {
        if line.starts_with(' ') || line.starts_with('\t') || !line.contains(':') {
            bail!("malformed HTTP header");
        }
        let (name, value) = line.split_once(':').expect("header delimiter checked");
        if name.eq_ignore_ascii_case("authorization")
            || name.eq_ignore_ascii_case("proxy-authorization")
            || name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case(DEVICE_GRANT_HEADER)
            || name.eq_ignore_ascii_case(DEVICE_ID_HEADER)
        {
            bail!("remote request contains a forbidden HTTP header");
        }
```

**Original bytes precede trusted headers** — `crates/latch/src/cli/remote_access.rs:809-831`

The proxy checks the real device grant for the route, then copies original header bytes before its own grant and identity headers.

```
    }
    let (_, required) = route_for(method, target)
        .ok_or_else(|| anyhow!("HTTP operation is not permitted through Remote Link"))?;
    if !permission.permits(required) {
        bail!("device permission does not allow this operation");
    }
    let mut injected = Vec::with_capacity(request.len() + token.len() + 64);
    injected.extend_from_slice(&request[..end]);
    injected.extend_from_slice(b"\r\nAuthorization: Bearer ");
    injected.extend_from_slice(token.as_bytes());
    injected.extend_from_slice(b"\r\n");
    injected.extend_from_slice(DEVICE_GRANT_HEADER.as_bytes());
    injected.extend_from_slice(b": ");
    injected.extend_from_slice(permission.as_header_value().as_bytes());
    injected.extend_from_slice(b"\r\n");
    injected.extend_from_slice(DEVICE_ID_HEADER.as_bytes());
    injected.extend_from_slice(b": ");
    injected.extend_from_slice(device_id.as_bytes());
    if !websocket_upgrade {
        injected.extend_from_slice(b"\r\nConnection: close");
    }
    injected.extend_from_slice(&request[end..]);
    Ok((injected, required))
```

**Gateway takes the first grant header** — `crates/latch/src/cli/serve/http.rs:278-290`

The loopback request is trusted and HeaderMap::get takes the first value, which can be attacker-supplied after downstream parsing.

```
        .map(|ConnectInfo(address)| address.ip().is_loopback())
        .unwrap_or(state.bind_is_loopback);
    let grant =
        match request.headers().get(DEVICE_GRANT_HEADER) {
            Some(value) if peer_is_loopback => value
                .to_str()
                .ok()
                .and_then(Grant::from_header_value)
                .ok_or_else(|| ApiError::new(StatusCode::BAD_REQUEST, "invalid device grant"))?,
            Some(_) => {
                return Err(ApiError::new(
                    StatusCode::FORBIDDEN,
                    "device grant header is trusted only from the loopback proxy",
```

**Conversation subscription carries the chosen grant** — `crates/latch/src/cli/serve/conversation.rs:125-132`

The grant selected at upgrade becomes the subscriber grant used for later actions.

```
        return;
    }
    let Some((subscriber, outcome)) =
        hub.subscribe_device(&id, grant, device.clone(), query.position())
    else {
        let _ = send(
            &mut socket,
            ConversationServerMessage::Error {
```

**Hub trusts the stored subscriber grant** — `crates/latch/src/conversation/hub.rs:739-769`

Action authorization checks the spoofed subscriber grant, not the underlying device store.

```
        let subscriber_record = actor
            .subscribers
            .get(&subscriber)
            .ok_or_else(|| anyhow::anyhow!("unknown subscriber"))?;
        let grant = subscriber_record.grant;
        let device = subscriber_record.device.clone();
        let payload_digest = Some(action_digest(action));
        if let Some(old) = actor.operations.iter().find(|r| r.id == operation_id) {
            if old.device != device || old.payload_digest != payload_digest {
                return Ok(OperationOutcome::Refused {
                    reason: "operation id conflict: another device or payload already used this id"
                        .into(),
                });
            }
            return Ok(match &old.outcome {
                OperationOutcome::Started => OperationOutcome::Ambiguous,
                v => v.clone(),
            });
        }
        let descriptor = actor
            .actions
            .iter()
            .find(|d| d.id == action.id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown action"))?;
        // Authorization is decided before availability so an observe-only
        // device is refused for the same reason whatever the connector's state.
        if !grant.permits(descriptor.required_grant) {
            return Ok(OperationOutcome::Refused {
                reason: "device grant does not permit this action".into(),
            });
```

**Allowed action writes into the hosted composer** — `crates/latch/src/conversation/connectors/jsonl.rs:1526-1533`

When live composer checks pass, the action submits text to the hosted process.

```
        let screen = self.current_screen(remaining()?)?;
        if action.id == ACTION_SEND_MESSAGE {
            if !screen.lines().any(|line| is_empty_composer(self.id, line)) {
                return Ok(ApplyResult::Refused {
                    reason: format!("the {} composer is no longer empty", self.id),
                });
            }
            self.control()?.submit(text, remaining()?)?;
```

#### Reachability

A paired Observe controller with its own valid endpoint key reaches Remote Link conversation WebSocket upgrade

- **Attacker:** A paired Observe controller with its own valid endpoint key

- **Entry point:** Remote Link conversation WebSocket upgrade

- **Outcome:** Unauthorized conversation messages and resolution of available agent requests

Limitations:
- Not unauthenticated access. The endpoint Noise handshake and paired key are still required.
- Direct terminal, session-create, and stop routes independently require Control at the outer proxy; this finding does not prove access to them.
- Operation epochs and live connector availability remain enforced. Periodic proxy checks compare the real grant to the Observe route requirement, so they do not repair the spoofed subscriber grant.
- The same parsing discrepancy permits a forged device identity; receipt impersonation additionally requires relevant target identifiers.

#### Severity

**High** — An authenticated lower-privilege device can reliably cross an explicit read/write boundary using its existing connection. It does not need the host bearer token. Impact is limited to conversation actions admitted by the connector; direct Control routes remain checked by the proxy.

Additional runtime or deployment evidence could raise or lower this severity.

Impact assessment:
- **Level:** high
- **Why:** Unauthorized conversation messages and resolution of available agent requests

Likelihood assessment:
- **Level:** high
- **Why:** An authenticated lower-privilege device can reliably cross an explicit read/write boundary using its existing connection. It does not need the host bearer token. Impact is limited to conversation actions admitted by the connector; direct Control routes remain checked by the proxy.

#### Remediation

Use the same HTTP parser as the gateway and reconstruct outbound headers from validated fields; remove all caller-supplied authority fields before injecting them. Reject bare CR/LF and duplicate trusted grant/identity headers as defense in depth.

Tests:
- An Observe peer using mixed line endings must be rejected or remain unable to perform any Interact action.
- Reject duplicate or hidden device identity and grant headers; retain legitimate CRLF upgrade behavior.

<a id="finding-2"></a>

### [2] An admitted client can crash the shared relay

| Field | Value |
| --- | --- |
| Severity | high |
| Confidence | high |
| Confidence rationale | Independently reviewed source dataflow and effective controls; no runtime reproduction. |
| Category | denial-of-service |
| CWE | CWE-248 |
| Affected lines | services/relay/src/server.ts:111-114, services/relay/src/server.ts:161-165, services/relay/src/server.ts:172-203, services/control-plane/src/api.ts:543-558 |

#### Summary

Malformed or oversized WebSocket traffic from one admitted client triggers an unhandled error and terminates the relay process, disconnecting unrelated rooms.

#### Root Cause

After claim verification and redemption, admit registers no WebSocket error listener. Installed ws Receiver.haveLength reports WS_ERR_UNSUPPORTED_MESSAGE_LENGTH when its maxPayload is exceeded (services/relay/node_modules/ws/lib/receiver.js:438-451). receiverOnError emits error on the WebSocket (websocket.js:1201-1218). With no listener, the error escapes the event emitter and terminates the process. The upgrade try/catch has already returned and cannot catch later receiver events.

**WebSocket parser has a payload limit** — `services/relay/src/server.ts:111-114`

The parser rejects oversized frames before the application message handler runs.

```
  const server = options.tls
    ? createHttpsServer({ cert: readFileSync(options.tls.certPath), key: readFileSync(options.tls.keyPath) }, listener)
    : createHttpServer(listener);
  const websocket = new WebSocketServer({ noServer: true, perMessageDeflate: false, maxPayload: MAX_RECORD_BYTES });
```

**Verified admission reaches the WebSocket** — `services/relay/src/server.ts:161-165`

A signed and redeemed admission is required.

```
    try {
      const claim = verifyAdmissionClaim(match[1]!, options.publicKeys, options.issuer, Math.floor(now() / 1000));
      const attemptId = randomBytes(16).toString('hex');
      const lease = await options.redeem(claim, attemptId);
      websocket.handleUpgrade(request, socket, head, (ws) => admit(ws, claim, lease, attemptId, sourceIp));
```

**Upgraded sockets lack an error listener** — `services/relay/src/server.ts:172-203`

Only pong, message, and close handlers are installed; the stale-generation branch also lacks an error handler.

```
  function admit(socket: WebSocket, claim: AdmissionClaim, lease: Lease, attemptId: string, sourceIp: string): void {
    let room = rooms.get(claim.roomId);
    if (!room) {
      room = {};
      rooms.set(claim.roomId, room);
    }
    const existing = peer(room, claim.role);
    if (existing && existing.claim.generation >= claim.generation) {
      socket.close(4004, 'stale generation');
      return;
    }
    if (existing) closeRoom(claim.roomId, 4001, 'role replaced');
    room = rooms.get(claim.roomId) ?? {};
    rooms.set(claim.roomId, room);
    const delay = Math.max(0, lease.expiresAt * 1000 - now());
    const value: Peer = {
      socket, claim, lease, attemptId, alive: true, bytesForwarded: 0, sourceIp,
      leaseTimer: setTimeout(() => closeRoom(claim.roomId, 4002, 'lease expired'), delay),
    };
    setPeer(room, claim.role, value);
    connectionsByIp.set(sourceIp, (connectionsByIp.get(sourceIp) ?? 0) + 1);
    socket.binaryType = 'arraybuffer';
    socket.send(JSON.stringify({ type: 'lease_started', leaseId: lease.leaseId, expiresAt: lease.expiresAt }));
    socket.on('pong', () => { value.alive = true; });
    socket.on('message', (data, isBinary) => forward(value, data, isBinary));
    socket.on('close', () => {
      const remaining = (connectionsByIp.get(sourceIp) ?? 1) - 1;
      if (remaining <= 0) connectionsByIp.delete(sourceIp); else connectionsByIp.set(sourceIp, remaining);
      const current = rooms.get(claim.roomId);
      if (!current || peer(current, claim.role) !== value) return;
      closeRoom(claim.roomId, 1000, 'peer gone');
    });
```

**Enrollment claims already receive relay admission** — `services/control-plane/src/api.ts:543-558`

An enrollment code holder receives admission before the separate host completion operation.

```
    if (!claimed) throw new HttpError(409, 'enrollment_unavailable', 'enrollment is invalid, expired, or already claimed');
    const enrollment = await store.getRemoteEnrollment(enrollmentId, unixSeconds(now));
    if (!enrollment) throw new HttpError(409, 'enrollment_unavailable', 'enrollment is unavailable');
    requireAdmissionBudget(context, enrollment.accountId, provisionalDeviceId);
    const controllerAdmission = await issueRemoteAdmission({
      linkId: null, enrollmentId, roomId: enrollment.roomId,
      role: 'controller', purpose: 'enrollment', generation: 1,
    });
    await audit(enrollment.accountId, provisionalDeviceId, 'enrollment.claim', 'allowed');
    return {
      status: 201,
      body: {
        version: 1, enrollmentId, provisionalDeviceId,
        provisionalToken: provisionalCredential.token,
        relayUrl: config.relayUrl, controllerAdmission: controllerAdmission.claim,
      },
```

#### Validation

After claim verification and redemption, admit registers no WebSocket error listener. Installed ws Receiver.haveLength reports WS_ERR_UNSUPPORTED_MESSAGE_LENGTH when its maxPayload is exceeded (services/relay/node_modules/ws/lib/receiver.js:438-451). receiverOnError emits error on the WebSocket (websocket.js:1201-1218). With no listener, the error escapes the event emitter and terminates the process. The upgrade try/catch has already returned and cannot catch later receiver events.

Validation method: offline static source trace

**WebSocket parser has a payload limit** — `services/relay/src/server.ts:111-114`

The parser rejects oversized frames before the application message handler runs.

```
  const server = options.tls
    ? createHttpsServer({ cert: readFileSync(options.tls.certPath), key: readFileSync(options.tls.keyPath) }, listener)
    : createHttpServer(listener);
  const websocket = new WebSocketServer({ noServer: true, perMessageDeflate: false, maxPayload: MAX_RECORD_BYTES });
```

**Verified admission reaches the WebSocket** — `services/relay/src/server.ts:161-165`

A signed and redeemed admission is required.

```
    try {
      const claim = verifyAdmissionClaim(match[1]!, options.publicKeys, options.issuer, Math.floor(now() / 1000));
      const attemptId = randomBytes(16).toString('hex');
      const lease = await options.redeem(claim, attemptId);
      websocket.handleUpgrade(request, socket, head, (ws) => admit(ws, claim, lease, attemptId, sourceIp));
```

**Upgraded sockets lack an error listener** — `services/relay/src/server.ts:172-203`

Only pong, message, and close handlers are installed; the stale-generation branch also lacks an error handler.

```
  function admit(socket: WebSocket, claim: AdmissionClaim, lease: Lease, attemptId: string, sourceIp: string): void {
    let room = rooms.get(claim.roomId);
    if (!room) {
      room = {};
      rooms.set(claim.roomId, room);
    }
    const existing = peer(room, claim.role);
    if (existing && existing.claim.generation >= claim.generation) {
      socket.close(4004, 'stale generation');
      return;
    }
    if (existing) closeRoom(claim.roomId, 4001, 'role replaced');
    room = rooms.get(claim.roomId) ?? {};
    rooms.set(claim.roomId, room);
    const delay = Math.max(0, lease.expiresAt * 1000 - now());
    const value: Peer = {
      socket, claim, lease, attemptId, alive: true, bytesForwarded: 0, sourceIp,
      leaseTimer: setTimeout(() => closeRoom(claim.roomId, 4002, 'lease expired'), delay),
    };
    setPeer(room, claim.role, value);
    connectionsByIp.set(sourceIp, (connectionsByIp.get(sourceIp) ?? 0) + 1);
    socket.binaryType = 'arraybuffer';
    socket.send(JSON.stringify({ type: 'lease_started', leaseId: lease.leaseId, expiresAt: lease.expiresAt }));
    socket.on('pong', () => { value.alive = true; });
    socket.on('message', (data, isBinary) => forward(value, data, isBinary));
    socket.on('close', () => {
      const remaining = (connectionsByIp.get(sourceIp) ?? 1) - 1;
      if (remaining <= 0) connectionsByIp.delete(sourceIp); else connectionsByIp.set(sourceIp, remaining);
      const current = rooms.get(claim.roomId);
      if (!current || peer(current, claim.role) !== value) return;
      closeRoom(claim.roomId, 1000, 'peer gone');
    });
```

**Enrollment claims already receive relay admission** — `services/control-plane/src/api.ts:543-558`

An enrollment code holder receives admission before the separate host completion operation.

```
    if (!claimed) throw new HttpError(409, 'enrollment_unavailable', 'enrollment is invalid, expired, or already claimed');
    const enrollment = await store.getRemoteEnrollment(enrollmentId, unixSeconds(now));
    if (!enrollment) throw new HttpError(409, 'enrollment_unavailable', 'enrollment is unavailable');
    requireAdmissionBudget(context, enrollment.accountId, provisionalDeviceId);
    const controllerAdmission = await issueRemoteAdmission({
      linkId: null, enrollmentId, roomId: enrollment.roomId,
      role: 'controller', purpose: 'enrollment', generation: 1,
    });
    await audit(enrollment.accountId, provisionalDeviceId, 'enrollment.claim', 'allowed');
    return {
      status: 201,
      body: {
        version: 1, enrollmentId, provisionalDeviceId,
        provisionalToken: provisionalCredential.token,
        relayUrl: config.relayUrl, controllerAdmission: controllerAdmission.claim,
      },
```

Counterevidence and remaining uncertainty:
- Signature verification and single-use redemption block anonymous clients.
- The application forward() record-size guard is not reached for parser-level rejection.
- No runtime crash was induced; the finding is supported by application and installed dependency source.

Limitations:
- No application code or exploit was executed; production exposure was not tested.

#### Dataflow

After claim verification and redemption, admit registers no WebSocket error listener. Installed ws Receiver.haveLength reports WS_ERR_UNSUPPORTED_MESSAGE_LENGTH when its maxPayload is exceeded (services/relay/node_modules/ws/lib/receiver.js:438-451). receiverOnError emits error on the WebSocket (websocket.js:1201-1218). With no listener, the error escapes the event emitter and terminates the process. The upgrade try/catch has already returned and cannot catch later receiver events.

- **Source:** /v1/connect followed by malformed WebSocket traffic

- **Sink:** Relay process termination and loss of all active rooms

- **Outcome:** Relay process termination and loss of all active rooms

**WebSocket parser has a payload limit** — `services/relay/src/server.ts:111-114`

The parser rejects oversized frames before the application message handler runs.

```
  const server = options.tls
    ? createHttpsServer({ cert: readFileSync(options.tls.certPath), key: readFileSync(options.tls.keyPath) }, listener)
    : createHttpServer(listener);
  const websocket = new WebSocketServer({ noServer: true, perMessageDeflate: false, maxPayload: MAX_RECORD_BYTES });
```

**Verified admission reaches the WebSocket** — `services/relay/src/server.ts:161-165`

A signed and redeemed admission is required.

```
    try {
      const claim = verifyAdmissionClaim(match[1]!, options.publicKeys, options.issuer, Math.floor(now() / 1000));
      const attemptId = randomBytes(16).toString('hex');
      const lease = await options.redeem(claim, attemptId);
      websocket.handleUpgrade(request, socket, head, (ws) => admit(ws, claim, lease, attemptId, sourceIp));
```

**Upgraded sockets lack an error listener** — `services/relay/src/server.ts:172-203`

Only pong, message, and close handlers are installed; the stale-generation branch also lacks an error handler.

```
  function admit(socket: WebSocket, claim: AdmissionClaim, lease: Lease, attemptId: string, sourceIp: string): void {
    let room = rooms.get(claim.roomId);
    if (!room) {
      room = {};
      rooms.set(claim.roomId, room);
    }
    const existing = peer(room, claim.role);
    if (existing && existing.claim.generation >= claim.generation) {
      socket.close(4004, 'stale generation');
      return;
    }
    if (existing) closeRoom(claim.roomId, 4001, 'role replaced');
    room = rooms.get(claim.roomId) ?? {};
    rooms.set(claim.roomId, room);
    const delay = Math.max(0, lease.expiresAt * 1000 - now());
    const value: Peer = {
      socket, claim, lease, attemptId, alive: true, bytesForwarded: 0, sourceIp,
      leaseTimer: setTimeout(() => closeRoom(claim.roomId, 4002, 'lease expired'), delay),
    };
    setPeer(room, claim.role, value);
    connectionsByIp.set(sourceIp, (connectionsByIp.get(sourceIp) ?? 0) + 1);
    socket.binaryType = 'arraybuffer';
    socket.send(JSON.stringify({ type: 'lease_started', leaseId: lease.leaseId, expiresAt: lease.expiresAt }));
    socket.on('pong', () => { value.alive = true; });
    socket.on('message', (data, isBinary) => forward(value, data, isBinary));
    socket.on('close', () => {
      const remaining = (connectionsByIp.get(sourceIp) ?? 1) - 1;
      if (remaining <= 0) connectionsByIp.delete(sourceIp); else connectionsByIp.set(sourceIp, remaining);
      const current = rooms.get(claim.roomId);
      if (!current || peer(current, claim.role) !== value) return;
      closeRoom(claim.roomId, 1000, 'peer gone');
    });
```

**Enrollment claims already receive relay admission** — `services/control-plane/src/api.ts:543-558`

An enrollment code holder receives admission before the separate host completion operation.

```
    if (!claimed) throw new HttpError(409, 'enrollment_unavailable', 'enrollment is invalid, expired, or already claimed');
    const enrollment = await store.getRemoteEnrollment(enrollmentId, unixSeconds(now));
    if (!enrollment) throw new HttpError(409, 'enrollment_unavailable', 'enrollment is unavailable');
    requireAdmissionBudget(context, enrollment.accountId, provisionalDeviceId);
    const controllerAdmission = await issueRemoteAdmission({
      linkId: null, enrollmentId, roomId: enrollment.roomId,
      role: 'controller', purpose: 'enrollment', generation: 1,
    });
    await audit(enrollment.accountId, provisionalDeviceId, 'enrollment.claim', 'allowed');
    return {
      status: 201,
      body: {
        version: 1, enrollmentId, provisionalDeviceId,
        provisionalToken: provisionalCredential.token,
        relayUrl: config.relayUrl, controllerAdmission: controllerAdmission.claim,
      },
```

#### Reachability

A client holding a valid relay admission, including a holder of a current enrollment code before host approval reaches /v1/connect followed by malformed WebSocket traffic

- **Attacker:** A client holding a valid relay admission, including a holder of a current enrollment code before host approval

- **Entry point:** /v1/connect followed by malformed WebSocket traffic

- **Outcome:** Relay process termination and loss of all active rooms

Limitations:
- Signature verification and single-use redemption block anonymous clients.
- The application forward() record-size guard is not reached for parser-level rejection.
- No runtime crash was induced; the finding is supported by application and installed dependency source.

#### Severity

**High** — One admitted client can cause a process-wide availability failure without completing the endpoint Noise handshake. Admission limits and single-use claims restrict access but do not contain malformed-frame errors.

Additional runtime or deployment evidence could raise or lower this severity.

Impact assessment:
- **Level:** high
- **Why:** Relay process termination and loss of all active rooms

Likelihood assessment:
- **Level:** high
- **Why:** One admitted client can cause a process-wide availability failure without completing the endpoint Noise handshake. Admission limits and single-use claims restrict access but do not contain malformed-frame errors.

#### Remediation

Attach an error handler immediately to every upgraded WebSocket, including sockets later rejected as stale. Terminate and clean up only the affected peer or room.

Tests:
- In an isolated process, verify malformed and oversized frames close the offending connection while another room and health checks remain available.

<a id="finding-3"></a>

### [3] Initial installer does not verify the release publisher before executing binaries

| Field | Value |
| --- | --- |
| Severity | low |
| Confidence | high |
| Confidence rationale | Independently reviewed source dataflow and effective controls; no runtime reproduction. |
| Category | signature-verification |
| CWE | CWE-347 |
| Affected lines | scripts/install-cli.sh:25-38, scripts/install-cli.sh:39-45, crates/latch/src/cli/update/mod.rs:568-582 |

#### Summary

The installer executes downloaded binaries after generic signature verification, without requiring Latch's signing identity. Replacement release assets can therefore run code without the genuine Latch signing key.

#### Root Cause

The authentic install script downloads the archive and checksums from one GitHub release, verifies a matching manifest and generic code-signature validity, then executes each downloaded binary for a version check. No pinned Developer ID requirement or publisher team comparison precedes those executions. A release-asset attacker can replace both archive and checksum with payloads signed under a different identity.

**Checksum and generic signatures do not pin publisher** — `scripts/install-cli.sh:25-38`

The archive and checksum come from the same release; codesign --verify --strict does not specify the expected publisher.

```
curl -fL "$release_base/$archive" -o "$work_dir/$archive"
curl -fL "$release_base/checksums.txt" -o "$work_dir/checksums.txt"
awk -v archive="$archive" '$2 == archive { print }' "$work_dir/checksums.txt" > "$work_dir/archive.sha256"
if [[ ! -s "$work_dir/archive.sha256" ]]; then
    echo "The release checksum does not list $archive." >&2
    exit 1
fi
(cd "$work_dir" && shasum -a 256 -c archive.sha256)
ditto -x -k "$work_dir/$archive" "$work_dir/extracted"
/usr/bin/python3 -c 'import json,sys; p=json.load(open(sys.argv[1])); expected=["latch","latch-remote","latchd"]; assert p == {"formatVersion":1,"version":sys.argv[2],"target":sys.argv[3],"binaries":expected}' \
    "$work_dir/extracted/latch-payload.json" "$version" "$target"
codesign --verify --strict "$work_dir/extracted/latch"
codesign --verify --strict "$work_dir/extracted/latch-remote"
codesign --verify --strict "$work_dir/extracted/latchd"
```

**Version checks execute downloaded code** — `scripts/install-cli.sh:39-45`

All three downloaded programs execute before installation.

```
"$work_dir/extracted/latch" --version | grep -F " $version"
"$work_dir/extracted/latch-remote" --version | grep -F " $version"
"$work_dir/extracted/latchd" version | grep -Fx "latchd $version protocol 1"
mkdir -p "$HOME/.local/bin"
install -m 0755 "$work_dir/extracted/latch-remote" "$HOME/.local/bin/latch-remote"
install -m 0755 "$work_dir/extracted/latchd" "$HOME/.local/bin/latchd"
install -m 0755 "$work_dir/extracted/latch" "$HOME/.local/bin/latch"
```

**Existing signed installations check the team** — `crates/latch/src/cli/update/mod.rs:568-582`

The Rust updater compares teams, illustrating a stronger control absent from initial installation.

```
fn verify_signature(current: &Path, replacement: &Path) -> Result<()> {
    let expected_team = match signing_team(current) {
        Some(team) => team,
        None => return Ok(()),
    };
    match signing_team(replacement) {
        Some(team) if team == expected_team => Ok(()),
        Some(team) => bail!(
            "the downloaded binary is signed by team {team}, but this install is signed by \
             {expected_team}; nothing was installed"
        ),
        None => bail!(
            "the downloaded binary is not validly signed, but this install is (team \
             {expected_team}); nothing was installed"
        ),
```

#### Validation

The authentic install script downloads the archive and checksums from one GitHub release, verifies a matching manifest and generic code-signature validity, then executes each downloaded binary for a version check. No pinned Developer ID requirement or publisher team comparison precedes those executions. A release-asset attacker can replace both archive and checksum with payloads signed under a different identity.

Validation method: offline static source trace

**Checksum and generic signatures do not pin publisher** — `scripts/install-cli.sh:25-38`

The archive and checksum come from the same release; codesign --verify --strict does not specify the expected publisher.

```
curl -fL "$release_base/$archive" -o "$work_dir/$archive"
curl -fL "$release_base/checksums.txt" -o "$work_dir/checksums.txt"
awk -v archive="$archive" '$2 == archive { print }' "$work_dir/checksums.txt" > "$work_dir/archive.sha256"
if [[ ! -s "$work_dir/archive.sha256" ]]; then
    echo "The release checksum does not list $archive." >&2
    exit 1
fi
(cd "$work_dir" && shasum -a 256 -c archive.sha256)
ditto -x -k "$work_dir/$archive" "$work_dir/extracted"
/usr/bin/python3 -c 'import json,sys; p=json.load(open(sys.argv[1])); expected=["latch","latch-remote","latchd"]; assert p == {"formatVersion":1,"version":sys.argv[2],"target":sys.argv[3],"binaries":expected}' \
    "$work_dir/extracted/latch-payload.json" "$version" "$target"
codesign --verify --strict "$work_dir/extracted/latch"
codesign --verify --strict "$work_dir/extracted/latch-remote"
codesign --verify --strict "$work_dir/extracted/latchd"
```

**Version checks execute downloaded code** — `scripts/install-cli.sh:39-45`

All three downloaded programs execute before installation.

```
"$work_dir/extracted/latch" --version | grep -F " $version"
"$work_dir/extracted/latch-remote" --version | grep -F " $version"
"$work_dir/extracted/latchd" version | grep -Fx "latchd $version protocol 1"
mkdir -p "$HOME/.local/bin"
install -m 0755 "$work_dir/extracted/latch-remote" "$HOME/.local/bin/latch-remote"
install -m 0755 "$work_dir/extracted/latchd" "$HOME/.local/bin/latchd"
install -m 0755 "$work_dir/extracted/latch" "$HOME/.local/bin/latch"
```

**Existing signed installations check the team** — `crates/latch/src/cli/update/mod.rs:568-582`

The Rust updater compares teams, illustrating a stronger control absent from initial installation.

```
fn verify_signature(current: &Path, replacement: &Path) -> Result<()> {
    let expected_team = match signing_team(current) {
        Some(team) => team,
        None => return Ok(()),
    };
    match signing_team(replacement) {
        Some(team) if team == expected_team => Ok(()),
        Some(team) => bail!(
            "the downloaded binary is signed by team {team}, but this install is signed by \
             {expected_team}; nothing was installed"
        ),
        None => bail!(
            "the downloaded binary is not validly signed, but this install is (team \
             {expected_team}); nothing was installed"
        ),
```

Counterevidence and remaining uncertainty:
- The initial download URLs are fixed GitHub HTTPS URLs. Ordinary network tampering is not established.
- The legitimate release workflow signs binaries. This finding requires artifact substitution outside that legitimate signing path.
- Existing signed installations use a team comparison in the Rust updater; this finding concerns initial installation.
- Native codesign/notarization behavior and release infrastructure permissions were not dynamically tested.

Limitations:
- No application code or exploit was executed; production exposure was not tested.

#### Dataflow

The authentic install script downloads the archive and checksums from one GitHub release, verifies a matching manifest and generic code-signature validity, then executes each downloaded binary for a version check. No pinned Developer ID requirement or publisher team comparison precedes those executions. A release-asset attacker can replace both archive and checksum with payloads signed under a different identity.

- **Source:** Initial install through scripts/install-cli.sh

- **Sink:** Execution of substituted code as the installing user

- **Outcome:** Execution of substituted code as the installing user

**Checksum and generic signatures do not pin publisher** — `scripts/install-cli.sh:25-38`

The archive and checksum come from the same release; codesign --verify --strict does not specify the expected publisher.

```
curl -fL "$release_base/$archive" -o "$work_dir/$archive"
curl -fL "$release_base/checksums.txt" -o "$work_dir/checksums.txt"
awk -v archive="$archive" '$2 == archive { print }' "$work_dir/checksums.txt" > "$work_dir/archive.sha256"
if [[ ! -s "$work_dir/archive.sha256" ]]; then
    echo "The release checksum does not list $archive." >&2
    exit 1
fi
(cd "$work_dir" && shasum -a 256 -c archive.sha256)
ditto -x -k "$work_dir/$archive" "$work_dir/extracted"
/usr/bin/python3 -c 'import json,sys; p=json.load(open(sys.argv[1])); expected=["latch","latch-remote","latchd"]; assert p == {"formatVersion":1,"version":sys.argv[2],"target":sys.argv[3],"binaries":expected}' \
    "$work_dir/extracted/latch-payload.json" "$version" "$target"
codesign --verify --strict "$work_dir/extracted/latch"
codesign --verify --strict "$work_dir/extracted/latch-remote"
codesign --verify --strict "$work_dir/extracted/latchd"
```

**Version checks execute downloaded code** — `scripts/install-cli.sh:39-45`

All three downloaded programs execute before installation.

```
"$work_dir/extracted/latch" --version | grep -F " $version"
"$work_dir/extracted/latch-remote" --version | grep -F " $version"
"$work_dir/extracted/latchd" version | grep -Fx "latchd $version protocol 1"
mkdir -p "$HOME/.local/bin"
install -m 0755 "$work_dir/extracted/latch-remote" "$HOME/.local/bin/latch-remote"
install -m 0755 "$work_dir/extracted/latchd" "$HOME/.local/bin/latchd"
install -m 0755 "$work_dir/extracted/latch" "$HOME/.local/bin/latch"
```

**Existing signed installations check the team** — `crates/latch/src/cli/update/mod.rs:568-582`

The Rust updater compares teams, illustrating a stronger control absent from initial installation.

```
fn verify_signature(current: &Path, replacement: &Path) -> Result<()> {
    let expected_team = match signing_team(current) {
        Some(team) => team,
        None => return Ok(()),
    };
    match signing_team(replacement) {
        Some(team) if team == expected_team => Ok(()),
        Some(team) => bail!(
            "the downloaded binary is signed by team {team}, but this install is signed by \
             {expected_team}; nothing was installed"
        ),
        None => bail!(
            "the downloaded binary is not validly signed, but this install is (team \
             {expected_team}); nothing was installed"
        ),
```

#### Reachability

An attacker able to replace release assets and checksums but lacking Latch's signing key reaches Initial install through scripts/install-cli.sh

- **Attacker:** An attacker able to replace release assets and checksums but lacking Latch's signing key

- **Entry point:** Initial install through scripts/install-cli.sh

- **Outcome:** Execution of substituted code as the installing user

Limitations:
- The initial download URLs are fixed GitHub HTTPS URLs. Ordinary network tampering is not established.
- The legitimate release workflow signs binaries. This finding requires artifact substitution outside that legitimate signing path.
- Existing signed installations use a team comparison in the Rust updater; this finding concerns initial installation.
- Native codesign/notarization behavior and release infrastructure permissions were not dynamically tested.

#### Severity

**Low** — Impact is arbitrary user-level code execution, but exploitation requires compromise of release assets and their checksum file while the victim uses an authentic installer. This is a conditional supply-chain boundary, not an anonymous network exploit.

Additional runtime or deployment evidence could raise or lower this severity.

Impact assessment:
- **Level:** high
- **Why:** Execution of substituted code as the installing user

Likelihood assessment:
- **Level:** low
- **Why:** Impact is arbitrary user-level code execution, but exploitation requires compromise of release assets and their checksum file while the victim uses an authentic installer. This is a conditional supply-chain boundary, not an anonymous network exploit.

#### Remediation

Before running any downloaded payload, require a pinned Latch Developer ID signing requirement with the trusted Apple anchor and expected Team ID. Retain checksum and exact-manifest checks as additional integrity controls.

Tests:
- Reject payloads signed by a different team or ad-hoc signed before any version command executes.
- Accept a legitimate release signed by the pinned identity.

<a id="finding-4"></a>

### [4] Concurrent upgrades bypass relay connection budgets

| Field | Value |
| --- | --- |
| Severity | low |
| Confidence | high |
| Confidence rationale | Independently reviewed source dataflow and effective controls; no runtime reproduction. |
| Category | resource-limit |
| CWE | CWE-362 |
| Affected lines | services/relay/src/server.ts:147-165, services/relay/src/server.ts:186-196, services/control-plane/src/config.ts:148-153 |

#### Summary

Connection limits count only admitted sockets. Concurrent requests can pass those limits while awaiting redemption and subsequently exceed the configured per-IP or global cap.

#### Root Cause

The upgrade handler reads connectionsByIp, checks the limits, then awaits redeem without reserving capacity. admit increments the count only after that await and does not recheck. Concurrent valid admissions for distinct rooms can therefore exceed the intended cap; replayed claims still consume concurrent redemption work although only one succeeds.

**Capacity is checked before async redemption** — `services/relay/src/server.ts:147-165`

No pending slot is reserved before awaiting the control-plane response.

```
    const sourceIp = (socket as import('node:net').Socket).remoteAddress ?? 'unknown';
    const active = [...connectionsByIp.values()].reduce((sum, count) => sum + count, 0);
    if (active >= (options.maxConnections ?? DEFAULT_MAX_CONNECTIONS) ||
        (connectionsByIp.get(sourceIp) ?? 0) >= (options.maxConnectionsPerIp ?? DEFAULT_MAX_CONNECTIONS_PER_IP)) {
      socket.write('HTTP/1.1 429 Too Many Requests\r\nConnection: close\r\n\r\n');
      socket.destroy();
      return;
    }
    const match = /^Bearer\s+([A-Za-z0-9._-]+)$/.exec(request.headers.authorization ?? '');
    if (!match) {
      socket.write('HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n');
      socket.destroy();
      return;
    }
    try {
      const claim = verifyAdmissionClaim(match[1]!, options.publicKeys, options.issuer, Math.floor(now() / 1000));
      const attemptId = randomBytes(16).toString('hex');
      const lease = await options.redeem(claim, attemptId);
      websocket.handleUpgrade(request, socket, head, (ws) => admit(ws, claim, lease, attemptId, sourceIp));
```

**Admission increments without rechecking** — `services/relay/src/server.ts:186-196`

Several requests that passed the old count can all become active.

```
    const delay = Math.max(0, lease.expiresAt * 1000 - now());
    const value: Peer = {
      socket, claim, lease, attemptId, alive: true, bytesForwarded: 0, sourceIp,
      leaseTimer: setTimeout(() => closeRoom(claim.roomId, 4002, 'lease expired'), delay),
    };
    setPeer(room, claim.role, value);
    connectionsByIp.set(sourceIp, (connectionsByIp.get(sourceIp) ?? 0) + 1);
    socket.binaryType = 'arraybuffer';
    socket.send(JSON.stringify({ type: 'lease_started', leaseId: lease.leaseId, expiresAt: lease.expiresAt }));
    socket.on('pong', () => { value.alive = true; });
    socket.on('message', (data, isBinary) => forward(value, data, isBinary));
```

**Issuance allows more than the per-IP socket cap** — `services/control-plane/src/config.ts:148-153`

The default per-device admission rate is 60/minute; the relay per-IP cap is 16, so distinct enrollment admissions can exceed it.

```
    maxDevicesPerAccount: integer(env, 'MAX_DEVICES_PER_ACCOUNT', 32, 2, 256),
    rateLimitPerMinute: integer(env, 'RATE_LIMIT_PER_MINUTE', 240, 10, 10_000),
    admissionRatePerDevice: integer(env, 'REMOTE_ADMISSION_RATE_PER_DEVICE', 60, 4, 1_000),
    admissionRatePerOwner: integer(env, 'REMOTE_ADMISSION_RATE_PER_OWNER', 180, 8, 5_000),
    admissionRatePerIp: integer(env, 'REMOTE_ADMISSION_RATE_PER_IP', 120, 4, 5_000),
    admissionRateGlobal: integer(env, 'REMOTE_ADMISSION_RATE_GLOBAL', 1_000, 16, 100_000),
```

#### Validation

The upgrade handler reads connectionsByIp, checks the limits, then awaits redeem without reserving capacity. admit increments the count only after that await and does not recheck. Concurrent valid admissions for distinct rooms can therefore exceed the intended cap; replayed claims still consume concurrent redemption work although only one succeeds.

Validation method: offline static source trace

**Capacity is checked before async redemption** — `services/relay/src/server.ts:147-165`

No pending slot is reserved before awaiting the control-plane response.

```
    const sourceIp = (socket as import('node:net').Socket).remoteAddress ?? 'unknown';
    const active = [...connectionsByIp.values()].reduce((sum, count) => sum + count, 0);
    if (active >= (options.maxConnections ?? DEFAULT_MAX_CONNECTIONS) ||
        (connectionsByIp.get(sourceIp) ?? 0) >= (options.maxConnectionsPerIp ?? DEFAULT_MAX_CONNECTIONS_PER_IP)) {
      socket.write('HTTP/1.1 429 Too Many Requests\r\nConnection: close\r\n\r\n');
      socket.destroy();
      return;
    }
    const match = /^Bearer\s+([A-Za-z0-9._-]+)$/.exec(request.headers.authorization ?? '');
    if (!match) {
      socket.write('HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n');
      socket.destroy();
      return;
    }
    try {
      const claim = verifyAdmissionClaim(match[1]!, options.publicKeys, options.issuer, Math.floor(now() / 1000));
      const attemptId = randomBytes(16).toString('hex');
      const lease = await options.redeem(claim, attemptId);
      websocket.handleUpgrade(request, socket, head, (ws) => admit(ws, claim, lease, attemptId, sourceIp));
```

**Admission increments without rechecking** — `services/relay/src/server.ts:186-196`

Several requests that passed the old count can all become active.

```
    const delay = Math.max(0, lease.expiresAt * 1000 - now());
    const value: Peer = {
      socket, claim, lease, attemptId, alive: true, bytesForwarded: 0, sourceIp,
      leaseTimer: setTimeout(() => closeRoom(claim.roomId, 4002, 'lease expired'), delay),
    };
    setPeer(room, claim.role, value);
    connectionsByIp.set(sourceIp, (connectionsByIp.get(sourceIp) ?? 0) + 1);
    socket.binaryType = 'arraybuffer';
    socket.send(JSON.stringify({ type: 'lease_started', leaseId: lease.leaseId, expiresAt: lease.expiresAt }));
    socket.on('pong', () => { value.alive = true; });
    socket.on('message', (data, isBinary) => forward(value, data, isBinary));
```

**Issuance allows more than the per-IP socket cap** — `services/control-plane/src/config.ts:148-153`

The default per-device admission rate is 60/minute; the relay per-IP cap is 16, so distinct enrollment admissions can exceed it.

```
    maxDevicesPerAccount: integer(env, 'MAX_DEVICES_PER_ACCOUNT', 32, 2, 256),
    rateLimitPerMinute: integer(env, 'RATE_LIMIT_PER_MINUTE', 240, 10, 10_000),
    admissionRatePerDevice: integer(env, 'REMOTE_ADMISSION_RATE_PER_DEVICE', 60, 4, 1_000),
    admissionRatePerOwner: integer(env, 'REMOTE_ADMISSION_RATE_PER_OWNER', 180, 8, 5_000),
    admissionRatePerIp: integer(env, 'REMOTE_ADMISSION_RATE_PER_IP', 120, 4, 5_000),
    admissionRateGlobal: integer(env, 'REMOTE_ADMISSION_RATE_GLOBAL', 1_000, 16, 100_000),
```

Counterevidence and remaining uncertainty:
- One ticket cannot create multiple successful connections because redemption is single-use.
- Actual process exhaustion has not been demonstrated.
- Record-size, buffer and bandwidth bounds remain active.

Limitations:
- No application code or exploit was executed; production exposure was not tested.

#### Dataflow

The upgrade handler reads connectionsByIp, checks the limits, then awaits redeem without reserving capacity. admit increments the count only after that await and does not recheck. Concurrent valid admissions for distinct rooms can therefore exceed the intended cap; replayed claims still consume concurrent redemption work although only one succeeds.

- **Source:** Concurrent /v1/connect upgrades

- **Sink:** Bypass of shared-service connection budgets and excess redemption work

- **Outcome:** Bypass of shared-service connection budgets and excess redemption work

**Capacity is checked before async redemption** — `services/relay/src/server.ts:147-165`

No pending slot is reserved before awaiting the control-plane response.

```
    const sourceIp = (socket as import('node:net').Socket).remoteAddress ?? 'unknown';
    const active = [...connectionsByIp.values()].reduce((sum, count) => sum + count, 0);
    if (active >= (options.maxConnections ?? DEFAULT_MAX_CONNECTIONS) ||
        (connectionsByIp.get(sourceIp) ?? 0) >= (options.maxConnectionsPerIp ?? DEFAULT_MAX_CONNECTIONS_PER_IP)) {
      socket.write('HTTP/1.1 429 Too Many Requests\r\nConnection: close\r\n\r\n');
      socket.destroy();
      return;
    }
    const match = /^Bearer\s+([A-Za-z0-9._-]+)$/.exec(request.headers.authorization ?? '');
    if (!match) {
      socket.write('HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n');
      socket.destroy();
      return;
    }
    try {
      const claim = verifyAdmissionClaim(match[1]!, options.publicKeys, options.issuer, Math.floor(now() / 1000));
      const attemptId = randomBytes(16).toString('hex');
      const lease = await options.redeem(claim, attemptId);
      websocket.handleUpgrade(request, socket, head, (ws) => admit(ws, claim, lease, attemptId, sourceIp));
```

**Admission increments without rechecking** — `services/relay/src/server.ts:186-196`

Several requests that passed the old count can all become active.

```
    const delay = Math.max(0, lease.expiresAt * 1000 - now());
    const value: Peer = {
      socket, claim, lease, attemptId, alive: true, bytesForwarded: 0, sourceIp,
      leaseTimer: setTimeout(() => closeRoom(claim.roomId, 4002, 'lease expired'), delay),
    };
    setPeer(room, claim.role, value);
    connectionsByIp.set(sourceIp, (connectionsByIp.get(sourceIp) ?? 0) + 1);
    socket.binaryType = 'arraybuffer';
    socket.send(JSON.stringify({ type: 'lease_started', leaseId: lease.leaseId, expiresAt: lease.expiresAt }));
    socket.on('pong', () => { value.alive = true; });
    socket.on('message', (data, isBinary) => forward(value, data, isBinary));
```

**Issuance allows more than the per-IP socket cap** — `services/control-plane/src/config.ts:148-153`

The default per-device admission rate is 60/minute; the relay per-IP cap is 16, so distinct enrollment admissions can exceed it.

```
    maxDevicesPerAccount: integer(env, 'MAX_DEVICES_PER_ACCOUNT', 32, 2, 256),
    rateLimitPerMinute: integer(env, 'RATE_LIMIT_PER_MINUTE', 240, 10, 10_000),
    admissionRatePerDevice: integer(env, 'REMOTE_ADMISSION_RATE_PER_DEVICE', 60, 4, 1_000),
    admissionRatePerOwner: integer(env, 'REMOTE_ADMISSION_RATE_PER_OWNER', 180, 8, 5_000),
    admissionRatePerIp: integer(env, 'REMOTE_ADMISSION_RATE_PER_IP', 120, 4, 5_000),
    admissionRateGlobal: integer(env, 'REMOTE_ADMISSION_RATE_GLOBAL', 1_000, 16, 100_000),
```

#### Reachability

An authorized host or clients possessing multiple fresh admissions for distinct rooms reaches Concurrent /v1/connect upgrades

- **Attacker:** An authorized host or clients possessing multiple fresh admissions for distinct rooms

- **Entry point:** Concurrent /v1/connect upgrades

- **Outcome:** Bypass of shared-service connection budgets and excess redemption work

Limitations:
- One ticket cannot create multiple successful connections because redemption is single-use.
- Actual process exhaustion has not been demonstrated.
- Record-size, buffer and bandwidth bounds remain active.

#### Severity

**Low** — The race is deterministic in source, but meaningful impact needs multiple admissions and sufficient concurrency. Per-record, buffer and lease limits still apply; service exhaustion was not demonstrated.

Additional runtime or deployment evidence could raise or lower this severity.

Impact assessment:
- **Level:** medium
- **Why:** Bypass of shared-service connection budgets and excess redemption work

Likelihood assessment:
- **Level:** medium
- **Why:** The race is deterministic in source, but meaningful impact needs multiple admissions and sufficient concurrency. Per-record, buffer and lease limits still apply; service exhaustion was not demonstrated.

#### Remediation

Reserve global and per-IP capacity before awaiting redemption, release it on every failure/disconnect path, and apply a bounded redemption timeout.

Tests:
- Hold multiple redemption promises pending and verify excess requests are refused before redemption begins.
- Verify reservations are released after rejection, timeout, malformed upgrade and disconnect.

## Reviewed Surfaces

| Surface | Risk Area | Outcome | Notes |
| --- | --- | --- | --- |
| Remote device grants and conversation actions | not recorded | Reported | Parser disagreement confirmed. Bearer authentication, route grants and periodic revocation exist, but do not prevent the reported upgrade/header discrepancy (remote_access.rs:760-831; serve/http.rs:278-335). |
| Relay admission and availability | not recorded | Reported | Signed claims, exact fields, role/room/lease binding and single-use redemption checked. Missing socket error handlers and pending-capacity reservations are reported (services/relay/src/server.ts:141-203). |
| Control-plane account, pairing and enrollment | not recorded | No issue found | Reviewed implementation enforces account/role checks and host completion with exact proposed key. PostgreSQL uses atomic single-use redemption and a transaction for same-enrollment completion. Concurrency and deployment questions below remain (api.ts:562-588; store/postgres.ts). |
| Local UID and process authority | not recorded | No issue found | Socket paths, framing and peer UID checks reviewed. Daemon rejects foreign UID before dispatch; client authenticates server UID. Same-UID execution is not a separate tenant boundary (latchd/src/daemon.rs:440-478; client.rs:27-31; peer.rs). |
| Installer and update integrity | not recorded | Reported | Initial installer lacks publisher pinning. Rust updater compares signing teams for recognized signed installations; unsigned/development installations intentionally skip that check (update/mod.rs:568-615). |
| Release and deployment workflows | not recorded | No issue found | Inspected release signing is tag-triggered, production deployment requires main push/environment, PR checks do not reference signing/deployment secrets. No separate exploit established. |
| Noise transport, FFI and all framing | not recorded | Reported | Follow-up `coo:1001.z2qn` fully reviewed `latch-transport`, `latch-transport-ffi`, and the Swift connector. Noise XX pins, the enrollment secret, and service/purpose separation held. Unbounded pre-handshake buffering, a stallable LAN acceptor, unrestricted Bonjour targets, and a missing phone `wss://` pin are in `transport-native-follow-up.md`. |
| Native clients, rendering and SDKs | not recorded | Reported | Follow-up reviewed pairing approval, Keychain, the loopback capability, gateway construction, helper IPC, and the Desktop updater. The approval sentence interpolates the peer device name. Conversation rendering and remaining UI were not fully audited. |

## Open Questions And Follow Up

- Do deployed database connections require verified TLS? DATABASE_SSL_REJECT_UNAUTHORIZED defaults false, and PostgresStore disables TLS based on substring matching across DATABASE_URL (config.ts:144-146; store/postgres.ts:140-147).
- Does a redemption already in flight during revocation get admitted after invalidation? The relay ignores notAfter and closes current rooms only. Endpoint checks limit impact; unauthorized session access was not established.
- Can concurrent generation updates or device-count checks cause a security-relevant cross-principal outcome? Duplicate/regressing generations and account count races need focused concurrency tests.
- Does the supported native archive extractor prevent traversal and symlink escape before publisher verification? No escape was established.
- Is the documented --allow-remote bearer path intended to work? Current gateway middleware denies non-loopback application requests despite the opt-in listener documentation.
- Transport/FFI and the security-relevant native pairing, key-storage, loopback, and updater paths were reviewed in `coo:1001.z2qn`. Findings are in `transport-native-follow-up.md`. Conversation rendering, generated FFI, and the remaining deferred paths were not fully audited. No full-repository completeness claim is made.
- Not fully audited. Some listed files had architecture mapping or finding-specific excerpts reviewed; those are not counted as full-file coverage. Tests, UI, generated code and supporting modules also remain outside completed full-file review.
  - Follow-up prompt: Review deferred unit remaining-source and close its stated proof gap. Paths: apps/LatchDesktop/Package.swift, apps/LatchDesktop/Sources/LatchDesktop/AppChromeState.swift, apps/LatchDesktop/Sources/LatchDesktop/AttentionForwarder.swift, apps/LatchDesktop/Sources/LatchDesktop/ControlPlaneHost.swift, apps/LatchDesktop/Sources/LatchDesktop/HelperLineReader.swift, apps/LatchDesktop/Sources/LatchDesktop/LatchClient.swift, apps/LatchDesktop/Sources/LatchDesktop/LatchDesktopApp.swift, apps/LatchDesktop/Sources/LatchDesktop/MenuTextWrapper.swift, apps/LatchDesktop/Sources/LatchDesktop/Models.swift, apps/LatchDesktop/Sources/LatchDesktop/RemoteAccessController.swift, apps/LatchDesktop/Sources/LatchDesktop/RemoteAccessModels.swift, apps/LatchDesktop/Sources/LatchDesktop/RemoteAccessSupervisor.swift, apps/LatchDesktop/Sources/LatchDesktop/RemoteAccessView.swift, apps/LatchDesktop/Sources/LatchDesktop/SessionStore.swift, apps/LatchDesktop/Sources/LatchDesktop/SidebarToolbarSeparator.swift, apps/LatchDesktop/Sources/LatchDesktop/SleepAssertion.swift, apps/LatchDesktop/Sources/LatchDesktop/TitlebarSidebarMaterial.swift, apps/LatchDesktop/Sources/LatchDesktop/Updater.swift, apps/LatchDesktop/Sources/LatchDesktop/Views.swift, apps/LatchDesktop/Sources/LatchDesktop/WindowAttachment.swift, apps/LatchDesktop/Tests/LatchDesktopTests/ControlPlaneHostTests.swift, apps/LatchDesktop/Tests/LatchDesktopTests/HelperLineReaderTests.swift, apps/LatchDesktop/Tests/LatchDesktopTests/ModelTests.swift, apps/LatchDesktop/Tests/LatchDesktopTests/RemoteAccessTests.swift, apps/LatchDesktop/Tests/LatchDesktopTests/UpdaterTests.swift, apps/LatchDesktop/build-app.sh, apps/LatchMobile/App/LatchMobile/ChatView.swift, apps/LatchMobile/App/LatchMobile/Conversation/ConversationActivityDisclosure.swift, apps/LatchMobile/App/LatchMobile/Conversation/ConversationComposer.swift, apps/LatchMobile/App/LatchMobile/Conversation/ConversationMarkdownView.swift, apps/LatchMobile/App/LatchMobile/Conversation/ConversationMessageRow.swift, apps/LatchMobile/App/LatchMobile/Conversation/ConversationNotices.swift, apps/LatchMobile/App/LatchMobile/Conversation/ConversationPreviews.swift, apps/LatchMobile/App/LatchMobile/Conversation/ConversationRequestCard.swift, apps/LatchMobile/App/LatchMobile/Conversation/ConversationScreen.swift, apps/LatchMobile/App/LatchMobile/Conversation/ConversationToolbar.swift, apps/LatchMobile/App/LatchMobile/Conversation/ConversationTranscript.swift, apps/LatchMobile/App/LatchMobile/FolderPickerView.swift, apps/LatchMobile/App/LatchMobile/LatchMobileApp.swift, apps/LatchMobile/App/LatchMobile/PairingView.swift, apps/LatchMobile/App/LatchMobile/QRScannerView.swift, apps/LatchMobile/App/LatchMobile/SessionTerminalSurface.swift, apps/LatchMobile/App/LatchMobile/SessionsView.swift, apps/LatchMobile/App/LatchMobile/SettingsView.swift, apps/LatchMobile/App/LatchMobile/SwiftTermSurface.swift, apps/LatchMobile/App/LatchMobile/TerminalKeyBar.swift, apps/LatchMobile/App/LatchMobile/TerminalView.swift, apps/LatchMobile/Package.swift, apps/LatchMobile/Sources/LatchMobileKit/AppModel.swift, apps/LatchMobile/Sources/LatchMobileKit/ControlPlane.swift, apps/LatchMobile/Sources/LatchMobileKit/ConversationPresentation/ConversationInputPresentation.swift, apps/LatchMobile/Sources/LatchMobileKit/ConversationPresentation/ConversationMarkdown.swift, apps/LatchMobile/Sources/LatchMobileKit/ConversationPresentation/ConversationPresentation.swift, apps/LatchMobile/Sources/LatchMobileKit/ConversationPresentation/ConversationProjection.swift, apps/LatchMobile/Sources/LatchMobileKit/ConversationPresentation/ConversationTailFollowState.swift, apps/LatchMobile/Sources/LatchMobileKit/ConversationPresentation/ConversationViewState.swift, apps/LatchMobile/Sources/LatchMobileKit/ConversationSocket.swift, apps/LatchMobile/Sources/LatchMobileKit/ConversationStore.swift, apps/LatchMobile/Sources/LatchMobileKit/DeviceIdentity.swift, apps/LatchMobile/Sources/LatchMobileKit/GatewayCompatibility.swift, apps/LatchMobile/Sources/LatchMobileKit/GatewayTransport.swift, apps/LatchMobile/Sources/LatchMobileKit/Generated/LatchContract.swift, apps/LatchMobile/Sources/LatchMobileKit/LatchGateway.swift, apps/LatchMobile/Sources/LatchMobileKit/LinkDiagnostics.swift, apps/LatchMobile/Sources/LatchMobileKit/Models.swift, apps/LatchMobile/Sources/LatchMobileKit/NewSessionFolder.swift, apps/LatchMobile/Sources/LatchMobileKit/PairedDevice.swift, apps/LatchMobile/Sources/LatchMobileKit/PairingModel.swift, apps/LatchMobile/Sources/LatchMobileKit/PairingPayload.swift, apps/LatchMobile/Sources/LatchMobileKit/RemoteLinkCoordinator.swift, apps/LatchMobile/Sources/LatchMobileKit/RemotePathMetrics.swift, apps/LatchMobile/Sources/LatchMobileKit/SessionPresentation.swift, apps/LatchMobile/Sources/LatchMobileKit/Signaling.swift, apps/LatchMobile/Sources/LatchMobileKit/TerminalGeometry.swift, apps/LatchMobile/Sources/LatchMobileKit/TerminalKey.swift, apps/LatchMobile/Sources/LatchMobileKit/TerminalSession.swift, apps/LatchMobile/Sources/LatchMobileKit/TerminalSocket.swift, apps/LatchMobile/Sources/LatchMobileKit/TerminalUnlock.swift, apps/LatchMobile/Sources/LatchTransportNative/NativeRemoteTransport.swift, apps/LatchMobile/Tests/LatchMobileKitTests/AppConfigurationTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/AppModelRecoveryTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/ColdOpenRecorderTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/ContractFreshnessTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/ConversationFixtureTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/ConversationForwardCompatibilityTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/ConversationInputTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/ConversationMarkdownTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/ConversationPresentationContractTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/ConversationProjectionTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/ConversationSocketTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/ConversationStoreBenchmarkTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/ConversationStoreTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/ConversationTailFollowStateTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/ConversationViewStateTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/DeviceIdentityTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/GatewayTransportTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/GatewayV2Tests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/NewSessionAppModelTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/NewSessionFolderTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/RecoveryLifecycleTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/RemoteEnrollmentTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/RemoteLinkCoordinatorTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/RemotePathMetricsTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/SessionConnectorTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/SessionPreviewTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/SessionRouteTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/SessionStopTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/TerminalGatewayTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/TerminalGeometryTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/TerminalKeyTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/TerminalLifecycleTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/TerminalSessionTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/TerminalSocketTests.swift, apps/LatchMobile/Tests/LatchMobileKitTests/TerminalUnlockTests.swift, apps/LatchMobile/Tests/LatchTransportNativeTests/NativeLifecycleTests.swift, apps/LatchMobile/Tests/TerminalEmulatorTests/EmulatorFixtureTests.swift, apps/LatchMobile/Tests/TerminalEmulatorTests/KeyEncodingModeTests.swift, apps/LatchMobile/Tools/generate-contract.py, crates/latch-remote/src/lib.rs, crates/latch-remote/src/main.rs, crates/latch-remote/tests/remote_link_composed.rs, crates/latch-remote/tests/remote_link_recovery.rs, crates/latch-term/src/lib.rs, crates/latch-term/src/model.rs, crates/latch-term/src/modes.rs, crates/latch-term/src/terminal.rs, crates/latch-term/tests/attach_history.rs, crates/latch-term/tests/scrollback.rs, crates/latch-term/tests/snapshot_roundtrip.rs, crates/latch-term/tests/support/mod.rs, crates/latch-term/tests/vt_fidelity.rs, crates/latch-transport-bindgen/src/main.rs, crates/latch-transport-ffi/src/lib.rs, crates/latch-transport/src/lib.rs, crates/latch-transport/src/link.rs, crates/latch/src/cli/attach.rs, crates/latch/src/cli/create.rs, crates/latch/src/cli/json.rs, crates/latch/src/cli/manage.rs, crates/latch/src/cli/mod.rs, crates/latch/src/cli/nesting.rs, crates/latch/src/cli/open.rs, crates/latch/src/cli/remote_access.rs, crates/latch/src/cli/serve/attention.rs, crates/latch/src/cli/serve/contract.rs, crates/latch/src/cli/serve/conversation.rs, crates/latch/src/cli/serve/directory.rs, crates/latch/src/cli/serve/http.rs, crates/latch/src/cli/serve/pty.rs, crates/latch/src/cli/serve/terminal.rs, crates/latch/src/cli/term.rs, crates/latch/src/conversation/cache.rs, crates/latch/src/conversation/connector.rs, crates/latch/src/conversation/connectors.rs, crates/latch/src/conversation/connectors/jsonl.rs, crates/latch/src/conversation/connectors/jsonl/geometry_tests.rs, crates/latch/src/conversation/hub.rs, crates/latch/src/conversation/mod.rs, crates/latch/src/conversation/model.rs, crates/latch/src/conversation/pending.rs, crates/latch/src/conversation/projection.rs, crates/latch/src/engine.rs, crates/latch/src/engine/latchd_kernel.rs, crates/latch/src/lib.rs, crates/latch/src/main.rs, crates/latch/src/observer.rs, crates/latch/src/session/manifest.rs, crates/latch/src/session/meta.rs, crates/latch/src/session/mod.rs, crates/latch/src/session/paths.rs, crates/latch/src/session/timing.rs, crates/latch/src/session/viewer.rs, crates/latch/tests/latchd_kernel_e2e.rs, crates/latchd/tests/security.rs, crates/latchd/tests/session.rs, packages/client/src/client.test.ts, packages/client/src/client.ts, packages/client/src/contract.test.ts, packages/client/src/generated.ts, packages/client/src/index.ts, packages/client/src/reconnect.test.ts, packages/client/src/reconnect.ts, packages/client/src/terminal.test.ts, packages/client/src/terminal.ts, packages/client/src/types.ts, packages/client/src/ws.ts, packages/terminal-react/src/LatchTerminal.tsx, packages/terminal-react/src/css.d.ts, packages/terminal-react/src/index.ts, packages/terminal-react/src/types.ts, packages/terminal-react/src/xterm.ts, scripts/build-latch-transport-xcframework.sh, scripts/bump-minor-version.sh, scripts/capture-vt.py, scripts/check-boundaries.sh, scripts/check-remote-link-contract.sh, scripts/diagnostics_summary.py, scripts/field-run.sh, scripts/field_run_delta.py, scripts/generate-remote-access-types.py, scripts/phone-diagnostics.sh, services/control-plane/src/api.test.ts, services/control-plane/src/config.test.ts, services/control-plane/src/credentials.test.ts, services/control-plane/src/migrate.test.ts, services/control-plane/src/privacy.test.ts, services/control-plane/src/push.test.ts, services/control-plane/src/store/postgres.test.ts, services/control-plane/src/test-harness.ts. Surfaces: transport, native-clients.

## Phase 3 follow-up: Q4, Q6, Q8 (coo:1001.6474)

This follow-up was an offline source review with local regression fixtures. It does not establish a production attack or claim full-repository coverage.

### Q4 — CLI archive extraction

The updater verified the archive checksum before invoking `tar` or `ditto`, then verified the payload manifest and publisher signature after extraction (`crates/latch/src/cli/update/mod.rs:366-390, 519-560`). The checksum is published beside the archive and does not constrain member paths or types. The updater previously had no application-level check on archive entries before extraction; extractor-specific traversal and symlink behavior was the open question. No working escape was established. The supported release shape is four flat regular files. The updater now lists members before extraction, requires exactly those four names and regular-file types, and checks extracted file types again. Local `../` and symlink archive fixtures are rejected before extraction and leave an outside sentinel unchanged. The updater test module passes (18 tests), including a published-shape ZIP preflight. This closes the Q4 proof gap for those entry types; no claim is made about every malformed archive feature of the platform extractors.

### Q6 — Transcript rendering, Desktop client, and generated bindings

**No injection issue found in the reviewed paths.** `ConversationMarkdown.swift` splits block structure in memory and uses Foundation attributed Markdown only for inline text. It strips image URLs and any link scheme outside `http`, `https`, and `mailto`; raw HTML is presented as text. `ConversationMarkdownView.swift` renders the result through SwiftUI `Text`, while `ConversationMessageRow.swift` renders user messages with plain `Text`. Code blocks display text and copy only on a tap. These paths contain no HTML view, script evaluation, automatic image loading, or command execution. `LatchClient.swift` passes CLI arguments through `Process.arguments` and sends the launch manifest as encoded JSON on stdin, so transcript text cannot become shell syntax at this boundary. The generated `LatchTransportFFI.swift` transports service bytes and typed link values; it has no transcript parser or renderer and does not interpret received bytes as executable UI content. This is a source-backed, scoped no-issue note, not a claim about the remaining deferred UI files. The existing `ConversationMarkdownTests` target covers link, image, and HTML behavior.

### Q8 — Desktop control-plane address validation

**Low, fixed:** `ControlPlaneHost.normalize` accepted non-loopback `http://` URLs and left refusal to App Transport Security. A configured endpoint could therefore send host enrollment credentials over cleartext if ATS exceptions or platform behavior allowed the request. The validator now accepts `http` only for `localhost`, IPv4 127/8, or IPv6 `::1`, while retaining HTTPS for other hosts. `ControlPlaneHostTests` includes accepted loopback and rejected public/LAN cases (7 tests pass). This is a defense-in-depth finding; no credential exposure was observed.
