//! One-shot real-device host smoke. Receives compressed AUs only; no pixels,
//! credential, peer key, automatic approval or compatibility claim is recorded.
use hbb_common::message_proto::{
    self as proto, LoginRequest, Message, Misc, OptionMessage, SupportedDecoding, message, misc,
    option_message::BoolOption, supported_decoding::PreferCodec, video_frame,
};
use rd_engine::{
    handshake::ViewerIdentity,
    session::{ViewerEvent, ViewerSession},
};
use std::{error::Error, net::SocketAddr, time::Duration};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn Error>> {
    let address: SocketAddr = std::env::args()
        .nth(1)
        .ok_or("usage: host_viewer_probe <ip:port>")?
        .parse()?;
    let mut session = ViewerSession::connect_direct(
        address,
        ViewerIdentity::LegacyUnverified,
        Duration::from_secs(120),
    )
    .await?;
    let request = LoginRequest {
        username: address.ip().to_string(),
        my_id: "modern-m2-probe".into(),
        my_name: "Modern M2 Probe".into(),
        my_platform: "macOS".into(),
        version: "1.4.9".into(),
        session_id: 1,
        option: Some(OptionMessage {
            supported_decoding: Some(SupportedDecoding {
                ability_h264: 1,
                ability_h265: 1,
                prefer: PreferCodec::H265.into(),
                prefer_chroma: proto::Chroma::I420.into(),
                ..Default::default()
            })
            .into(),
            custom_fps: 60,
            disable_keyboard: BoolOption::Yes.into(),
            disable_clipboard: BoolOption::Yes.into(),
            disable_audio: BoolOption::Yes.into(),
            enable_file_transfer: BoolOption::No.into(),
            disable_camera: BoolOption::Yes.into(),
            ..Default::default()
        })
        .into(),
        ..Default::default()
    };
    loop {
        match session.recv().await? {
            ViewerEvent::Challenge => session.login(request.clone(), None).await?,
            ViewerEvent::LoginError(error) if error == "No Password Access" => {
                eprintln!("awaiting explicit local approval");
            }
            ViewerEvent::LoginError(error) => return Err(error.into()),
            ViewerEvent::Authorized(info) => {
                eprintln!(
                    "authorized {}x{}",
                    info.displays.first().map(|v| v.width).unwrap_or_default(),
                    info.displays.first().map(|v| v.height).unwrap_or_default()
                );
                break;
            }
            ViewerEvent::Closed => return Err("host closed before authorization".into()),
            _ => {}
        }
    }
    let mut parts = session.into_authenticated_parts()?;
    let mut misc = Misc::new();
    misc.set_refresh_video(true);
    let mut refresh = Message::new();
    refresh.set_misc(misc);
    parts.writer.send(&refresh).await?;
    let evidence = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let incoming = parts
                .reader
                .recv()
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| "host closed before media".to_string())?;
            match incoming.union {
                Some(message::Union::VideoFrame(frame)) => {
                    let (codec, frames) = match frame.union {
                        Some(video_frame::Union::H264s(frames)) => ("H264", frames),
                        Some(video_frame::Union::H265s(frames)) => ("H265", frames),
                        _ => return Err("unexpected non-hardware video codec".to_string()),
                    };
                    if let Some(unit) = frames.frames.first() {
                        if unit.data.len() < 5
                            || !(unit.data.starts_with(&[0, 0, 1])
                                || unit.data.starts_with(&[0, 0, 0, 1]))
                        {
                            return Err("invalid Annex-B access unit".to_string());
                        }
                        return Ok((codec, unit.data.len(), unit.key, unit.pts));
                    }
                }
                Some(message::Union::TestDelay(probe)) if !probe.from_client => {
                    let mut response = Message::new();
                    response.set_test_delay(probe);
                    parts
                        .writer
                        .send(&response)
                        .await
                        .map_err(|_| "delay reply failed".to_string())?;
                }
                Some(message::Union::Misc(m))
                    if matches!(m.union, Some(misc::Union::CloseReason(_))) =>
                {
                    return Err("host closed before media".to_string());
                }
                _ => {}
            }
        }
    })
    .await
    .map_err(|_| "timed out waiting for real encoded media")??;
    println!(
        "{{\"codec\":\"{}\",\"annexb_bytes\":{},\"key\":{},\"pts_ms\":{}}}",
        evidence.0, evidence.1, evidence.2, evidence.3
    );
    let mut close = Misc::new();
    close.set_close_reason(String::new());
    let mut message = Message::new();
    message.set_misc(close);
    let _ = parts.writer.send(&message).await;
    Ok(())
}
