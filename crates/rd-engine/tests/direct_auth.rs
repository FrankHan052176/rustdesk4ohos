//! Actual loopback TCP tests, with hbb_common framing/crypto as peer fixtures.
//! These are NOT tests against a released RustDesk application binary.
pub use rd_engine::transport;
#[path = "../src/authentication.rs"]
mod authentication;
#[path = "../src/handshake.rs"]
mod handshake;
#[path = "../src/session.rs"]
mod session;

use authentication::*;
use handshake::*;
use hbb_common::{
    message_proto::{
        Hash, IdPk, LoginRequest, LoginResponse, Message, PeerInfo, PublicKey, SignedId, message,
    },
    protobuf::Message as _,
    sodiumoxide::{
        crypto::{box_, secretbox, sign},
        randombytes,
    },
    tcp::{Encrypt, FramedStream},
};
use session::*;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::net::{TcpListener, TcpStream};

const TIMEOUT: Duration = Duration::from_secs(5);
struct Budget {
    failures: usize,
}
impl AttemptPolicy for Budget {
    fn allow(&mut self, _: bool) -> Result<(), &'static str> {
        if self.failures > 6 {
            Err("Please try 1 minute later")
        } else {
            Ok(())
        }
    }
    fn outcome(&mut self, _: bool, accepted: bool) {
        if !accepted {
            self.failures += 1;
        }
    }
}
fn config(password: &[u8]) -> HostAuthConfig {
    HostAuthConfig {
        accepted_targets: vec!["test-target".into()],
        salt: "fixture-salt".into(),
        passwords: Passwords::from_salted(vec![salted_password(password, "fixture-salt")]),
        policy: PrimaryPolicy::PasswordOnly,
        ceiling: Permissions::default(),
        password_permissions: Permissions::default(),
        peer_info: PeerInfo {
            version: "1.4.9".into(),
            platform: "test-fixture".into(),
            ..Default::default()
        },
        approval: Box::new(PendingApproval),
        second_factor: Box::new(NoSecondFactor),
        attempts: Box::new(Budget { failures: 0 }),
    }
}
fn login() -> LoginRequest {
    LoginRequest {
        username: "test-target".into(),
        my_id: "fixture-controller".into(),
        my_name: "test".into(),
        session_id: 9,
        version: "1.4.9".into(),
        ..Default::default()
    }
}
async fn recv(wire: &mut FramedStream) -> Message {
    let bytes = tokio::time::timeout(TIMEOUT, wire.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    Message::parse_from_bytes(&bytes).unwrap()
}
async fn tcp_wire(address: std::net::SocketAddr) -> FramedStream {
    let stream = TcpStream::connect(address).await.unwrap();
    let local = stream.local_addr().unwrap();
    FramedStream::from(stream, local)
}
async fn next_host_event(host: &mut HostSession) -> HostEvent {
    loop {
        match host.recv().await.unwrap() {
            HostEvent::Progress => {}
            event => return event,
        }
    }
}
async fn next_viewer_event(viewer: &mut ViewerSession) -> ViewerEvent {
    loop {
        match viewer.recv().await.unwrap() {
            ViewerEvent::Progress => {}
            event => return event,
        }
    }
}

#[tokio::test]
async fn direct_plain_wrong_then_right_password_and_close_over_tcp() {
    hbb_common::sodiumoxide::init().unwrap();
    let password = randombytes::randombytes(24);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let host_config = config(&password);
    let host = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut host =
            HostSession::accept_direct(stream, HostIdentity::LegacyPlain, host_config, TIMEOUT)
                .await
                .unwrap();
        assert!(matches!(host.security(), Security::LegacyPlain { .. }));
        assert!(matches!(
            next_host_event(&mut host).await,
            HostEvent::LoginRejected(WRONG_PASSWORD)
        ));
        assert_ne!(host.state(), HostAuthState::Authorized);
        assert!(matches!(
            next_host_event(&mut host).await,
            HostEvent::Authorized(_)
        ));
        let mut parts = host.into_authenticated_parts().unwrap();
        assert!(matches!(parts.context.side, AuthenticatedSide::Host));
        assert_eq!(parts.context.claims.controller_id, "fixture-controller");
        assert_eq!(parts.context.permissions, Permissions::default());
        let close = parts.reader.recv().await.unwrap().unwrap();
        assert!(close.misc().has_close_reason());
    });
    let mut viewer =
        ViewerSession::connect_direct(address, ViewerIdentity::LegacyUnverified, TIMEOUT)
            .await
            .unwrap();
    assert!(matches!(
        next_viewer_event(&mut viewer).await,
        ViewerEvent::Challenge
    ));
    viewer
        .login(login(), Some(b"incorrect-fixture-password"))
        .await
        .unwrap();
    assert!(
        matches!(next_viewer_event(&mut viewer).await, ViewerEvent::LoginError(ref e) if e == WRONG_PASSWORD)
    );
    viewer.login(login(), Some(&password)).await.unwrap();
    assert!(matches!(
        next_viewer_event(&mut viewer).await,
        ViewerEvent::Authorized(_)
    ));
    viewer.close().await.unwrap();
    host.await.unwrap();
}

#[tokio::test]
async fn new_viewer_authenticates_against_signed_hbb_tcp_fixture() {
    hbb_common::sodiumoxide::init().unwrap();
    let password = randombytes::randombytes(24);
    let fixture_password = password.clone();
    let (server_pk, server_sk) = sign::gen_keypair();
    let (host_pk, host_sk) = sign::gen_keypair();
    let signed = sign::sign(
        &IdPk {
            id: "stable-host".into(),
            pk: host_pk.0.to_vec().into(),
            ..Default::default()
        }
        .write_to_bytes()
        .unwrap(),
        &server_sk,
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let fixture = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let local = stream.local_addr().unwrap();
        let mut wire = FramedStream::from(stream, local);
        let (box_pk, box_sk) = box_::gen_keypair();
        let data = IdPk {
            id: "stable-host".into(),
            pk: box_pk.0.to_vec().into(),
            ..Default::default()
        }
        .write_to_bytes()
        .unwrap();
        let mut msg = Message::new();
        msg.set_signed_id(SignedId {
            id: sign::sign(&data, &host_sk).into(),
            ..Default::default()
        });
        wire.send(&msg).await.unwrap();
        let reply = recv(&mut wire).await;
        let Some(message::Union::PublicKey(key)) = reply.union else {
            panic!("Expected PublicKey")
        };
        wire.set_key(
            Encrypt::decode(&key.symmetric_value, &key.asymmetric_value, &box_sk).unwrap(),
        );
        let hash = Hash {
            salt: "fixture-salt".into(),
            challenge: "fresh-challenge".into(),
            ..Default::default()
        };
        msg.set_hash(hash.clone());
        wire.send(&msg).await.unwrap();
        let request = recv(&mut wire).await;
        let Some(message::Union::LoginRequest(request)) = request.union else {
            panic!("Expected login")
        };
        // Authentication must not invent a decoder advertisement to get video
        // started. Media-provider integration (and its zero-copy gate) is later.
        assert!(request.option.supported_decoding.is_none());
        // Independent fixture computes the same SHA256 concatenations via libsodium.
        let mut h1_input = fixture_password;
        h1_input.extend_from_slice(hash.salt.as_bytes());
        let h1 = hbb_common::sodiumoxide::crypto::hash::sha256::hash(&h1_input);
        let mut h2_input = h1.0.to_vec();
        h2_input.extend_from_slice(hash.challenge.as_bytes());
        assert_eq!(
            &request.password[..],
            &hbb_common::sodiumoxide::crypto::hash::sha256::hash(&h2_input).0
        );
        let mut response = LoginResponse::new();
        response.set_peer_info(PeerInfo::new());
        msg.set_login_response(response);
        wire.send(&msg).await.unwrap();
        assert!(matches!(
            recv(&mut wire).await.union,
            Some(message::Union::Misc(_))
        ));
    });
    let identity = ViewerIdentity::Rendezvous {
        expected_id: "stable-host".into(),
        signed_id_pk: signed,
        server_key: server_pk,
    };
    let mut viewer = ViewerSession::connect_direct(address, identity, TIMEOUT)
        .await
        .unwrap();
    assert_eq!(
        viewer.security(),
        &Security::Encrypted {
            peer_id: Some("stable-host".into())
        }
    );
    assert_eq!(viewer.permissions(), Permissions::default());
    assert!(matches!(
        next_viewer_event(&mut viewer).await,
        ViewerEvent::Challenge
    ));
    viewer.login(login(), Some(&password)).await.unwrap();
    assert!(matches!(
        next_viewer_event(&mut viewer).await,
        ViewerEvent::Authorized(_)
    ));
    // This original-style host emitted no PermissionInfo at all. Omission is
    // allowed remotely after login, not a permanent keyboard/clipboard deny.
    let expected = Permissions {
        keyboard: true,
        clipboard: true,
        audio: true,
        file: true,
    };
    assert_eq!(viewer.permissions(), expected);
    let mut parts = viewer.into_authenticated_parts().unwrap();
    assert_eq!(parts.context.permissions, expected);
    assert!(parts.context.permission_reports.is_empty());
    let mut misc = hbb_common::message_proto::Misc::new();
    misc.set_close_reason("".into());
    let mut close = Message::new();
    close.set_misc(misc);
    parts.writer.send(&close).await.unwrap();
    fixture.await.unwrap();
}

#[tokio::test]
async fn signed_new_host_accepts_hbb_key_transition_then_rejects_pre_auth_input() {
    hbb_common::sodiumoxide::init().unwrap();
    let (sign_pk, sign_sk) = sign::gen_keypair();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let host = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut host = HostSession::accept_direct(
            stream,
            HostIdentity::Signed {
                id: "host".into(),
                secret_key: sign_sk,
            },
            config(b"fixture-only"),
            TIMEOUT,
        )
        .await
        .unwrap();
        assert!(matches!(
            host.security(),
            Security::Encrypted { peer_id: None }
        ));
        assert!(host.recv().await.is_err());
        assert_eq!(host.state(), HostAuthState::Closed);
    });
    let mut wire = tcp_wire(address).await;
    let msg = recv(&mut wire).await;
    let Some(message::Union::SignedId(signed)) = msg.union else {
        panic!("Expected signed ID")
    };
    let id = IdPk::parse_from_bytes(&sign::verify(&signed.id, &sign_pk).unwrap()).unwrap();
    let their_pk = box_::PublicKey::from_slice(&id.pk).unwrap();
    let (pk, sk) = box_::gen_keypair();
    let key = secretbox::gen_key();
    let mut msg = Message::new();
    msg.set_public_key(PublicKey {
        asymmetric_value: pk.0.to_vec().into(),
        symmetric_value: box_::seal(&key.0, &box_::Nonce([0; 24]), &their_pk, &sk).into(),
        ..Default::default()
    });
    wire.send(&msg).await.unwrap();
    wire.set_key(key);
    assert!(matches!(
        recv(&mut wire).await.union,
        Some(message::Union::Hash(_))
    ));
    msg.set_mouse_event(Default::default());
    wire.send(&msg).await.unwrap();
    assert_eq!(
        recv(&mut wire).await.login_response().error(),
        "Connection not allowed"
    );
    host.await.unwrap();
}

struct Manual(Arc<AtomicBool>);
impl ApprovalProvider for Manual {
    fn check(&mut self, _: &LoginRequest) -> Approval {
        if self.0.load(Ordering::SeqCst) {
            Approval::Approved(Permissions::default())
        } else {
            Approval::Pending
        }
    }
}

#[test]
fn approval_two_factor_trusted_and_role_states_fail_closed() {
    let mut cfg = config(b"fixture");
    cfg.second_factor = Box::new(UnsupportedSecondFactor);
    let mut auth = HostAuthentication::new(cfg).unwrap();
    let mut request = login();
    request.hwid = vec![1, 2, 3].into();
    request.password = password_response(
        &salted_password(b"fixture", &auth.challenge().salt),
        &auth.challenge().challenge,
    )
    .to_vec()
    .into();
    assert!(matches!(
        auth.login(request),
        AuthAction::Error {
            message: TWO_FACTOR_REQUIRED,
            terminal: false
        }
    ));
    assert_eq!(auth.state(), HostAuthState::Await2Fa);
    assert!(matches!(
        auth.second_factor(Default::default()),
        AuthAction::Error {
            message: "2FA verification unsupported",
            terminal: false
        }
    ));
    assert_ne!(auth.state(), HostAuthState::Authorized);
    let approved = Arc::new(AtomicBool::new(false));
    let mut cfg = config(b"fixture");
    cfg.policy = PrimaryPolicy::ClickOnly;
    cfg.approval = Box::new(Manual(approved.clone()));
    let mut auth = HostAuthentication::new(cfg).unwrap();
    assert!(matches!(auth.login(login()), AuthAction::PendingApproval));
    assert_eq!(auth.state(), HostAuthState::AwaitApproval);
    approved.store(true, Ordering::SeqCst);
    assert!(matches!(
        auth.poll_approval(),
        AuthAction::Authorized { .. }
    ));
    let mut auth = HostAuthentication::new(config(b"fixture")).unwrap();
    let mut request = login();
    request.set_file_transfer(Default::default());
    assert!(matches!(
        auth.login(request),
        AuthAction::Error {
            message: "No permission of file transfer",
            terminal: true
        }
    ));
    assert_eq!(auth.state(), HostAuthState::Closed);
}

#[tokio::test]
async fn empty_public_key_requests_registration_refresh_without_fake_encryption() {
    hbb_common::sodiumoxide::init().unwrap();
    let (_, sk) = sign::gen_keypair();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let local = stream.local_addr().unwrap();
        let wire = handshake::host(
            FramedStream::from(stream, local),
            HostIdentity::Signed {
                id: "host".into(),
                secret_key: sk,
            },
            TIMEOUT,
        )
        .await
        .unwrap();
        assert_eq!(
            wire.security(),
            &Security::LegacyPlain {
                registration_key_refresh: true
            }
        );
    });
    let mut wire = tcp_wire(address).await;
    assert!(matches!(
        recv(&mut wire).await.union,
        Some(message::Union::SignedId(_))
    ));
    let mut msg = Message::new();
    msg.set_public_key(PublicKey::new());
    wire.send(&msg).await.unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn encrypted_new_host_authenticates_hbb_viewer_fixture_with_wrong_retry() {
    hbb_common::sodiumoxide::init().unwrap();
    let password = randombytes::randombytes(24);
    let cfg = config(&password);
    let (sign_pk, sign_sk) = sign::gen_keypair();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let host = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut host = HostSession::accept_direct(
            stream,
            HostIdentity::Signed {
                id: "stable-host".into(),
                secret_key: sign_sk,
            },
            cfg,
            TIMEOUT,
        )
        .await
        .unwrap();
        assert!(matches!(
            next_host_event(&mut host).await,
            HostEvent::LoginRejected(WRONG_PASSWORD)
        ));
        assert_ne!(host.state(), HostAuthState::Authorized);
        assert!(matches!(
            next_host_event(&mut host).await,
            HostEvent::Authorized(_)
        ));
        assert_eq!(host.permissions(), Permissions::default());
        // Authentication consumed multiple encrypted records before split.
        // The next peer record must decrypt with the existing receive nonce.
        let mut parts = host.into_authenticated_parts().unwrap();
        let mut probe = Message::new();
        probe.set_test_delay(Default::default());
        parts.writer.send(&probe).await.unwrap();
        assert!(
            parts
                .reader
                .recv()
                .await
                .unwrap()
                .unwrap()
                .misc()
                .has_close_reason()
        );
    });
    let mut wire = tcp_wire(address).await;
    let msg = recv(&mut wire).await;
    let Some(message::Union::SignedId(signed)) = msg.union else {
        panic!("Expected SignedId")
    };
    let id = IdPk::parse_from_bytes(&sign::verify(&signed.id, &sign_pk).unwrap()).unwrap();
    assert_eq!(id.id, "stable-host");
    let their_pk = box_::PublicKey::from_slice(&id.pk).unwrap();
    let (pk, sk) = box_::gen_keypair();
    let key = secretbox::gen_key();
    let mut msg = Message::new();
    msg.set_public_key(PublicKey {
        asymmetric_value: pk.0.to_vec().into(),
        symmetric_value: box_::seal(&key.0, &box_::Nonce([0; 24]), &their_pk, &sk).into(),
        ..Default::default()
    });
    wire.send(&msg).await.unwrap();
    wire.set_key(key);
    let Some(message::Union::Hash(hash)) = recv(&mut wire).await.union else {
        panic!("Expected encrypted Hash")
    };
    let mut request = login();
    request.password = vec![0; 32].into();
    msg.set_login_request(request.clone());
    wire.send(&msg).await.unwrap();
    assert_eq!(
        recv(&mut wire).await.login_response().error(),
        WRONG_PASSWORD
    );
    let mut plain = password;
    plain.extend_from_slice(hash.salt.as_bytes());
    let h1 = hbb_common::sodiumoxide::crypto::hash::sha256::hash(&plain);
    let mut challenge = h1.0.to_vec();
    challenge.extend_from_slice(hash.challenge.as_bytes());
    request.password = hbb_common::sodiumoxide::crypto::hash::sha256::hash(&challenge)
        .0
        .to_vec()
        .into();
    msg.set_login_request(request);
    wire.send(&msg).await.unwrap();
    let response = recv(&mut wire).await;
    assert!(response.login_response().has_peer_info());
    let peer = response.login_response().peer_info();
    assert!(peer.encoding.is_none());
    assert!(peer.displays.is_empty());
    for _ in 0..4 {
        assert!(matches!(
            recv(&mut wire).await.union,
            Some(message::Union::Misc(_))
        ));
    }
    assert!(matches!(
        recv(&mut wire).await.union,
        Some(message::Union::TestDelay(_))
    ));
    let mut close = hbb_common::message_proto::Misc::new();
    close.set_close_reason("".into());
    msg.set_misc(close);
    wire.send(&msg).await.unwrap();
    host.await.unwrap();
}

#[tokio::test]
async fn viewer_rejects_media_instead_of_treating_it_as_login_success() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let fixture = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let local = stream.local_addr().unwrap();
        let mut wire = FramedStream::from(stream, local);
        assert!(recv(&mut wire).await.union.is_none());
        let mut msg = Message::new();
        msg.set_hash(Hash {
            salt: "salt".into(),
            challenge: "challenge".into(),
            ..Default::default()
        });
        wire.send(&msg).await.unwrap();
        assert!(matches!(
            recv(&mut wire).await.union,
            Some(message::Union::LoginRequest(_))
        ));
        msg.set_video_frame(Default::default());
        wire.send(&msg).await.unwrap();
    });
    let mut viewer =
        ViewerSession::connect_direct(address, ViewerIdentity::LegacyUnverified, TIMEOUT)
            .await
            .unwrap();
    assert!(matches!(
        next_viewer_event(&mut viewer).await,
        ViewerEvent::Challenge
    ));
    viewer.login(login(), None).await.unwrap();
    assert!(viewer.recv().await.is_err());
    assert_eq!(viewer.state(), ViewerState::Closed);
    fixture.await.unwrap();
}

#[tokio::test]
async fn original_pre_auth_controls_and_hash_first_compat_survive_active_handoff() {
    use hbb_common::message_proto::{
        MessageBox, Misc, PermissionInfo, TestDelay, permission_info::Permission,
    };
    hbb_common::sodiumoxide::init().unwrap();
    let (rs_pk, rs_sk) = sign::gen_keypair();
    let (host_pk, _) = sign::gen_keypair();
    let signed_id_pk = sign::sign(
        &IdPk {
            id: "stable-host".into(),
            pk: host_pk.0.to_vec().into(),
            ..Default::default()
        }
        .write_to_bytes()
        .unwrap(),
        &rs_sk,
    );
    let password = randombytes::randombytes(24);
    let fixture_password = password.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let fixture = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let local = stream.local_addr().unwrap();
        let mut wire = FramedStream::from(stream, local);
        let hash = Hash {
            salt: "fixture-salt".into(),
            challenge: "only-challenge".into(),
            ..Default::default()
        };
        let mut msg = Message::new();
        msg.set_hash(hash.clone());
        wire.send(&msg).await.unwrap();
        // 1.4.9 non-SignedId branch emits empty M; no fabricated PublicKey/key.
        assert!(recv(&mut wire).await.union.is_none());
        // Order taken from original Connection::start: Hash -> permissions,
        // before any LoginRequest/response. Android may send true keyboard.
        for (permission, enabled) in [
            (Permission::Keyboard, true),
            (Permission::Audio, false),
            (Permission::Keyboard, false),
        ] {
            let mut misc = Misc::new();
            misc.set_permission_info(PermissionInfo {
                permission: permission.into(),
                enabled,
                ..Default::default()
            });
            msg.set_misc(misc);
            wire.send(&msg).await.unwrap();
        }
        let Some(message::Union::LoginRequest(request)) = recv(&mut wire).await.union else {
            panic!("Expected login")
        };
        let mut p1 = fixture_password;
        p1.extend_from_slice(hash.salt.as_bytes());
        let mut p2 = hbb_common::sodiumoxide::crypto::hash::sha256::hash(&p1)
            .0
            .to_vec();
        p2.extend_from_slice(hash.challenge.as_bytes());
        assert_eq!(
            &request.password[..],
            &hbb_common::sodiumoxide::crypto::hash::sha256::hash(&p2).0
        );
        // Original Wayland is_inited_msg occurs inside send_logon_response,
        // before PeerInfo; CM chat also has no authorized guard.
        msg.set_message_box(MessageBox {
            msgtype: "nook-nocancel-hasclose".into(),
            title: "Wayland".into(),
            text: "select screen".into(),
            ..Default::default()
        });
        wire.send(&msg).await.unwrap();
        let mut misc = Misc::new();
        misc.set_chat_message(Default::default());
        msg.set_misc(misc);
        wire.send(&msg).await.unwrap();
        let mut response = LoginResponse::new();
        response.set_peer_info(PeerInfo::new());
        msg.set_login_response(response);
        wire.send(&msg).await.unwrap();
        msg.set_test_delay(TestDelay {
            from_client: false,
            ..Default::default()
        });
        wire.send(&msg).await.unwrap();
        // A post-login opaque payload exceeds the authentication limit. Only
        // Active handoff may lift that limit; this is not a video/codec fixture.
        msg.set_clipboard(hbb_common::message_proto::Clipboard {
            content: vec![7; 96 * 1024].into(),
            ..Default::default()
        });
        wire.send(&msg).await.unwrap();
        assert!(
            matches!(recv(&mut wire).await.union, Some(message::Union::TestDelay(p)) if p.from_client)
        );
    });
    let mut viewer = ViewerSession::connect_direct(
        address,
        ViewerIdentity::Rendezvous {
            expected_id: "stable-host".into(),
            signed_id_pk,
            server_key: rs_pk,
        },
        TIMEOUT,
    )
    .await
    .unwrap();
    assert!(matches!(viewer.security(), Security::LegacyPlain { .. }));
    // Hash is retained once, not dropped, regenerated, or treated as auth proof.
    assert!(matches!(
        next_viewer_event(&mut viewer).await,
        ViewerEvent::Challenge
    ));
    for _ in 0..3 {
        assert!(matches!(
            viewer.recv().await.unwrap(),
            ViewerEvent::PreAuthControl(_)
        ));
        assert_eq!(viewer.state(), ViewerState::AwaitLogin);
        assert_eq!(viewer.permissions(), Permissions::default());
    }
    viewer.login(login(), Some(&password)).await.unwrap();
    for _ in 0..2 {
        assert!(matches!(
            viewer.recv().await.unwrap(),
            ViewerEvent::PreAuthControl(_)
        ));
    }
    assert_ne!(viewer.state(), ViewerState::Active);
    assert!(matches!(
        viewer.recv().await.unwrap(),
        ViewerEvent::Authorized(_)
    ));
    let parts = viewer.into_authenticated_parts().unwrap();
    assert!(matches!(parts.context.side, AuthenticatedSide::Viewer));
    assert!(matches!(
        parts.context.security,
        Security::LegacyPlain { .. }
    ));
    assert_eq!(parts.context.permission_reports.len(), 2);
    // Latest false override wins; omitted clipboard/file retain original remote
    // defaults. The earlier keyboard=true never allowed anything pre-auth.
    assert!(!parts.context.permissions.keyboard);
    assert!(!parts.context.permissions.audio);
    assert!(parts.context.permissions.clipboard);
    assert!(parts.context.permissions.file);
    assert!(parts.context.peer_info.is_some());
    let mut reader = parts.reader;
    let mut writer = parts.writer;
    let mut outgoing = Message::new();
    outgoing.set_test_delay(TestDelay {
        from_client: true,
        ..Default::default()
    });
    // Both halves are independently movable; no ViewerSession writer/echo loop
    // remains. Already-buffered post-login records survive the ownership move.
    let read = tokio::spawn(async move {
        let message = reader.recv().await.unwrap().unwrap();
        let large = reader.recv().await.unwrap().unwrap();
        assert_eq!(large.clipboard().content.len(), 96 * 1024);
        (message, reader)
    });
    let write = tokio::spawn(async move {
        writer.send(&outgoing).await.unwrap();
        writer
    });
    let (incoming, _reader) = read.await.unwrap();
    assert!(matches!(incoming.union, Some(message::Union::TestDelay(p)) if !p.from_client));
    let _writer = write.await.unwrap();
    fixture.await.unwrap();
}

#[tokio::test]
async fn authenticated_parts_cannot_be_taken_before_login() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (host, viewer) = tokio::join!(
        async {
            let (stream, _) = listener.accept().await.unwrap();
            HostSession::accept_direct(
                stream,
                HostIdentity::LegacyPlain,
                config(b"fixture"),
                TIMEOUT,
            )
            .await
            .unwrap()
        },
        ViewerSession::connect_direct(address, ViewerIdentity::LegacyUnverified, TIMEOUT)
    );
    assert!(viewer.unwrap().into_authenticated_parts().is_err());
    assert!(host.into_authenticated_parts().is_err());
}

#[tokio::test]
async fn pre_hash_refusal_is_control_but_unsolicited_success_never_authorizes() {
    for refusal in [true, false] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let fixture = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let local = stream.local_addr().unwrap();
            let mut wire = FramedStream::from(stream, local);
            assert!(recv(&mut wire).await.union.is_none());
            let mut response = LoginResponse::new();
            if refusal {
                response.set_error("Connection not allowed".into());
            } else {
                response.set_peer_info(PeerInfo::new());
            }
            let mut message = Message::new();
            message.set_login_response(response);
            wire.send(&message).await.unwrap();
        });
        let mut viewer =
            ViewerSession::connect_direct(address, ViewerIdentity::LegacyUnverified, TIMEOUT)
                .await
                .unwrap();
        if refusal {
            assert!(
                matches!(viewer.recv().await.unwrap(), ViewerEvent::LoginError(e) if e == "Connection not allowed")
            );
        } else {
            assert!(viewer.recv().await.is_err());
        }
        assert_eq!(viewer.state(), ViewerState::Closed);
        assert!(viewer.into_authenticated_parts().is_err());
        fixture.await.unwrap();
    }
}

#[tokio::test]
async fn authentication_rejects_oversized_record_headers_on_both_roles() {
    use tokio::io::AsyncWriteExt;
    // Length 65537 uses a three-byte original RustDesk header. Sending no body
    // ensures a small-limit decoder rejects immediately instead of buffering
    // until the authentication timeout or accepting the media 64MiB limit.
    let encoded = ((64 * 1024 + 1) << 2) | 2;
    let header = [encoded as u8, (encoded >> 8) as u8, (encoded >> 16) as u8];
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let host = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut host = HostSession::accept_direct(
            stream,
            HostIdentity::LegacyPlain,
            config(b"fixture"),
            TIMEOUT,
        )
        .await
        .unwrap();
        let error = match host.recv().await {
            Err(error) => error,
            _ => panic!("Oversized header accepted"),
        };
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(host.state(), HostAuthState::Closed);
    });
    let mut wire = tcp_wire(address).await;
    assert!(matches!(
        recv(&mut wire).await.union,
        Some(message::Union::Hash(_))
    ));
    wire.0.get_mut().write_all(&header).await.unwrap();
    host.await.unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let fixture = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let local = stream.local_addr().unwrap();
        let mut wire = FramedStream::from(stream, local);
        assert!(recv(&mut wire).await.union.is_none());
        wire.0.get_mut().write_all(&header).await.unwrap();
        // Keep the peer alive until rejection closes it; EOF is not our trigger.
        assert!(
            tokio::time::timeout(TIMEOUT, wire.next())
                .await
                .unwrap()
                .is_none()
        );
    });
    let mut viewer =
        ViewerSession::connect_direct(address, ViewerIdentity::LegacyUnverified, TIMEOUT)
            .await
            .unwrap();
    let error = match viewer.recv().await {
        Err(error) => error,
        _ => panic!("Oversized header accepted"),
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(viewer.state(), ViewerState::Closed);
    fixture.await.unwrap();
}

#[tokio::test]
async fn cancelled_unsplit_authentication_write_is_terminal() {
    let (a, b) = tokio::io::duplex(128);
    let address = "127.0.0.1:1".parse().unwrap();
    let established = handshake::viewer(
        FramedStream::from(a, address),
        ViewerIdentity::LegacyUnverified,
        TIMEOUT,
    )
    .await
    .unwrap();
    let mut fixture = FramedStream::from(b, address);
    assert!(recv(&mut fixture).await.union.is_none());
    let mut msg = Message::new();
    msg.set_hash(Hash {
        salt: "fixture-salt".into(),
        challenge: "challenge".into(),
        ..Default::default()
    });
    fixture.send(&msg).await.unwrap();
    let mut viewer = ViewerSession::from_established(established, TIMEOUT, TIMEOUT);
    assert!(matches!(
        viewer.recv().await.unwrap(),
        ViewerEvent::Challenge
    ));
    let mut request = login();
    request.my_name = "x".repeat(8192);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), viewer.login(request, None))
            .await
            .is_err()
    );
    assert!(viewer.login(login(), None).await.is_err());
    assert_eq!(viewer.state(), ViewerState::Closed);
    assert!(viewer.into_authenticated_parts().is_err());
}
