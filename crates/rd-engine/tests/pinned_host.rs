//! One regression boundary: a correctly signed but wrong host must not trigger
//! the original compatibility reply and subsequently leak a password response.
use hbb_common::{
    message_proto::{Hash, IdPk, Message, SignedId},
    protobuf::Message as _,
    sodiumoxide::crypto::{box_, sign},
    tcp::FramedStream,
};
use rd_engine::{handshake::ViewerIdentity, session::ViewerSession};
use std::time::Duration;
use tokio::{io::AsyncReadExt, net::TcpListener};

#[tokio::test]
async fn pinned_mismatch_closes_without_compatibility_reply_or_login() {
    hbb_common::sodiumoxide::init().unwrap();
    let (pin, signer) = sign::gen_keypair();
    let (ephemeral, _) = box_::gen_keypair();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let local = stream.local_addr().unwrap();
        let mut wire = FramedStream::from(stream, local);
        let identity = IdPk {
            id: "different-host".into(),
            pk: ephemeral.0.to_vec().into(),
            ..Default::default()
        };
        let mut signed = Message::new();
        signed.set_signed_id(SignedId {
            id: sign::sign(&identity.write_to_bytes().unwrap(), &signer).into(),
            ..Default::default()
        });
        wire.send(&signed).await.unwrap();
        // Tempt the former fallback path with a password challenge. Write may
        // race the strict viewer's close, so EOF/no outbound bytes is the proof.
        let mut challenge = Message::new();
        challenge.set_hash(Hash {
            salt: "salt".into(),
            challenge: "challenge".into(),
            ..Default::default()
        });
        let _ = wire.send(&challenge).await;
        let mut byte = [0u8; 1];
        let observed =
            tokio::time::timeout(Duration::from_secs(3), wire.0.get_mut().read(&mut byte))
                .await
                .unwrap();
        match observed {
            Ok(0) => {}
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
            _ => panic!("Strict pin emitted outbound data or did not disconnect"),
        }
    });
    let result = ViewerSession::connect_direct(
        address,
        ViewerIdentity::PinnedHost {
            expected_id: "trusted-host".into(),
            peer_signing_key: pin,
        },
        Duration::from_secs(3),
    )
    .await;
    assert!(
        result.is_err(),
        "Mismatched pin must never produce a login-capable session"
    );
    peer.await.unwrap();
}
