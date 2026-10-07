use super::super::framing::{authorize_and_inject, request_framing, MAX_INITIAL_REQUEST};
use super::super::gateway::{proxy_authenticated_stream, AuthenticatedGateway, GatewayOwnerLock};
use super::*;
use crate::cli::serve::routes::{DEVICE_GRANT_HEADER, DEVICE_ID_HEADER};
use std::sync::{atomic::AtomicUsize, Arc};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn enrolled_home(permission: DevicePermission) -> (tempfile::TempDir, LatchHome, String) {
    let directory = tempfile::tempdir().unwrap();
    let home = LatchHome::new(directory.path());
    set_enabled(&home, true).unwrap();
    let secret = StaticSecret::from([7_u8; 32]);
    let key = hex_encode(PublicKey::from(&secret).as_bytes());
    authorize_enrollment(
        &home,
        &format!("enr_{}", "1".repeat(32)),
        &key,
        "Test phone",
        permission,
        &format!("dev_{}", "2".repeat(32)),
    )
    .unwrap();
    (directory, home, key)
}

#[test]
fn a_re_enrolled_key_resolves_to_its_active_record_not_an_older_revoked_one() {
    // A phone keeps its identity key across re-pairing. The first
    // pairing after the transport replacement failed exactly here: the
    // retired-protocol record for the same key had been revoked and the
    // lookup returned it, so the helper refused a controller the owner
    // had just approved.
    let (_directory, home, key) = enrolled_home(DevicePermission::Control);
    let paths = Paths::new(&home);
    let first = list_devices(&home).unwrap()[0].device_id.clone();
    revoke(&home, &first).unwrap();
    assert!(matches!(
        current_device(&paths, &key, initial_grant_revision()),
        Err(error) if error.to_string() == "revoked controller"
    ));

    let second = authorize_enrollment(
        &home,
        &format!("enr_{}", "3".repeat(32)),
        &key,
        "Test phone again",
        DevicePermission::Interact,
        &format!("dev_{}", "4".repeat(32)),
    )
    .unwrap();
    let current = current_device(&paths, &key, initial_grant_revision()).unwrap();
    assert_eq!(current.device_id, second.device_id);
    assert_eq!(current.permission, DevicePermission::Interact);
    assert!(!current.revoked);

    // Revoking the active record leaves only revoked history for the key,
    // and that must still read as revoked, never as unpaired.
    revoke(&home, &second.device_id).unwrap();
    assert!(matches!(
        current_device(&paths, &key, initial_grant_revision()),
        Err(error) if error.to_string() == "revoked controller"
    ));
}

#[test]
fn fixed_gateway_proxy_rejects_forged_authority_paths_and_pipelining() {
    let good = authorize_and_inject(
        b"GET /v2/sessions HTTP/1.1\r\nHost: latch\r\n\r\n".to_vec(),
        DevicePermission::Observe,
        "phone-local-id",
        "internal-token",
    )
    .unwrap()
    .0;
    let good = String::from_utf8(good).unwrap();
    assert!(good.contains("Authorization: Bearer internal-token"));
    assert!(good
        .to_ascii_lowercase()
        .contains("x-latch-device-grant: observe"));
    assert!(good
        .to_ascii_lowercase()
        .contains("x-latch-device-id: phone-local-id"));

    for request in [
        "GET /v2/sessions HTTP/1.1\r\nAuthorization: Bearer forged\r\n\r\n",
        "GET /v2/sessions HTTP/1.1\r\nX-Latch-Device-Grant: control\r\n\r\n",
        "GET /v2/sessions HTTP/1.1\r\nX-Latch-Device-Id: someone-else\r\n\r\n",
        "GET /v2/../secret HTTP/1.1\r\nHost: latch\r\n\r\n",
        "GET /v2/not-a-route HTTP/1.1\r\nHost: latch\r\n\r\n",
        "GET /v2/sessions HTTP/1.1\r\nHost: latch\r\n\r\nGET /v2/sessions HTTP/1.1\r\n\r\n",
    ] {
        assert!(authorize_and_inject(
            request.as_bytes().to_vec(),
            DevicePermission::Control,
            "phone-local-id",
            "internal-token",
        )
        .is_err());
    }

    assert!(authorize_and_inject(
        b"POST /v2/sessions HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}".to_vec(),
        DevicePermission::Observe,
        "phone-local-id",
        "internal-token",
    )
    .is_err());

    // Stopping a session travels the same table as every other route: an
    // interacting phone is refused it, and a controlling one reaches it
    // with the grant stamped on by the proxy rather than by the phone.
    assert!(authorize_and_inject(
        b"POST /v2/sessions/ses_1/stop HTTP/1.1\r\nHost: latch\r\n\r\n".to_vec(),
        DevicePermission::Interact,
        "phone-local-id",
        "internal-token",
    )
    .is_err());
    let (stop, required) = authorize_and_inject(
        b"POST /v2/sessions/ses_1/stop HTTP/1.1\r\nHost: latch\r\n\r\n".to_vec(),
        DevicePermission::Control,
        "phone-local-id",
        "internal-token",
    )
    .unwrap();
    assert_eq!(required, DevicePermission::Control);
    assert!(String::from_utf8(stop)
        .unwrap()
        .to_ascii_lowercase()
        .contains("x-latch-device-grant: control"));
}

/// The attachments route is the one route whose body is not buffered.
/// Its headers still pass through every check above, it is still gated by
/// the route table's grant, and its body is bounded by what it declares.
#[test]
fn streamed_attachment_requests_are_authorized_from_their_headers() {
    let upload = |length: &str, body: &[u8]| {
        let mut request = format!(
            "POST /v2/sessions/ses_1/attachments?name=a.png HTTP/1.1\r\nHost: latch\r\n{length}\r\n"
        )
        .into_bytes();
        request.extend_from_slice(body);
        request
    };
    // A large declared body is framed as headers-only, not refused under
    // the 32 KiB whole-request bound.
    let framing = request_framing(&upload("Content-Length: 1048576\r\n", b"")).unwrap();
    assert!(framing.streamed);
    assert_eq!(framing.buffered + 1_048_576, framing.total);

    let (injected, required) = authorize_and_inject(
        upload("Content-Length: 1048576\r\n", b"first-bytes"),
        DevicePermission::Interact,
        "phone-local-id",
        "internal-token",
    )
    .unwrap();
    assert_eq!(required, DevicePermission::Interact);
    assert!(injected.ends_with(b"\r\n\r\nfirst-bytes"));

    // An observing phone may not place files, whatever it declares.
    assert!(authorize_and_inject(
        upload("Content-Length: 1\r\n", b"x"),
        DevicePermission::Observe,
        "phone-local-id",
        "internal-token",
    )
    .is_err());
    // No length, a length over the route's limit, or bytes past the
    // declared body are all refused before the gateway sees anything.
    let over = format!(
        "Content-Length: {}\r\n",
        crate::cli::serve::routes::ATTACHMENT_MAX_BYTES + 1
    );
    for request in [
        upload("", b""),
        upload(&over, b""),
        upload(
            "Content-Length: 2\r\n",
            b"abcGET /v2/sessions HTTP/1.1\r\n\r\n",
        ),
    ] {
        assert!(authorize_and_inject(
            request,
            DevicePermission::Control,
            "phone-local-id",
            "internal-token",
        )
        .is_err());
    }
    // Every other route keeps the whole-request bound.
    let big = format!(
        "POST /v2/sessions HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
        MAX_INITIAL_REQUEST
    );
    assert!(request_framing(big.as_bytes()).is_err());
}

/// End to end through the proxy: the gateway receives the declared body
/// in full and not one byte after it.
#[tokio::test]
async fn a_streamed_body_is_relayed_exactly_and_nothing_follows_it() {
    let (_directory, home, key) = enrolled_home(DevicePermission::Interact);
    let gateway = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = gateway.local_addr().unwrap();
    let body = vec![b'z'; 200_000];
    let expected = body.clone();
    let gateway_seen = tokio::spawn(async move {
        let (mut stream, _) = gateway.accept().await.unwrap();
        let mut received = Vec::new();
        stream.read_to_end(&mut received).await.unwrap();
        let split = received
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        let headers = String::from_utf8_lossy(&received[..split]).to_ascii_lowercase();
        assert!(headers.contains("x-latch-device-grant: interact"));
        assert_eq!(&received[split + 4..], expected.as_slice());
    });
    let (mut client, server) = tokio::io::duplex(64 * 1024);
    let proxy = tokio::spawn(async move {
        proxy_authenticated_stream(
            server,
            &Paths::new(&home),
            &key,
            1,
            "internal-token",
            address,
            &Arc::new(AtomicUsize::new(0)),
        )
        .await
    });
    let mut request = format!(
        "POST /v2/sessions/ses_1/attachments?name=z.bin HTTP/1.1\r\nHost: latch\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    request.extend_from_slice(&body);
    // A second request smuggled after the body must never arrive.
    request.extend_from_slice(b"POST /v2/sessions/ses_1/stop HTTP/1.1\r\n\r\n");
    client.write_all(&request).await.unwrap();
    client.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), gateway_seen)
        .await
        .expect("gateway never saw the body end")
        .unwrap();
    drop(client);
    let _ = tokio::time::timeout(Duration::from_secs(2), proxy).await;
}

#[test]
fn proxy_rejects_smuggled_and_ambiguous_headers() {
    for request in [
        b"GET /v2/sessions HTTP/1.1\r\nX-Foo: a\nX-Latch-Device-Grant: control\r\n\r\n".as_slice(),
        b"GET /v2/sessions HTTP/1.1\r\nX-Foo: a\rX-Latch-Device-Grant: control\r\n\r\n",
        b"GET /v2/sessions HTTP/1.1\r\nx-LaTcH-DeViCe-GrAnT: control\r\n\r\n",
        b"GET /v2/sessions HTTP/1.1\r\n X-Latch-Device-Grant: control\r\n\r\n",
        b"GET /v2/sessions HTTP/1.1\r\nX-Foo: ok\r\n\tX-Latch-Device-Grant: control\r\n\r\n",
    ] {
        assert!(authorize_and_inject(
            request.to_vec(),
            DevicePermission::Observe,
            "phone-local-id",
            "internal-token",
        )
        .is_err());
    }
}

#[test]
fn proxy_rebuilds_websocket_upgrade_with_single_authority_headers() {
    let request = b"GET /v2/sessions/ses_1/conversation HTTP/1.1\r\nHost: gateway\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n";
    let (serialized, _) = authorize_and_inject(
        request.to_vec(),
        DevicePermission::Observe,
        "phone-local-id",
        "internal-token",
    )
    .unwrap();
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut parsed = httparse::Request::new(&mut headers);
    assert_eq!(
        parsed.parse(&serialized).unwrap(),
        httparse::Status::Complete(serialized.len())
    );
    let names = parsed
        .headers
        .iter()
        .map(|header| header.name.to_ascii_lowercase())
        .collect::<Vec<_>>();
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == DEVICE_GRANT_HEADER)
            .count(),
        1
    );
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == DEVICE_ID_HEADER)
            .count(),
        1
    );
    let last_caller = names
        .iter()
        .position(|name| name == "sec-websocket-version")
        .unwrap();
    for trusted in ["authorization", DEVICE_GRANT_HEADER, DEVICE_ID_HEADER] {
        assert!(names.iter().position(|name| name == trusted).unwrap() > last_caller);
    }
    assert_eq!(
        parsed
            .headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(DEVICE_GRANT_HEADER))
            .unwrap()
            .value,
        b"observe"
    );
    assert_eq!(
        parsed
            .headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(DEVICE_ID_HEADER))
            .unwrap()
            .value,
        b"phone-local-id"
    );
    assert_eq!(
        names
            .iter()
            .filter(|name| name.as_str() == "connection")
            .count(),
        1
    );
}

#[test]
fn unreadable_authority_store_fails_closed() {
    let (_directory, home, key) = enrolled_home(DevicePermission::Control);
    let paths = Paths::new(&home);
    fs::write(paths.devices(), b"not-json").unwrap();
    assert!(current_device(&paths, &key, 1).is_err());
}

#[test]
fn shared_gateway_owner_lock_is_exclusive_and_recovers_after_owner_exit() {
    let directory = tempfile::tempdir().unwrap();
    let lock_path = directory.path().join("remote-link-gateway-owner.lock");

    // This is the same advisory lock held by the process that owns the
    // one Conversation Hub child. A competing supervisor must fail while
    // it is live, while a stale pathname after an abnormal process exit
    // must not block a replacement owner.
    let owner = GatewayOwnerLock::acquire(&lock_path).unwrap();
    assert!(GatewayOwnerLock::acquire(&lock_path).is_err());
    drop(owner);
    GatewayOwnerLock::acquire(&lock_path).unwrap();
}

#[tokio::test]
async fn revoking_one_device_does_not_remove_another_devices_shared_gateway_authority() {
    let (_directory, home, first_key) = enrolled_home(DevicePermission::Control);
    let second_secret = StaticSecret::from([8_u8; 32]);
    let second_key = hex_encode(PublicKey::from(&second_secret).as_bytes());
    let second = authorize_enrollment(
        &home,
        &format!("enr_{}", "3".repeat(32)),
        &second_key,
        "Second test phone",
        DevicePermission::Observe,
        &format!("dev_{}", "4".repeat(32)),
    )
    .unwrap();
    let paths = Paths::new(&home);
    write_bytes_atomic(
        &paths.runtime().join("remote-link-gateway-ready.json"),
        b"{\"address\":\"127.0.0.1:1\"}\n",
    )
    .unwrap();

    // Both device helpers can resolve the one shared ready document.
    AuthenticatedGateway::connect(&home, &first_key, 1)
        .await
        .unwrap();
    AuthenticatedGateway::connect(&home, &second_key, 1)
        .await
        .unwrap();

    let first_device_id = list_devices(&home).unwrap()[0].device_id.clone();
    revoke(&home, &first_device_id).unwrap();

    // The revoked helper can no longer acquire an authority handle, but
    // revocation does not disturb the independently authenticated peer.
    assert!(AuthenticatedGateway::connect(&home, &first_key, 1)
        .await
        .is_err());
    AuthenticatedGateway::connect(&home, &second_key, second.grant_revision)
        .await
        .unwrap();
}

#[tokio::test]
async fn live_grant_change_closes_an_active_privileged_stream() {
    let (_directory, home, key) = enrolled_home(DevicePermission::Control);
    let device_id = list_devices(&home).unwrap().remove(0).device_id;
    let gateway = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = gateway.local_addr().unwrap();
    let gateway_seen = tokio::spawn(async move {
        let (mut stream, _) = gateway.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let count = stream.read(&mut request).await.unwrap();
        assert!(String::from_utf8_lossy(&request[..count])
            .to_ascii_lowercase()
            .contains("x-latch-device-grant: control"));
        stream.read_to_end(&mut Vec::new()).await.unwrap();
    });
    let (mut client, server) = tokio::io::duplex(4096);
    let proxy_home = home.clone();
    let proxy_key = key.clone();
    let proxy = tokio::spawn(async move {
        proxy_authenticated_stream(
            server,
            &Paths::new(&proxy_home),
            &proxy_key,
            1,
            "internal-token",
            address,
            &Arc::new(AtomicUsize::new(0)),
        )
        .await
    });
    client
        .write_all(
            b"GET /v2/sessions/test/terminal HTTP/1.1\r\nHost: latch\r\nUpgrade: websocket\r\n\r\n",
        )
        .await
        .unwrap();
    client.flush().await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    grant(&home, &device_id, DevicePermission::Observe).unwrap();
    tokio::time::timeout(Duration::from_secs(2), proxy)
        .await
        .expect("grant enforcement did not close the stream")
        .unwrap()
        .unwrap();
    gateway_seen.await.unwrap();
}

/// The shared fixture is also read by the phone's tests and the contract
/// script, so the three name policies cannot drift apart silently.
const DEVICE_NAMES: &str = include_str!("../../../../../fixtures/remote-link/v1/device-names.json");

#[test]
fn device_name_policy_matches_the_shared_fixture() {
    let fixture: serde_json::Value = serde_json::from_str(DEVICE_NAMES).unwrap();
    let policy = &fixture["policy"];
    assert_eq!(policy["allowedPunctuation"], DEVICE_NAME_PUNCTUATION);
    assert_eq!(policy["maxBytes"], MAX_DEVICE_NAME_BYTES);
    for name in fixture["accepted"].as_array().unwrap() {
        let name = name.as_str().unwrap();
        assert!(
            validate_device_name(name).is_ok(),
            "{name:?} should be accepted"
        );
    }
    for case in fixture["rejected"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        assert!(
            validate_device_name(name).is_err(),
            "{name:?} ({}) should be rejected",
            case["reason"]
        );
    }
}

#[test]
fn enrollment_refuses_a_name_outside_the_allowlist_instead_of_storing_it() {
    let directory = tempfile::tempdir().unwrap();
    let home = LatchHome::new(directory.path());
    set_enabled(&home, true).unwrap();
    let key = hex_encode(PublicKey::from(&StaticSecret::from([7_u8; 32])).as_bytes());
    for name in [
        "Phone\nrequests Observe",
        " Phone",
        "Phone\u{200d}",
        &"a".repeat(81),
    ] {
        assert!(authorize_enrollment(
            &home,
            &format!("enr_{}", "1".repeat(32)),
            &key,
            name,
            DevicePermission::Observe,
            &format!("dev_{}", "2".repeat(32)),
        )
        .is_err());
    }
    assert!(list_devices(&home).unwrap().is_empty());
}
