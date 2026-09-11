//! First original-peer direct TCP viewer. No legacy Connection/VideoHandler.
//! Surface ownership comes from the trusted HAR, not a retained numeric ID.
//! RX, one writer, and one ordered video feeder are independent persistent tasks.
//! SDK creation/destruction run on blocking workers; compressed admission waits
//! are cancellation-aware notifications, never sleep retries or frame tasks.

#[path = "platform/ohos_decoder.rs"]
pub mod decoder;
use crate::{
    handshake::ViewerIdentity,
    media_capability::{self, AdvertisedCodec, CapabilityError},
    rendezvous::{RendezvousConfig, RouteKind},
    session::{AuthenticatedParts, ViewerEvent, ViewerSession},
    transport::{WireReader, WireWriter},
};
pub use decoder::SurfaceLease;
use decoder::{
    AccessUnit, AccessUnitKind, DecoderConfig, DecoderError, DecoderObserver, SurfaceDecoder,
    VideoCodec,
};
use hbb_common::message_proto::{
    self as proto, EncodedVideoFrame, LoginRequest, Message, MouseEvent, OptionMessage, PeerInfo,
    SupportedDecoding, message, misc, option_message::BoolOption, permission_info::Permission,
    supported_decoding::PreferCodec, video_frame,
};
use std::{
    fmt,
    net::SocketAddr,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    runtime::Runtime,
    sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

// End-to-end low-latency budget: one record may be feeding while one waits.
// Larger queues preserve throughput by displaying increasingly stale frames.
const VIDEO_RECORDS: usize = 2;
const VIDEO_BYTES: usize = 32 * 1024 * 1024;
const MAX_UNITS_PER_RECORD: usize = 256;
const COMMANDS: usize = 128;
// Local feature policy for this explicitly started interactive viewer: only
// mouse input is implemented/enabled. It is NEVER granted by a peer report.
const LOCAL_MOUSE_ENABLED: bool = true;

// Deliberately no Debug: credentials and peer identities must not enter logs.
pub struct ViewerOptions {
    /// Present only for direct TCP. ID/rendezvous resolves its own peer route.
    pub address: Option<SocketAddr>,
    pub username: String,
    pub local_id: String,
    pub local_name: String,
    pub password: String,
    pub requested_fps: u32,
}
impl Drop for ViewerOptions {
    fn drop(&mut self) {
        let mut password = std::mem::take(&mut self.password).into_bytes();
        hbb_common::sodiumoxide::utils::memzero(&mut password);
    }
}

#[derive(Debug, Clone, Default)]
pub struct ViewerSnapshot {
    pub phase: String,
    pub error: Option<String>,
    pub width: i32,
    pub height: i32,
    pub codec: String,
    /// `direct_tcp`, `tcp_hole_punch` or `relay`; never inferred from an address.
    pub route: String,
    pub received_units: u64,
    pub pushed_units: u64,
    pub render_submissions: u64,
    pub keyboard_allowed: bool,
    pub closed: bool,
    /// Actual authenticated connection evidence, never inferred from options.
    pub encrypted: bool,
    pub peer_verified: bool,
}

#[derive(Debug, Clone)]
pub enum ViewerError {
    InvalidOptions,
    UnsupportedPlatform,
    NoHardwareCodec,
    RuntimeUnavailable,
    ConnectFailed,
    RendezvousFailed,
    AuthenticationFailed,
    SecondFactorUnsupported,
    TransportFailed,
    RemoteClosed,
    InvalidPeerGeometry,
    UnsupportedCodec,
    InvalidVideoRecord,
    VideoRecordTooLarge,
    TimestampOverflow,
    InputNotAllowed,
    InvalidMouse,
    Backpressure,
    Closed,
    TaskFailed,
    ReclamationUnconfirmed,
    Decoder(DecoderError),
}
impl fmt::Display for ViewerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // All variants contain local constants/native error codes only. Never
        // embed peer LoginError/MessageBox/CloseReason strings or I/O diagnostics.
        match self {
            Self::Decoder(error) => write!(f, "Surface decoder: {error:?}"),
            other => write!(f, "{other:?}"),
        }
    }
}
impl std::error::Error for ViewerError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Geometry {
    display: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}
struct Inner {
    snapshot: ViewerSnapshot,
    geometry: Option<Geometry>,
    decoder: Option<DecoderObserver>,
}
struct State {
    inner: Mutex<Inner>,
    cancel: CancellationToken,
    done: Notify,
    result: Mutex<Option<Result<(), ViewerError>>>,
    authenticated: AtomicBool,
    remote_keyboard: AtomicBool,
    // Independent from operational/auth/network errors. Set BEFORE creating a
    // native decoder and cleared only by confirmed destruction/no allocation.
    resources_unconfirmed: AtomicBool,
}
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl State {
    fn phase(&self, phase: &str) {
        lock(&self.inner).snapshot.phase = phase.into();
    }
    fn geometry(&self, geometry: Geometry) {
        let mut inner = lock(&self.inner);
        inner.geometry = Some(geometry);
        inner.snapshot.width = geometry.width;
        inner.snapshot.height = geometry.height;
    }
    fn route(&self, route: &'static str) {
        lock(&self.inner).snapshot.route = route.into();
    }
    fn snapshot(&self) -> ViewerSnapshot {
        let inner = lock(&self.inner);
        let mut out = inner.snapshot.clone();
        out.keyboard_allowed = LOCAL_MOUSE_ENABLED
            && self.authenticated.load(Ordering::Acquire)
            && self.remote_keyboard.load(Ordering::Acquire)
            && !self.cancel.is_cancelled();
        if let Some(observer) = &inner.decoder {
            if let Ok(stats) = observer.stats() {
                out.pushed_units = out.pushed_units.saturating_add(stats.pushed_units);
                out.render_submissions = out
                    .render_submissions
                    .saturating_add(stats.render_submissions);
                if let Some(error) = stats.failure {
                    out.error = Some(ViewerError::Decoder(error).to_string());
                }
                if stats.render_submissions > 0
                    && out.error.is_none()
                    && !self.cancel.is_cancelled()
                    && !out.closed
                {
                    out.phase = "streaming".into();
                }
            }
        }
        out
    }
    fn request_close(&self) {
        self.authenticated.store(false, Ordering::Release);
        self.cancel.cancel();
        let mut inner = lock(&self.inner);
        if !inner.snapshot.closed {
            inner.snapshot.phase = "closing".into();
        }
        if let Some(observer) = &inner.decoder {
            observer.request_close();
        }
    }
    fn finish(&self, result: Result<(), ViewerError>) {
        self.authenticated.store(false, Ordering::Release);
        let reclamation = if self.resources_unconfirmed.load(Ordering::Acquire) {
            Err(match &result {
                Err(error @ ViewerError::Decoder(DecoderError::DestroyFailed { .. })) => {
                    error.clone()
                }
                _ => ViewerError::ReclamationUnconfirmed,
            })
        } else {
            Ok(())
        };
        let mut inner = lock(&self.inner);
        inner.snapshot.closed = true;
        inner.snapshot.keyboard_allowed = false;
        inner.snapshot.error = result
            .as_ref()
            .err()
            .or_else(|| reclamation.as_ref().err())
            .map(ToString::to_string);
        inner.snapshot.phase = if inner.snapshot.error.is_none() {
            "closed"
        } else {
            "failed"
        }
        .into();
        // HAR uses ONLY this result to decide whether it may release its lease
        // and registry slot. Old wrong-password/disconnect errors are not a UAF.
        *lock(&self.result) = Some(reclamation);
        self.done.notify_waiters();
    }
}

pub struct Viewer {
    state: Arc<State>,
    mouse: mpsc::Sender<Message>,
}

enum Connector {
    Direct(ViewerIdentity),
    Rendezvous(RendezvousConfig),
}

fn runtime() -> Result<&'static Runtime, ViewerError> {
    crate::executor::runtime().map_err(|_| ViewerError::RuntimeUnavailable)
}

impl Viewer {
    pub fn start(
        options: ViewerOptions,
        lease: Arc<dyn SurfaceLease>,
    ) -> Result<Arc<Self>, ViewerError> {
        if options.address.is_none_or(|address| address.port() == 0) {
            return Err(ViewerError::InvalidOptions);
        }
        Self::start_with_connector(
            options,
            lease,
            Connector::Direct(ViewerIdentity::LegacyUnverified),
        )
    }

    /// Trust must come from an explicit out-of-band peer pin, not this socket.
    pub fn start_verified(
        options: ViewerOptions,
        lease: Arc<dyn SurfaceLease>,
        expected_id: String,
        peer_signing_key: hbb_common::sodiumoxide::crypto::sign::PublicKey,
    ) -> Result<Arc<Self>, ViewerError> {
        if expected_id.is_empty() || expected_id.len() > 512 {
            return Err(ViewerError::InvalidOptions);
        }
        if options.address.is_none_or(|address| address.port() == 0) {
            return Err(ViewerError::InvalidOptions);
        }
        Self::start_with_connector(
            options,
            lease,
            Connector::Direct(ViewerIdentity::PinnedHost {
                expected_id,
                peer_signing_key,
            }),
        )
    }

    /// Original hbbs/hbbr ID route with mandatory server-signed peer identity.
    /// The dialer completes SignedId/box/secretbox before password authentication.
    pub fn start_rendezvous(
        options: ViewerOptions,
        lease: Arc<dyn SurfaceLease>,
        config: RendezvousConfig,
    ) -> Result<Arc<Self>, ViewerError> {
        if options.address.is_some() || options.username != config.id {
            return Err(ViewerError::InvalidOptions);
        }
        Self::start_with_connector(options, lease, Connector::Rendezvous(config))
    }

    fn start_with_connector(
        options: ViewerOptions,
        lease: Arc<dyn SurfaceLease>,
        connector: Connector,
    ) -> Result<Arc<Self>, ViewerError> {
        if lease.surface_id() == 0
            || options.username.is_empty()
            || options.local_id.is_empty()
            || options.username.len() > 512
            || options.local_id.len() > 512
            || options.local_name.len() > 512
            || options.password.len() > 4096
            || !(1..=240).contains(&options.requested_fps)
        {
            return Err(ViewerError::InvalidOptions);
        }
        let runtime = runtime()?;
        let (mouse, mouse_rx) = mpsc::channel(COMMANDS);
        let state = Arc::new(State {
            inner: Mutex::new(Inner {
                snapshot: ViewerSnapshot {
                    phase: "capability".into(),
                    ..Default::default()
                },
                geometry: None,
                decoder: None,
            }),
            cancel: CancellationToken::new(),
            done: Notify::new(),
            result: Mutex::new(None),
            authenticated: AtomicBool::new(false),
            remote_keyboard: AtomicBool::new(false),
            resources_unconfirmed: AtomicBool::new(false),
        });
        let viewer = Arc::new(Self {
            state: state.clone(),
            mouse,
        });
        // Tasks hold State, NOT Viewer: dropping the last UI/HAR Viewer triggers
        // cancellation instead of a self-retaining session/Surface reference.
        runtime.spawn(async move {
            // Keep one final lease until run/decoder teardown ends, including
            // authentication failures; release that reference off the IO thread.
            let mut result = run(state.clone(), options, lease.clone(), mouse_rx, connector).await;
            if tokio::task::spawn_blocking(move || drop(lease))
                .await
                .is_err()
            {
                state.resources_unconfirmed.store(true, Ordering::Release);
                if result.is_ok() {
                    result = Err(ViewerError::TaskFailed);
                }
            }
            state.finish(result);
        });
        Ok(viewer)
    }
    pub fn snapshot(&self) -> ViewerSnapshot {
        self.state.snapshot()
    }

    /// x/y are selected-display-local pixels for absolute events; the original
    /// protocol receives desktop-global coordinates. Wheel/trackpad/relative
    /// events retain signed deltas. Original mask = kind | (button_bits << 3).
    pub fn send_mouse(&self, kind: u32, button: u32, x: i32, y: i32) -> Result<(), ViewerError> {
        if !LOCAL_MOUSE_ENABLED
            || !self.state.authenticated.load(Ordering::Acquire)
            || !self.state.remote_keyboard.load(Ordering::Acquire)
            || self.state.cancel.is_cancelled()
        {
            return Err(ViewerError::InputNotAllowed);
        }
        let valid = match kind {
            0 => button <= 0x1f,
            1 | 2 => matches!(button, 1 | 2 | 4 | 8 | 16),
            3..=5 => button == 0,
            _ => false,
        };
        if !valid {
            return Err(ViewerError::InvalidMouse);
        }
        let (x, y) = if kind <= 2 {
            let geometry = lock(&self.state.inner)
                .geometry
                .ok_or(ViewerError::InputNotAllowed)?;
            if x < 0 || y < 0 || x >= geometry.width || y >= geometry.height {
                return Err(ViewerError::InvalidMouse);
            }
            (
                x.checked_add(geometry.x).ok_or(ViewerError::InvalidMouse)?,
                y.checked_add(geometry.y).ok_or(ViewerError::InvalidMouse)?,
            )
        } else {
            (x, y)
        };
        let mut message = Message::new();
        message.set_mouse_event(MouseEvent {
            mask: (kind | (button << 3)) as i32,
            x,
            y,
            ..Default::default()
        });
        self.mouse.try_send(message).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => ViewerError::Backpressure,
            mpsc::error::TrySendError::Closed(_) => ViewerError::Closed,
        })
    }
    pub fn request_close(&self) {
        self.state.request_close();
    }
    /// Confirms resource reclamation only. Authentication/network/media errors
    /// stay in snapshot.error; they do not prevent a safe retry on this Surface.
    pub async fn close(&self) -> Result<(), ViewerError> {
        self.request_close();
        loop {
            let done = self.state.done.notified();
            tokio::pin!(done);
            done.as_mut().enable();
            if let Some(result) = lock(&self.state.result).clone() {
                return result;
            }
            done.await;
        }
    }
}
impl Drop for Viewer {
    fn drop(&mut self) {
        self.state.request_close();
    }
}

struct Password(Vec<u8>);
impl Drop for Password {
    fn drop(&mut self) {
        hbb_common::sodiumoxide::utils::memzero(&mut self.0);
    }
}
#[derive(Clone, Copy)]
struct Decoders {
    h264: bool,
    h265: bool,
}
fn advertised(value: &Result<AdvertisedCodec, CapabilityError>) -> bool {
    value.as_ref().is_ok_and(|v| {
        v.hardware
            && !v.codec_name.is_empty()
            && v.native_buffer_formats
                .as_ref()
                .is_ok_and(|f| !f.is_empty())
    })
}
fn capabilities() -> Result<Decoders, ViewerError> {
    let h264 = media_capability::query_h264_hardware_decoder();
    let h265 = media_capability::query_hevc_hardware_capabilities().decoder;
    if matches!(&h264, Err(CapabilityError::UnsupportedPlatform))
        && matches!(&h265, Err(CapabilityError::UnsupportedPlatform))
    {
        return Err(ViewerError::UnsupportedPlatform);
    }
    let result = Decoders {
        h264: advertised(&h264),
        h265: advertised(&h265),
    };
    if !result.h264 && !result.h265 {
        return Err(ViewerError::NoHardwareCodec);
    }
    Ok(result)
}
fn login_request(options: &ViewerOptions, decoders: Decoders) -> LoginRequest {
    // Key handshake has initialized sodium before Challenge. Never reuse a
    // process-local counter as an original-peer reconnect session identifier.
    let mut session_bytes = [0u8; 8];
    hbb_common::sodiumoxide::randombytes::randombytes_into(&mut session_bytes);
    let supported = SupportedDecoding {
        ability_h264: i32::from(decoders.h264),
        ability_h265: i32::from(decoders.h265),
        // No VPx/AV1 baseline assumption. These zeroes are deliberate.
        ability_vp9: 0,
        ability_vp8: 0,
        ability_av1: 0,
        prefer: if decoders.h265 {
            PreferCodec::H265
        } else {
            PreferCodec::H264
        }
        .into(),
        prefer_chroma: proto::Chroma::I420.into(),
        ..Default::default()
    };
    LoginRequest {
        username: options.username.clone(),
        my_id: options.local_id.clone(),
        my_name: options.local_name.clone(),
        my_platform: "HarmonyOS".into(),
        version: "1.4.9".into(),
        session_id: u64::from_le_bytes(session_bytes).max(1),
        video_ack_required: false,
        option: Some(OptionMessage {
            supported_decoding: Some(supported).into(),
            custom_fps: options.requested_fps as i32,
            disable_audio: BoolOption::Yes.into(),
            disable_clipboard: BoolOption::Yes.into(),
            enable_file_transfer: BoolOption::No.into(),
            disable_keyboard: if LOCAL_MOUSE_ENABLED {
                BoolOption::No
            } else {
                BoolOption::Yes
            }
            .into(),
            disable_camera: BoolOption::Yes.into(),
            ..Default::default()
        })
        .into(),
        ..Default::default()
    }
}

fn geometry(info: &PeerInfo) -> Result<Geometry, ViewerError> {
    let index =
        usize::try_from(info.current_display).map_err(|_| ViewerError::InvalidPeerGeometry)?;
    let display = info
        .displays
        .get(index)
        .ok_or(ViewerError::InvalidPeerGeometry)?;
    if display.width <= 0 || display.height <= 0 {
        return Err(ViewerError::InvalidPeerGeometry);
    }
    Ok(Geometry {
        display: info.current_display,
        x: display.x,
        y: display.y,
        width: display.width,
        height: display.height,
    })
}

async fn authenticate(
    state: &State,
    mut options: ViewerOptions,
    decoders: Decoders,
    connector: Connector,
) -> Result<AuthenticatedParts, ViewerError> {
    let password = Password(std::mem::take(&mut options.password).into_bytes());
    let mut session = match connector {
        Connector::Direct(identity) => {
            state.phase("connecting");
            let address = options.address.ok_or(ViewerError::InvalidOptions)?;
            let session = ViewerSession::connect_direct(address, identity, Duration::from_secs(30))
                .await
                .map_err(|_| ViewerError::ConnectFailed)?;
            state.route("direct_tcp");
            session
        }
        Connector::Rendezvous(config) => {
            state.phase("rendezvous");
            let timeout = config.connect_timeout;
            let (established, evidence) = crate::rendezvous::connect_viewer(config)
                .await
                .map_err(|_| ViewerError::RendezvousFailed)?;
            if !evidence.encrypted || !evidence.peer_verified {
                return Err(ViewerError::RendezvousFailed);
            }
            state.route(match evidence.route {
                RouteKind::TcpHolePunch => "tcp_hole_punch",
                RouteKind::Relay => "relay",
            });
            ViewerSession::from_established(established, timeout, Duration::from_secs(30))
        }
    };
    state.phase("authenticating");
    loop {
        match session
            .recv()
            .await
            .map_err(|_| ViewerError::AuthenticationFailed)?
        {
            ViewerEvent::Challenge => {
                let request = login_request(&options, decoders);
                session
                    .login(
                        request,
                        if password.0.is_empty() {
                            None
                        } else {
                            Some(&password.0)
                        },
                    )
                    .await
                    .map_err(|_| ViewerError::AuthenticationFailed)?;
            }
            ViewerEvent::Authorized(_) => {
                return session
                    .into_authenticated_parts()
                    .map_err(|_| ViewerError::AuthenticationFailed);
            }
            ViewerEvent::LoginError(error) if error == "No Password Access" => {
                state.phase("awaiting_approval")
            }
            ViewerEvent::LoginError(error)
                if error == "2FA Required" || error == "Wrong 2FA Code" =>
            {
                return Err(ViewerError::SecondFactorUnsupported);
            }
            ViewerEvent::LoginError(_) => return Err(ViewerError::AuthenticationFailed),
            ViewerEvent::Closed => return Err(ViewerError::RemoteClosed),
            ViewerEvent::Progress | ViewerEvent::PreAuthControl(_) => {}
            _ => return Err(ViewerError::AuthenticationFailed),
        }
    }
}

struct VideoRecord {
    codec: VideoCodec,
    geometry: Geometry,
    frames: Vec<EncodedVideoFrame>,
    // Held until the ordered feeder finishes this group, not released by recv.
    _slot: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
}

async fn run(
    state: Arc<State>,
    options: ViewerOptions,
    lease: Arc<dyn SurfaceLease>,
    mouse: mpsc::Receiver<Message>,
    connector: Connector,
) -> Result<(), ViewerError> {
    // Metadata IPC is also off the UI / network worker. Do not discard a native
    // creation JoinHandle on cancellation; the feeder always joins and closes it.
    let caps = tokio::task::spawn_blocking(capabilities)
        .await
        .map_err(|_| ViewerError::TaskFailed)??;
    let parts = tokio::select! {
        biased;
        _ = state.cancel.cancelled() => return Ok(()),
        parts = authenticate(&state, options, caps, connector) => parts?,
    };
    {
        let mut inner = state.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.snapshot.encrypted = matches!(
            &parts.context.security,
            crate::handshake::Security::Encrypted { .. }
        );
        inner.snapshot.peer_verified = matches!(
            &parts.context.security,
            crate::handshake::Security::Encrypted { peer_id: Some(_) }
        );
    }
    let source = geometry(
        parts
            .context
            .peer_info
            .as_ref()
            .ok_or(ViewerError::InvalidPeerGeometry)?,
    )?;
    state.geometry(source);
    state
        .remote_keyboard
        .store(parts.context.permissions.keyboard, Ordering::Release);
    state.authenticated.store(true, Ordering::Release);
    state.phase("authenticated");
    let (video_tx, video_rx) = mpsc::channel(VIDEO_RECORDS);
    let (control_tx, control_rx) = mpsc::channel(COMMANDS);
    let mut tasks = JoinSet::new();
    tasks.spawn(receive(
        state.clone(),
        parts.reader,
        source,
        caps,
        video_tx,
        control_tx,
    ));
    tasks.spawn(write(state.clone(), parts.writer, mouse, control_rx));
    tasks.spawn(feed(state.clone(), video_rx, lease));
    let first = tokio::select! {
        biased;
        _ = state.cancel.cancelled() => Ok(()),
        result = tasks.join_next() => match result {
            Some(Ok(result)) => result,
            _ => Err(ViewerError::TaskFailed),
        },
    };
    state.request_close();
    let mut result = first;
    // Never abort the feeder: it owns the native decoder and its blocking join.
    while let Some(joined) = tasks.join_next().await {
        let next = joined.unwrap_or(Err(ViewerError::TaskFailed));
        if let Err(error) = next {
            // A teardown failure must reach HAR even when localClose was first.
            if result.is_ok()
                || matches!(
                    &error,
                    ViewerError::Decoder(DecoderError::DestroyFailed { .. })
                )
            {
                result = Err(error);
            }
        }
    }
    result
}

async fn receive(
    state: Arc<State>,
    mut reader: WireReader,
    mut source: Geometry,
    caps: Decoders,
    video: mpsc::Sender<VideoRecord>,
    control: mpsc::Sender<Message>,
) -> Result<(), ViewerError> {
    let slots = Arc::new(Semaphore::new(VIDEO_RECORDS));
    let bytes = Arc::new(Semaphore::new(VIDEO_BYTES));
    loop {
        let message = tokio::select! {
            biased;
            _ = state.cancel.cancelled() => return Ok(()),
            result = reader.recv() => result.map_err(|_| ViewerError::TransportFailed)?.ok_or(ViewerError::RemoteClosed)?,
        };
        match message.union {
            Some(message::Union::VideoFrame(frame)) => {
                if frame.display != source.display {
                    return Err(ViewerError::InvalidPeerGeometry);
                }
                let (codec, frames) = match frame.union {
                    Some(video_frame::Union::H264s(frames)) if caps.h264 => {
                        (VideoCodec::H264, frames.frames)
                    }
                    Some(video_frame::Union::H265s(frames)) if caps.h265 => {
                        (VideoCodec::H265, frames.frames)
                    }
                    _ => return Err(ViewerError::UnsupportedCodec),
                };
                if frames.is_empty() {
                    continue;
                }
                if frames.len() > MAX_UNITS_PER_RECORD || frames.iter().any(|f| f.data.is_empty()) {
                    return Err(ViewerError::InvalidVideoRecord);
                }
                let length = frames
                    .iter()
                    .try_fold(0usize, |n, f| n.checked_add(f.data.len()))
                    .ok_or(ViewerError::VideoRecordTooLarge)?;
                if length > VIDEO_BYTES {
                    return Err(ViewerError::VideoRecordTooLarge);
                }
                lock(&state.inner).snapshot.received_units += frames.len() as u64;
                // Transport itself bounds the one not-yet-admitted RX record to
                // 64MiB. Admitted records INCLUDING the feeder are byte+slot bound.
                let record = tokio::select! {
                    biased;
                    _ = state.cancel.cancelled() => return Ok(()),
                    admitted = async {
                        let slot = slots.clone().acquire_owned().await.map_err(|_| ViewerError::Closed)?;
                        let permit = bytes.clone().acquire_many_owned(length as u32).await.map_err(|_| ViewerError::Closed)?;
                        Ok::<_, ViewerError>(VideoRecord { codec, geometry: source, frames, _slot: slot, _bytes: permit })
                    } => admitted?,
                };
                tokio::select! {
                    biased;
                    _ = state.cancel.cancelled() => return Ok(()),
                    sent = video.send(record) => sent.map_err(|_| ViewerError::Closed)?,
                }
            }
            Some(message::Union::TestDelay(probe)) => {
                if !probe.from_client {
                    let mut response = Message::new();
                    response.set_test_delay(probe);
                    control
                        .try_send(response)
                        .map_err(|_| ViewerError::Backpressure)?;
                }
            }
            Some(message::Union::Misc(m)) => match m.union {
                Some(misc::Union::PermissionInfo(permission)) => {
                    if permission.permission.enum_value().ok() == Some(Permission::Keyboard) {
                        state
                            .remote_keyboard
                            .store(permission.enabled, Ordering::Release);
                    }
                }
                Some(misc::Union::CloseReason(_)) => return Err(ViewerError::RemoteClosed),
                Some(misc::Union::SwitchDisplay(display)) => {
                    if display.width <= 0 || display.height <= 0 || display.display < 0 {
                        return Err(ViewerError::InvalidPeerGeometry);
                    }
                    source = Geometry {
                        display: display.display,
                        x: display.x,
                        y: display.y,
                        width: display.width,
                        height: display.height,
                    };
                }
                _ => {}
            },
            // Audio/clipboard/file features were explicitly disabled. Cursor and
            // informational messages do not authorize input or fake video state.
            _ => {}
        }
    }
}

async fn write(
    state: Arc<State>,
    mut writer: WireWriter,
    mut mouse: mpsc::Receiver<Message>,
    mut control: mpsc::Receiver<Message>,
) -> Result<(), ViewerError> {
    loop {
        let (message, input) = tokio::select! {
            biased;
            _ = state.cancel.cancelled() => return Ok(()),
            value = control.recv() => match value { Some(m) => (m, false), None => return Ok(()) },
            value = mouse.recv() => match value { Some(m) => (m, true), None => return Ok(()) },
        };
        // Recheck revocation at SEND time, not only UI enqueue time.
        if input
            && (!LOCAL_MOUSE_ENABLED
                || !state.authenticated.load(Ordering::Acquire)
                || !state.remote_keyboard.load(Ordering::Acquire))
        {
            continue;
        }
        tokio::select! {
            biased;
            _ = state.cancel.cancelled() => return Ok(()),
            result = writer.send(&message) => result.map_err(|_| ViewerError::TransportFailed)?,
        }
    }
}

async fn close_decoder(
    state: &State,
    current: &mut Option<SurfaceDecoder>,
) -> Result<(), ViewerError> {
    let Some(decoder) = current.take() else {
        return Ok(());
    };
    let observer = decoder.observer();
    let closed = tokio::task::spawn_blocking(move || decoder.close())
        .await
        .map_err(|_| ViewerError::TaskFailed);
    let stats = observer.stats();
    if stats
        .as_ref()
        .is_ok_and(|stats| stats.closed && !stats.quarantined)
    {
        // close() can carry an earlier codec/Stop error even though Destroy
        // succeeded. The worker's final ownership state is the reclamation proof.
        state.resources_unconfirmed.store(false, Ordering::Release);
    }
    let mut inner = lock(&state.inner);
    if let Ok(stats) = stats {
        inner.snapshot.pushed_units = inner
            .snapshot
            .pushed_units
            .saturating_add(stats.pushed_units);
        inner.snapshot.render_submissions = inner
            .snapshot
            .render_submissions
            .saturating_add(stats.render_submissions);
    }
    inner.decoder = None;
    closed?.map(|_| ()).map_err(ViewerError::Decoder)
}

async fn feed(
    state: Arc<State>,
    mut video: mpsc::Receiver<VideoRecord>,
    lease: Arc<dyn SurfaceLease>,
) -> Result<(), ViewerError> {
    let mut current = None;
    let result = feed_loop(&state, &mut video, lease, &mut current).await;
    if result.is_err() {
        // Network/UI cancellation must not wait for a blocking SDK teardown.
        state.request_close();
    }
    let teardown = close_decoder(&state, &mut current).await;
    teardown?;
    result
}

async fn feed_loop(
    state: &State,
    video: &mut mpsc::Receiver<VideoRecord>,
    lease: Arc<dyn SurfaceLease>,
    current: &mut Option<SurfaceDecoder>,
) -> Result<(), ViewerError> {
    let mut contract = None;
    loop {
        let observer = current.as_ref().map(SurfaceDecoder::observer);
        let record = tokio::select! {
            biased;
            _ = state.cancel.cancelled() => return Ok(()),
            stopped = async {
                match observer { Some(observer) => observer.wait_stopped().await, None => std::future::pending().await }
            } => return Err(ViewerError::Decoder(stopped.err().unwrap_or(DecoderError::Closed))),
            record = video.recv() => match record { Some(record) => record, None => return Ok(()) },
        };
        let next = (record.codec, record.geometry);
        if contract != Some(next) {
            close_decoder(state, current).await?;
            if state.cancel.is_cancelled() {
                return Ok(());
            }
            state.phase("opening_decoder");
            let lease = lease.clone();
            state.resources_unconfirmed.store(true, Ordering::Release);
            let opened = tokio::task::spawn_blocking(move || {
                SurfaceDecoder::open(
                    DecoderConfig {
                        codec: next.0,
                        width: next.1.width,
                        height: next.1.height,
                        // One AU may be inside PushInputBuffer while one waits
                        // for an input callback; do not build a stale decode tail.
                        max_queued_units: 2,
                        max_queued_bytes: VIDEO_BYTES,
                    },
                    lease,
                )
            })
            .await
            .map_err(|_| ViewerError::TaskFailed)?;
            let decoder = match opened {
                Ok(decoder) => decoder,
                Err(error) => {
                    // open() joins partial initialization cleanup before return.
                    // Only Destroy failure / worker unwind leaves native ownership
                    // unproven. OwnerLimit/QuarantinePresent allocate no decoder.
                    if !matches!(
                        error,
                        DecoderError::DestroyFailed { .. } | DecoderError::WorkerPanicked
                    ) {
                        state.resources_unconfirmed.store(false, Ordering::Release);
                    }
                    return Err(ViewerError::Decoder(error));
                }
            };
            // Keep ownership even if close arrived during the blocking open; the
            // outer feeder will always run blocking teardown, never drop on IO.
            *current = Some(decoder);
            state.geometry(next.1);
            let mut inner = lock(&state.inner);
            inner.decoder = current.as_ref().map(SurfaceDecoder::observer);
            inner.snapshot.codec = match next.0 {
                VideoCodec::H264 => "H264",
                VideoCodec::H265 => "H265",
            }
            .into();
            inner.snapshot.phase = "decoding".into();
            contract = Some(next);
        }
        let decoder = current.as_ref().ok_or(ViewerError::Closed)?;
        // Keep both record permits until its complete FIFO group has crossed
        // decoder admission. Each AU also retains decoder in-progress quota.
        for frame in record.frames {
            if state.cancel.is_cancelled() {
                return Ok(());
            }
            let pts_us = frame
                .pts
                .checked_mul(1000)
                .ok_or(ViewerError::TimestampOverflow)?;
            let unit = AccessUnit {
                bytes: frame.data.to_vec(),
                pts_us,
                kind: AccessUnitKind::Frame { key: frame.key },
            };
            if let Err(rejected) = decoder.submit_cancellable(unit, &state.cancel).await {
                if state.cancel.is_cancelled() {
                    return Ok(());
                }
                return Err(ViewerError::Decoder(rejected.reason));
            }
        }
    }
}
