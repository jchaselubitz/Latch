// The tungstenite handshake callback carries its error response by value.
#![allow(clippy::result_large_err)]
use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use futures::{SinkExt, StreamExt};
use latch::cli::remote_access::{
    authorize_enrollment, proxy_authenticated_stream_for_test, remote_link_identity, set_enabled,
    DevicePermission,
};
use latch::session::paths::LatchHome;
use latch_transport::link::{
    LinkConfig, LinkPurpose, LinkRole, LinkTimings, SecureLink, Service, WssRecordIo,
};
use openssl::{pkcs12::Pkcs12, pkey::PKey, stack::Stack, x509::X509};
use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_native_tls::TlsAcceptor;
use tokio_tungstenite::{accept_hdr_async, tungstenite::Message};
use zeroize::Zeroizing;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|value| format!("{value:02x}")).collect()
}

fn test_certificates(subject: &str) -> (TlsAcceptor, String) {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    // rcgen gives every certificate the same default subject; OpenSSL then
    // sees a leaf whose issuer equals its own subject and treats it as
    // self-signed, so the Linux CI run rejected the chain that
    // Security.framework accepted. Distinct names keep both verifiers honest.
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "Latch composed-test CA");
    let ca = ca_params.self_signed(&ca_key).unwrap();

    let leaf_key = KeyPair::generate().unwrap();
    let mut leaf_params = CertificateParams::new(vec![subject.to_owned()]).unwrap();
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, subject);
    let leaf = leaf_params.signed_by(&leaf_key, &ca, &ca_key).unwrap();
    let leaf_x509 = X509::from_pem(leaf.pem().as_bytes()).unwrap();
    let ca_x509 = X509::from_pem(ca.pem().as_bytes()).unwrap();
    let pkey = PKey::private_key_from_pem(leaf_key.serialize_pem().as_bytes()).unwrap();
    let mut chain = Stack::new().unwrap();
    chain.push(ca_x509).unwrap();
    let mut bundle = Pkcs12::builder();
    bundle
        .name("latch-composed-test")
        .pkey(&pkey)
        .cert(&leaf_x509)
        .ca(chain);
    let der = bundle.build2("latch-test").unwrap().to_der().unwrap();
    let identity = native_tls::Identity::from_pkcs12(&der, "latch-test").unwrap();
    (
        TlsAcceptor::from(native_tls::TlsAcceptor::new(identity).unwrap()),
        ca.pem(),
    )
}

async fn start_wss_relay() -> (String, String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (acceptor, ca) = test_certificates("localhost");
    let admissions = Arc::new(Mutex::new(HashSet::new()));
    let task = tokio::spawn(async move {
        let mut sockets = Vec::new();
        for _ in 0..2 {
            let (tcp, _) = listener.accept().await.unwrap();
            let tls = acceptor.accept(tcp).await.unwrap();
            let admissions = admissions.clone();
            let socket = accept_hdr_async(
                tls,
                move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                      response| {
                    let authorization = request
                        .headers()
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_owned();
                    assert!(
                        authorization == "Bearer host-admission"
                            || authorization == "Bearer controller-admission"
                    );
                    assert!(admissions.lock().unwrap().insert(authorization));
                    Ok(response)
                },
            )
            .await
            .unwrap();
            sockets.push(socket);
        }
        let mut right = sockets.pop().unwrap();
        let mut left = sockets.pop().unwrap();
        left.send(Message::Text(r#"{"type":"lease_started","leaseId":"lease_composed_00000001","expiresAt":4102444800}"#.into())).await.unwrap();
        right.send(Message::Text(r#"{"type":"lease_started","leaseId":"lease_composed_00000002","expiresAt":4102444800}"#.into())).await.unwrap();
        loop {
            tokio::select! {
                message = left.next() => match message {
                    Some(Ok(Message::Binary(bytes))) => right.send(Message::Binary(bytes)).await.unwrap(),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                },
                message = right.next() => match message {
                    Some(Ok(Message::Binary(bytes))) => left.send(Message::Binary(bytes)).await.unwrap(),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(_)) => {}
                },
            }
        }
    });
    (
        format!("wss://localhost:{}/v1/connect", address.port()),
        ca,
        task,
    )
}

#[tokio::test]
async fn native_tls_wss_connects_over_ipv6_loopback() {
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (acceptor, ca) = test_certificates("::1");
    let server = tokio::spawn(async move {
        let (tcp, peer) = listener.accept().await.unwrap();
        assert!(peer.is_ipv6());
        let tls = acceptor.accept(tcp).await.unwrap();
        let mut socket = accept_hdr_async(
            tls,
            |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                assert_eq!(
                    request.headers().get("authorization").unwrap(),
                    "Bearer ipv6-admission"
                );
                Ok(response)
            },
        )
        .await
        .unwrap();
        let _ = socket.next().await;
    });
    let url = format!("wss://[::1]:{}/v1/connect", address.port());
    let (mut records, _) = WssRecordIo::connect_with_test_ca(&url, "ipv6-admission", ca.as_bytes())
        .await
        .unwrap();
    use latch_transport::link::RecordIo;
    records.close().await.unwrap();
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_tls_wss_native_link_reaches_the_authorized_mac_gateway() {
    let directory = tempfile::tempdir().unwrap();
    let home = LatchHome::new(directory.path());
    set_enabled(&home, true).unwrap();
    let controller_keys = snow::Builder::new("Noise_XX_25519_ChaChaPoly_BLAKE2s".parse().unwrap())
        .generate_keypair()
        .unwrap();
    authorize_enrollment(
        &home,
        &format!("enr_{}", "1".repeat(32)),
        &hex(&controller_keys.public),
        "Composed phone",
        DevicePermission::Control,
        &format!("dev_{}", "2".repeat(32)),
    )
    .unwrap();
    let host_identity = remote_link_identity(&home).unwrap();
    let host_private = (0..64)
        .step_by(2)
        .map(|index| u8::from_str_radix(&host_identity.private_key[index..index + 2], 16).unwrap())
        .collect::<Vec<_>>();
    let host_public = (0..64)
        .step_by(2)
        .map(|index| u8::from_str_radix(&host_identity.public_key[index..index + 2], 16).unwrap())
        .collect::<Vec<_>>();

    let gateway = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_address = gateway.local_addr().unwrap();
    let gateway_task = tokio::spawn(async move {
        let (mut stream, _) = gateway.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !request.windows(4).any(|value| value == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).await.unwrap();
            assert_ne!(read, 0);
            request.extend_from_slice(&buffer[..read]);
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with("GET /v2/sessions HTTP/1.1\r\n"));
        assert!(request.contains("\r\nAuthorization: Bearer composed-gateway-token\r\n"));
        assert!(request
            .to_ascii_lowercase()
            .contains("\r\nx-latch-device-grant: control\r\n"));
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\ncomposed-ok",
            )
            .await
            .unwrap();
        stream.shutdown().await.unwrap();
    });

    let (url, ca, relay_task) = start_wss_relay().await;
    let host_records = WssRecordIo::connect_with_test_ca(&url, "host-admission", ca.as_bytes());
    let controller_records =
        WssRecordIo::connect_with_test_ca(&url, "controller-admission", ca.as_bytes());
    let ((host_records, _), (controller_records, _)) =
        tokio::try_join!(host_records, controller_records).unwrap();
    let host = SecureLink::establish(
        host_records,
        LinkConfig {
            purpose: LinkPurpose::Session,
            role: LinkRole::Host,
            local_private_key: Zeroizing::new(host_private),
            local_public_key: host_public.clone(),
            expected_remote_public_key: Some(controller_keys.public.clone()),
            enrollment_id: None,
            enrollment_secret: None,
            grant_revision: 1,
            timings: LinkTimings::default(),
        },
    );
    let controller = SecureLink::establish(
        controller_records,
        LinkConfig {
            purpose: LinkPurpose::Session,
            role: LinkRole::Controller,
            local_private_key: Zeroizing::new(controller_keys.private.clone()),
            local_public_key: controller_keys.public.clone(),
            expected_remote_public_key: Some(host_public),
            enrollment_id: None,
            enrollment_secret: None,
            grant_revision: 1,
            timings: LinkTimings::default(),
        },
    );
    let (host, controller) = tokio::try_join!(host, controller).unwrap();

    let host_task = tokio::spawn({
        let home = home.clone();
        let controller_key = hex(&controller_keys.public);
        let host = host.clone();
        async move {
            let (header, stream) = host.accept(LinkPurpose::Session).await.unwrap();
            assert_eq!(header.service, Service::Gateway);
            assert_eq!(header.grant_revision, 1);
            proxy_authenticated_stream_for_test(
                &home,
                stream,
                &controller_key,
                1,
                "composed-gateway-token",
                gateway_address,
            )
            .await
            .unwrap();
        }
    });
    let mut stream = controller.open(Service::Gateway, 1).await.unwrap();
    stream
        .write_all(b"GET /v2/sessions HTTP/1.1\r\nHost: latch\r\n\r\n")
        .await
        .unwrap();
    stream.flush().await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    assert!(String::from_utf8(response)
        .unwrap()
        .ends_with("composed-ok"));

    host_task.await.unwrap();
    gateway_task.await.unwrap();
    controller.close().await;
    host.close().await;
    relay_task.abort();
    let _ = relay_task.await;
}
