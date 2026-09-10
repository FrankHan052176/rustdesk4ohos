//! Peer handshake only. No legacy runtime/configuration, no socket-address identity.
use hbb_common::{
    message_proto::{IdPk, Message, PublicKey, SignedId, message},
    protobuf::Message as _,
    sodiumoxide::{
        self,
        crypto::{box_, secretbox, sign},
    },
    tcp::{Encrypt, FramedStream},
};
use std::{io, time::Duration};

/// Evidence is supplied by the dialer; an IP address is never a stable peer ID.
pub enum ViewerIdentity {
    /// The original direct/missing-rendezvous-key compatibility branch.
    LegacyUnverified,
    /// Already trusted OUT OF BAND: peer Ed25519 signing key, never an hbbs
    /// server key or a key learned from this connection. No plaintext fallback.
    PinnedHost {
        expected_id: String,
        peer_signing_key: sign::PublicKey,
    },
    /// A rendezvous-signed serialized IdPk, NOT a detached signature.
    Rendezvous {
        expected_id: String,
        signed_id_pk: Vec<u8>,
        server_key: sign::PublicKey,
    },
}

pub enum HostIdentity {
    LegacyPlain,
    Signed {
        id: String,
        secret_key: sign::SecretKey,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Security {
    Encrypted { peer_id: Option<String> },
    LegacyPlain { registration_key_refresh: bool },
}

/// Constructible only after the key decision. The unsplit stream and buffered
/// bytes/counters move together; a timed-out/cancelled handshake drops the socket.
/// This is NOT user authorization. Keep the small record limit until login.
pub struct Established {
    wire: FramedStream,
    security: Security,
    /// A non-SignedId first record consumed while choosing legacy compatibility.
    /// The original 1.4.9 viewer discards it; retain it so a sole Hash cannot be
    /// lost. This message still goes through normal pre-auth validation.
    prefetched: Option<Message>,
}

impl Established {
    pub fn security(&self) -> &Security {
        &self.security
    }
    pub(crate) fn into_unsplit(self) -> (FramedStream, Security, Option<Message>) {
        (self.wire, self.security, self.prefetched)
    }
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn init() -> io::Result<()> {
    sodiumoxide::init().map_err(|_| io::Error::other("Crypto initialization failed"))
}

pub(crate) async fn send(wire: &mut FramedStream, message: &Message) -> io::Result<()> {
    wire.send(message)
        .await
        .map_err(|_| io::Error::other("Peer handshake write failed"))
}

async fn recv(wire: &mut FramedStream) -> io::Result<Message> {
    let bytes = wire.next().await.ok_or_else(|| {
        io::Error::new(io::ErrorKind::UnexpectedEof, "Peer closed during handshake")
    })??;
    Message::parse_from_bytes(&bytes).map_err(|_| invalid("Invalid handshake message"))
}

fn id_pk(signed: &[u8], key: &sign::PublicKey) -> io::Result<IdPk> {
    let data = sign::verify(signed, key).map_err(|_| invalid("Signature mismatch"))?;
    let value = IdPk::parse_from_bytes(&data).map_err(|_| invalid("Invalid signed identity"))?;
    if value.pk.len() != 32 {
        return Err(invalid("Invalid identity public key length"));
    }
    Ok(value)
}

pub(crate) const AUTH_MAX_RECORD_BYTES: usize = 64 * 1024;

fn prepare(wire: &mut FramedStream) -> io::Result<()> {
    // Calling handshake on an already encrypted or raw stream is a caller error.
    if wire.2.is_some() {
        return Err(invalid("Key transition already completed"));
    }
    wire.0
        .codec_mut()
        .set_max_packet_length(AUTH_MAX_RECORD_BYTES);
    Ok(())
}

pub async fn viewer(
    mut wire: FramedStream,
    identity: ViewerIdentity,
    deadline: Duration,
) -> io::Result<Established> {
    init()?;
    prepare(&mut wire)?;
    tokio::time::timeout(deadline, async move {
        let strict = matches!(&identity, ViewerIdentity::PinnedHost { .. });
        let verified = match identity {
            ViewerIdentity::LegacyUnverified => None,
            ViewerIdentity::PinnedHost {
                expected_id,
                peer_signing_key,
            } => {
                if expected_id.is_empty() || expected_id.len() > 512 {
                    return Err(invalid("Invalid pinned identity"));
                }
                Some((expected_id, peer_signing_key))
            }
            ViewerIdentity::Rendezvous {
                expected_id,
                signed_id_pk,
                server_key,
            } => {
                // This compatibility fallback matches the original viewer; it
                // MUST NOT be reported as a verified/encrypted peer.
                id_pk(&signed_id_pk, &server_key)
                    .ok()
                    .filter(|v| v.id == expected_id)
                    .and_then(|v| sign::PublicKey::from_slice(&v.pk).map(|pk| (expected_id, pk)))
            }
        };
        let Some((expected_id, sign_key)) = verified else {
            send(&mut wire, &Message::new()).await?;
            return Ok(Established {
                wire,
                prefetched: None,
                security: Security::LegacyPlain {
                    registration_key_refresh: false,
                },
            });
        };
        let message = recv(&mut wire).await?;
        let mut reply = Message::new();
        let mut security = Security::LegacyPlain {
            registration_key_refresh: false,
        };
        if let Some(message::Union::SignedId(signed)) = &message.union {
            match id_pk(&signed.id, &sign_key) {
                Ok(id) if id.id == expected_id => {
                    let their_pk = box_::PublicKey::from_slice(&id.pk)
                        .ok_or_else(|| invalid("Invalid box key"))?;
                    let (public, secret) = box_::gen_keypair();
                    let key = secretbox::gen_key();
                    let sealed = box_::seal(
                        &key.0,
                        &box_::Nonce([0; box_::NONCEBYTES]),
                        &their_pk,
                        &secret,
                    );
                    reply.set_public_key(PublicKey {
                        asymmetric_value: public.0.to_vec().into(),
                        symmetric_value: sealed.into(),
                        ..Default::default()
                    });
                    send(&mut wire, &reply).await?;
                    wire.set_key(key);
                    security = Security::Encrypted {
                        peer_id: Some(expected_id),
                    };
                    return Ok(Established {
                        wire,
                        security,
                        prefetched: None,
                    });
                }
                Ok(_) if strict => return Err(invalid("Pinned identity mismatch")),
                Ok(_) => {} // Original id mismatch: empty Message, not a key.
                Err(_) if strict => return Err(invalid("Pinned identity verification failed")),
                Err(_) => {
                    reply.set_public_key(PublicKey::new());
                    security = Security::LegacyPlain {
                        registration_key_refresh: true,
                    };
                }
            }
        }
        if strict {
            // Missing/wrong record: drop the owned socket WITHOUT the original
            // compatibility reply. No Hash reaches login and no password leaks.
            return Err(invalid("Pinned host SignedId required"));
        }
        send(&mut wire, &reply).await?;
        // Preserve 1.4.9's empty-M wire reply for non-SignedId. Unlike its
        // accidental discard, hand off the record (especially Hash) exactly
        // once. LoginResponse/media here cannot authorize the viewer.
        let prefetched = if matches!(&message.union, Some(message::Union::SignedId(_))) {
            None
        } else {
            Some(message)
        };
        Ok(Established {
            wire,
            security,
            prefetched,
        })
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Peer handshake timeout"))?
}

pub async fn host(
    mut wire: FramedStream,
    identity: HostIdentity,
    deadline: Duration,
) -> io::Result<Established> {
    init()?;
    prepare(&mut wire)?;
    tokio::time::timeout(deadline, async move {
        let HostIdentity::Signed { id, secret_key } = identity else {
            // Dialer explicitly selected the protocol's non-secure host route.
            return Ok(Established {
                wire,
                prefetched: None,
                security: Security::LegacyPlain {
                    registration_key_refresh: false,
                },
            });
        };
        let (public, secret) = box_::gen_keypair();
        let data = IdPk {
            id,
            pk: public.0.to_vec().into(),
            ..Default::default()
        }
        .write_to_bytes()
        .map_err(|_| invalid("Identity serialization failed"))?;
        let mut message = Message::new();
        message.set_signed_id(SignedId {
            id: sign::sign(&data, &secret_key).into(),
            ..Default::default()
        });
        send(&mut wire, &message).await?;
        let reply = recv(&mut wire).await?;
        let security = match reply.union {
            Some(message::Union::PublicKey(key)) if key.asymmetric_value.is_empty() => {
                Security::LegacyPlain {
                    registration_key_refresh: true,
                }
            }
            Some(message::Union::PublicKey(key)) => {
                let key = Encrypt::decode(&key.symmetric_value, &key.asymmetric_value, &secret)
                    .map_err(|_| invalid("Invalid peer key exchange"))?;
                wire.set_key(key);
                // Peer encryption does not authenticate the controlling user.
                Security::Encrypted { peer_id: None }
            }
            None => Security::LegacyPlain {
                registration_key_refresh: false,
            },
            // Do not consume an early LoginRequest as successful authorization.
            _ => return Err(invalid("Unexpected message during key transition")),
        };
        Ok(Established {
            wire,
            security,
            prefetched: None,
        })
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Peer handshake timeout"))?
}
