//! Full-duplex RustDesk transport, without the legacy read/write select loop.
//! Handshake code supplies the established framing/encryption state. Splitting
//! preserves both nonce counters and already-buffered bytes. There is one writer
//! so control/media interleaving cannot race the encryption sequence.

use hbb_common::{
    bytes::{Bytes, BytesMut},
    bytes_codec::BytesCodec,
    futures::{
        SinkExt, StreamExt,
        stream::{SplitSink, SplitStream},
    },
    message_proto::Message,
    protobuf::Message as ProtobufMessage,
    tcp::{DynTcpStream, Encrypt, FramedStream},
};
use std::{io, time::Duration};
use tokio_util::{codec::Framed, sync::CancellationToken};

const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
type Framing = Framed<DynTcpStream, BytesCodec>;

pub struct WireReader {
    stream: SplitStream<Framing>,
    cipher: Option<Encrypt>,
    closed: CancellationToken,
}

pub struct WireWriter {
    sink: SplitSink<Framing, Bytes>,
    cipher: Option<Encrypt>,
    deadline: Option<Duration>,
    closed: CancellationToken,
}

/// An intentionally plain negotiated connection is supported; this function
/// does not make authentication decisions. Complete key transitions first.
pub fn split(mut wire: FramedStream) -> (WireReader, WireWriter) {
    wire.0.codec_mut().set_max_packet_length(
        MAX_MESSAGE_BYTES + hbb_common::sodiumoxide::crypto::secretbox::MACBYTES,
    );
    let deadline = (wire.3 != 0).then(|| Duration::from_millis(wire.3));
    let cipher = wire.2;
    let (sink, stream) = wire.0.split();
    let closed = CancellationToken::new();
    (
        WireReader {
            stream,
            cipher: cipher.clone(),
            closed: closed.clone(),
        },
        WireWriter {
            sink,
            cipher,
            deadline,
            closed,
        },
    )
}

fn disconnected() -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionAborted,
        "RustDesk transport closed",
    )
}

impl WireReader {
    pub async fn recv(&mut self) -> io::Result<Option<Message>> {
        let packet = tokio::select! {
            biased;
            _ = self.closed.cancelled() => return Err(disconnected()),
            packet = self.stream.next() => packet,
        };
        let result = match packet {
            None => {
                self.closed.cancel();
                return Ok(None);
            }
            Some(Err(error)) => Err(error),
            Some(Ok(mut bytes)) => self.decode(&mut bytes),
        };
        if result.is_err() {
            self.closed.cancel();
        }
        result.map(Some)
    }

    fn decode(&mut self, bytes: &mut BytesMut) -> io::Result<Message> {
        if let Some(cipher) = &mut self.cipher {
            cipher.dec(bytes)?;
        }
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Oversized RustDesk message",
            ));
        }
        Message::parse_from_bytes(bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
}

/// A cancelled send may already have consumed its nonce or written part of a
/// frame. It must terminate the connection, never retry on the same byte stream.
struct PendingWrite {
    closed: CancellationToken,
    complete: bool,
}
impl Drop for PendingWrite {
    fn drop(&mut self) {
        if !self.complete {
            self.closed.cancel();
        }
    }
}

impl WireWriter {
    pub async fn send(&mut self, message: &Message) -> io::Result<()> {
        if self.closed.is_cancelled() {
            return Err(disconnected());
        }
        if message.compute_size() > MAX_MESSAGE_BYTES as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Oversized RustDesk message",
            ));
        }
        let mut bytes = message
            .write_to_bytes()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let mut pending = PendingWrite {
            closed: self.closed.clone(),
            complete: false,
        };
        if let Some(cipher) = &mut self.cipher {
            bytes = cipher.enc(&bytes);
        }
        let deadline = self.deadline;
        let send = self.sink.send(Bytes::from(bytes));
        tokio::select! {
            biased;
            _ = self.closed.cancelled() => return Err(disconnected()),
            result = async {
                match deadline {
                    Some(limit) => tokio::time::timeout(limit, send).await
                        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "RustDesk write timeout"))?,
                    None => send.await,
                }
            } => result?,
        }
        pending.complete = true;
        Ok(())
    }
}

impl Drop for WireReader {
    fn drop(&mut self) {
        self.closed.cancel();
    }
}
impl Drop for WireWriter {
    fn drop(&mut self) {
        self.closed.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hbb_common::{message_proto::Hash, sodiumoxide::crypto::secretbox};

    fn message(challenge: &str) -> Message {
        let mut message = Message::new();
        message.set_hash(Hash {
            salt: "test-salt".into(),
            challenge: challenge.into(),
            ..Default::default()
        });
        message
    }

    fn pair() -> (FramedStream, FramedStream) {
        let (a, b) = tokio::io::duplex(1024);
        let address = "127.0.0.1:1".parse().unwrap();
        (
            FramedStream::from(a, address),
            FramedStream::from(b, address),
        )
    }

    #[tokio::test]
    async fn upstream_peer_roundtrip_preserves_nonce_counters_and_buffered_frames() {
        hbb_common::sodiumoxide::init().unwrap();
        let (mut current, mut upstream) = pair();
        current.set_key(secretbox::Key([7; secretbox::KEYBYTES]));
        upstream.set_key(secretbox::Key([7; secretbox::KEYBYTES]));
        let first = message("before-split");
        current.send(&first).await.unwrap();
        upstream.next().await.unwrap().unwrap();
        upstream.send(&first).await.unwrap();
        current.next().await.unwrap().unwrap();
        let later = message("after-split");
        upstream.send(&later).await.unwrap();
        let (mut reader, mut writer) = split(current);
        assert_eq!(reader.recv().await.unwrap().unwrap(), later);
        writer.send(&later).await.unwrap();
        let bytes = upstream.next().await.unwrap().unwrap();
        assert_eq!(Message::parse_from_bytes(&bytes).unwrap(), later);
    }

    #[tokio::test]
    async fn cancelled_partial_send_is_terminal_and_does_not_retry_nonce() {
        let (current, _upstream) = pair();
        let (mut reader, mut writer) = split(current);
        let large = message(&"x".repeat(8192));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), writer.send(&large))
                .await
                .is_err()
        );
        assert!(writer.send(&message("must-not-send")).await.is_err());
        assert!(reader.recv().await.is_err());
    }
}
