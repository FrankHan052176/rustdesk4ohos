//! Two source-order TCP fixtures: hbbs→direct encrypted handoff and
//! hbbs→relay fallback with UUID pairing followed by a rejected peer signature.
use hbb_common::{
    AddrMangle,
    message_proto::{
        Hash, IdPk, LoginRequest, LoginResponse, Message, Misc, PeerInfo, SignedId, message,
    },
    protobuf::Message as _,
    rendezvous_proto::{PunchHoleResponse, RelayResponse, RendezvousMessage, rendezvous_message},
    sodiumoxide::{
        self,
        crypto::{box_, sign},
    },
    tcp::{Encrypt, FramedStream},
    uuid::Uuid,
};
use rd_engine::{
    rendezvous::{RendezvousConfig, RouteKind, connect_viewer},
    session::{ViewerEvent, ViewerSession},
};
use std::time::Duration;
use tokio::{
    io::AsyncReadExt,
    net::{TcpListener, TcpStream},
};
const LIMIT: Duration = Duration::from_secs(8);

fn wire(stream: TcpStream) -> FramedStream {
    let local = stream.local_addr().unwrap();
    FramedStream::from(stream, local)
}
async fn control(wire: &mut FramedStream) -> RendezvousMessage {
    RendezvousMessage::parse_from_bytes(
        &tokio::time::timeout(LIMIT, wire.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}
async fn peer_message(wire: &mut FramedStream) -> Message {
    Message::parse_from_bytes(
        &tokio::time::timeout(LIMIT, wire.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    )
    .unwrap()
}
fn certificate(public: &[u8], signer: &sign::SecretKey) -> Vec<u8> {
    sign::sign(
        &IdPk {
            id: "target".into(),
            pk: public.to_vec().into(),
            ..Default::default()
        }
        .write_to_bytes()
        .unwrap(),
        signer,
    )
}

#[tokio::test]
async fn direct_punch_reuses_source_port_and_preserves_encrypted_auth_handoff() {
    sodiumoxide::init().unwrap();
    let (server_pk, server_sk) = sign::gen_keypair();
    let (peer_pk, peer_sk) = sign::gen_keypair();
    let hbbs = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_address = hbbs.local_addr().unwrap();
    let peer = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer_address = peer.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, origin) = hbbs.accept().await.unwrap();
        let mut wire = wire(stream);
        let Some(rendezvous_message::Union::PunchHoleRequest(request)) =
            control(&mut wire).await.union
        else {
            panic!("Expected PunchHoleRequest")
        };
        assert_eq!(request.id, "target");
        assert_eq!(request.licence_key, "wire-key");
        assert!(!request.force_relay && request.udp_port == 0);
        let mut response = RendezvousMessage::new();
        response.set_key_exchange(Default::default());
        wire.send(&response).await.unwrap();
        response.set_punch_hole_response(PunchHoleResponse {
            socket_addr: AddrMangle::encode(peer_address).into(),
            pk: certificate(&peer_pk.0, &server_sk).into(),
            ..Default::default()
        });
        wire.send(&response).await.unwrap();
        origin.port()
    });
    let peer = tokio::spawn(async move {
        let (stream, origin) = peer.accept().await.unwrap();
        let mut wire = wire(stream);
        let (ephemeral_pk, ephemeral_sk) = box_::gen_keypair();
        let mut response = Message::new();
        response.set_signed_id(SignedId {
            id: certificate(&ephemeral_pk.0, &peer_sk).into(),
            ..Default::default()
        });
        wire.send(&response).await.unwrap();
        let Some(message::Union::PublicKey(key)) = peer_message(&mut wire).await.union else {
            panic!("Expected PublicKey")
        };
        wire.set_key(
            Encrypt::decode(&key.symmetric_value, &key.asymmetric_value, &ephemeral_sk).unwrap(),
        );
        response.set_hash(Hash {
            salt: "salt".into(),
            challenge: "challenge".into(),
            ..Default::default()
        });
        wire.send(&response).await.unwrap();
        let Some(message::Union::LoginRequest(request)) = peer_message(&mut wire).await.union
        else {
            panic!("Expected encrypted LoginRequest")
        };
        assert_eq!(request.username, "target");
        let mut success = LoginResponse::new();
        success.set_peer_info(PeerInfo::new());
        response.set_login_response(success);
        wire.send(&response).await.unwrap();
        response.set_test_delay(Default::default());
        wire.send(&response).await.unwrap();
        assert!(peer_message(&mut wire).await.misc().has_close_reason());
        origin.port()
    });
    let (established, route) = connect_viewer(RendezvousConfig {
        id: "target".into(),
        rendezvous_server: format!("localhost:{}", server_address.port()),
        licence_key: "wire-key".into(),
        server_key: server_pk,
        relay_server: None,
        connect_timeout: LIMIT,
    })
    .await
    .unwrap();
    assert_eq!(route.route, RouteKind::TcpHolePunch);
    assert!(route.encrypted && route.peer_verified);
    let mut session = ViewerSession::from_established(established, LIMIT, LIMIT);
    assert!(matches!(
        session.recv().await.unwrap(),
        ViewerEvent::Challenge
    ));
    session
        .login(
            LoginRequest {
                username: "target".into(),
                my_id: "controller".into(),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    assert!(matches!(
        session.recv().await.unwrap(),
        ViewerEvent::Authorized(_)
    ));
    let mut parts = session.into_authenticated_parts().unwrap();
    assert!(matches!(
        parts.reader.recv().await.unwrap().unwrap().union,
        Some(message::Union::TestDelay(_))
    ));
    let mut misc = Misc::new();
    misc.set_close_reason(String::new());
    let mut close = Message::new();
    close.set_misc(misc);
    parts.writer.send(&close).await.unwrap();
    assert_eq!(server.await.unwrap(), peer.await.unwrap());
}

#[tokio::test]
async fn relay_fallback_pairs_original_uuid_but_rejects_substituted_peer_key() {
    sodiumoxide::init().unwrap();
    let (server_pk, server_sk) = sign::gen_keypair();
    let (peer_pk, _) = sign::gen_keypair();
    let (_, attacker_sk) = sign::gen_keypair();
    let hbbs = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_address = hbbs.local_addr().unwrap();
    let hbbr = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let relay_address = hbbr.local_addr().unwrap();
    // Reserve a non-listening port; do not race ephemeral-port reuse after drop.
    let unavailable = tokio::net::TcpSocket::new_v4().unwrap();
    unavailable.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let unavailable_address = unavailable.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = hbbs.accept().await.unwrap();
        let mut first = wire(stream);
        let Some(rendezvous_message::Union::PunchHoleRequest(request)) =
            control(&mut first).await.union
        else {
            panic!("Expected PunchHoleRequest")
        };
        assert_eq!(request.licence_key, "wire-key");
        let mut response = RendezvousMessage::new();
        response.set_punch_hole_response(PunchHoleResponse {
            socket_addr: AddrMangle::encode(unavailable_address).into(),
            pk: certificate(&peer_pk.0, &server_sk).into(),
            relay_server: format!("localhost:{}", relay_address.port()),
            ..Default::default()
        });
        first.send(&response).await.unwrap();
        drop(first);
        let (stream, _) = hbbs.accept().await.unwrap();
        let mut next = wire(stream);
        let Some(rendezvous_message::Union::RequestRelay(request)) = control(&mut next).await.union
        else {
            panic!("Expected hbbs RequestRelay")
        };
        assert!(request.secure);
        assert_eq!(request.id, "target");
        assert_eq!(
            request.relay_server,
            format!("localhost:{}", relay_address.port())
        );
        assert!(Uuid::parse_str(&request.uuid).is_ok());
        assert!(request.licence_key.is_empty());
        // Original ACK is allowed to omit uuid/pk; pairing uses our sent UUID.
        response.set_relay_response(RelayResponse::new());
        next.send(&response).await.unwrap();
        request.uuid
    });
    let relay = tokio::spawn(async move {
        let (stream, _) = hbbr.accept().await.unwrap();
        let mut wire = wire(stream);
        let Some(rendezvous_message::Union::RequestRelay(request)) = control(&mut wire).await.union
        else {
            panic!("Expected hbbr RequestRelay")
        };
        assert_eq!(request.id, "target");
        assert_eq!(request.licence_key, "wire-key");
        assert!(!request.secure); // The hbbs secure flag is not hbbr's join field.
        let (ephemeral, _) = box_::gen_keypair();
        let mut signed = Message::new();
        signed.set_signed_id(SignedId {
            id: certificate(&ephemeral.0, &attacker_sk).into(),
            ..Default::default()
        });
        wire.send(&signed).await.unwrap();
        let mut byte = [0];
        let result = tokio::time::timeout(LIMIT, wire.0.get_mut().read(&mut byte))
            .await
            .unwrap();
        assert!(
            matches!(result, Ok(0))
                || result.is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionReset),
            "Must not emit a fallback or key to an unverified relay peer"
        );
        request.uuid
    });
    assert!(
        connect_viewer(RendezvousConfig {
            id: "target".into(),
            rendezvous_server: format!("localhost:{}", server_address.port()),
            licence_key: "wire-key".into(),
            server_key: server_pk,
            relay_server: None,
            connect_timeout: LIMIT,
        })
        .await
        .is_err()
    );
    assert_eq!(server.await.unwrap(), relay.await.unwrap());
}
