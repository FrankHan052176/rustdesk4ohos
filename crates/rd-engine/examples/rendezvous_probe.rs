//! One-shot public hbbs reachability/protocol probe. It uses a deliberately
//! non-existent target and never sends a password, local identity or peer key.
use hbb_common::{
    base64::{Engine as _, engine::general_purpose::STANDARD},
    config::RS_PUB_KEY,
    sodiumoxide::crypto::sign,
};
use rd_engine::rendezvous::{RendezvousConfig, connect_viewer};
use std::{error::Error, time::Duration};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let key = STANDARD.decode(RS_PUB_KEY)?;
    let server_key = sign::PublicKey::from_slice(&key).ok_or("invalid built-in server key")?;
    let result = connect_viewer(RendezvousConfig {
        id: "m3-probe-target-that-does-not-exist".into(),
        rendezvous_server: "rs-ny.rustdesk.com:21116".into(),
        licence_key: RS_PUB_KEY.into(),
        server_key,
        relay_server: None,
        connect_timeout: Duration::from_secs(20),
    })
    .await;
    match result {
        Ok(_) => Err("unexpected peer route for reserved probe target".into()),
        Err(error)
            if error.kind() == std::io::ErrorKind::InvalidData
                && error.to_string() == "Peer unavailable or rendezvous request refused" =>
        {
            println!("public_hbbs_control_response=received_and_rejected_without_peer_route");
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}
