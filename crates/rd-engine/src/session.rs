//! Direct TCP authentication slice and post-auth dispatch gates, not a media
//! runtime. Handshake/relay callers can both supply an Established connection.
//! Authorization is not media readiness. No decoder, capture, CPU pixel copy,
//! upload fallback, or synthetic video-success path exists in this module.
use crate::{
    authentication::{
        self, AuthAction, HostAuthConfig, HostAuthState, HostAuthentication, Permissions,
    },
    handshake::{self, Established, HostIdentity, Security, ViewerIdentity},
    transport::{WireReader, WireWriter},
};
use hbb_common::{
    message_proto::{
        Auth2FA, Hash, LoginRequest, LoginResponse, Message, Misc, OptionMessage, PeerInfo,
        PermissionInfo, message, misc, permission_info::Permission,
    },
    protobuf::Message as _,
    sodiumoxide::{crypto::secretbox, utils},
    tcp::FramedStream,
};
use std::{io, net::SocketAddr, time::Duration};
use tokio::{net::TcpStream, time::Instant};

fn failure(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}
fn login_error(error: &str) -> Message {
    let mut response = LoginResponse::new();
    response.set_error(error.to_owned());
    let mut message = Message::new();
    message.set_login_response(response);
    message
}
fn close_message() -> Message {
    let mut misc = Misc::new();
    misc.set_close_reason("".into());
    let mut message = Message::new();
    message.set_misc(misc);
    message
}
fn is_close(message: &Message) -> bool {
    matches!(&message.union, Some(message::Union::Misc(m)) if matches!(m.union, Some(misc::Union::CloseReason(_))))
}

/// Authentication owns the original, unsplit, small-limit wire. Only the
/// Active-gated public handoff may turn this into full-duplex media transport.
struct SessionIo {
    wire: Option<FramedStream>,
    prefetched: Option<Message>,
}
impl SessionIo {
    fn new(mut wire: FramedStream, prefetched: Option<Message>) -> Self {
        wire.0
            .codec_mut()
            .set_max_packet_length(handshake::AUTH_MAX_RECORD_BYTES);
        Self {
            wire: Some(wire),
            prefetched,
        }
    }
    fn terminate(&mut self) {
        self.wire.take();
        self.prefetched.take();
    }
    async fn recv(&mut self) -> io::Result<Option<Message>> {
        if let Some(message) = self.prefetched.take() {
            return Ok(Some(message));
        }
        let result = match self.wire.as_mut() {
            Some(wire) => match wire.next().await {
                Some(Ok(bytes)) => Message::parse_from_bytes(&bytes)
                    .map(Some)
                    .map_err(|_| failure("Invalid authentication message")),
                Some(Err(error)) => Err(error),
                None => Ok(None),
            },
            None => return Ok(None),
        };
        if !matches!(result, Ok(Some(_))) {
            self.terminate();
        }
        result
    }
    async fn send(&mut self, message: &Message) -> io::Result<()> {
        let wire = self
            .wire
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionAborted, "Session closed"))?;
        let overhead = if wire.2.is_some() {
            secretbox::MACBYTES
        } else {
            0
        };
        if message.compute_size() > (handshake::AUTH_MAX_RECORD_BYTES - overhead) as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Oversized authentication message",
            ));
        }
        // A cancelled/failed send may have advanced the nonce or partially
        // written a record. Own the wire across await: cancellation drops it,
        // leaving self.wire=None. Never restore a half-written byte stream.
        let mut wire = self.wire.take().ok_or_else(|| failure("Session closed"))?;
        wire.send(message)
            .await
            .map_err(|_| io::Error::other("Authentication write failed"))?;
        self.wire = Some(wire);
        Ok(())
    }

    fn take_parts(&mut self) -> io::Result<(WireReader, WireWriter)> {
        // A prefetched handshake record must pass authentication dispatch first.
        if self.prefetched.is_some() {
            return Err(failure("Undispatched handshake record"));
        }
        let wire = self.wire.take().ok_or_else(|| failure("Session closed"))?;
        Ok(crate::transport::split(wire))
    }
}

/// Sanitized routing/session claims. Controller ID/name are peer claims, not
/// cryptographic identities. No password, OS credential, hwid or key is retained.
pub struct SessionClaims {
    pub target: String,
    pub controller_id: String,
    pub controller_name: String,
    pub controller_platform: String,
    pub controller_version: String,
    pub session_id: u64,
    pub video_ack_required: bool,
    pub options: Option<OptionMessage>,
}
impl From<&LoginRequest> for SessionClaims {
    fn from(request: &LoginRequest) -> Self {
        Self {
            target: request.username.clone(),
            controller_id: request.my_id.clone(),
            controller_name: request.my_name.clone(),
            controller_platform: request.my_platform.clone(),
            controller_version: request.version.clone(),
            session_id: request.session_id,
            video_ack_required: request.video_ack_required,
            options: request.option.as_ref().cloned(),
        }
    }
}

pub enum AuthenticatedSide {
    Viewer,
    Host,
}

// Original desktop hosts send only disabled permissions during Connection::start;
// original viewer initializes keyboard/file/clipboard as enabled (1.4.9
// src/flutter.rs / src/ui/remote.rs) and treats omitted remote permissions as
// allowed. These are REMOTE defaults, never a local host grant or codec claim.
// Keep Permissions::default() deny-all for local host authorization and expose
// this cached remote state only after LoginResponse.PeerInfo succeeds.
const VIEWER_REMOTE_DEFAULTS: Permissions = Permissions {
    keyboard: true,
    clipboard: true,
    audio: true,
    file: true,
};

pub struct AuthenticatedContext {
    pub side: AuthenticatedSide,
    /// Only Encrypted.peer_id=Some identifies a signature-verified host. A host
    /// must never relabel claims.controller_id as a signature-verified peer.
    pub security: Security,
    /// Host: local deny-by-default policy grant. Viewer: original remote defaults
    /// overlaid by received reports, applicable only after authentication.
    /// Runtime must additionally apply local policy, options and real capability
    /// gates; remote permission does not mean a local feature is implemented.
    pub permissions: Permissions,
    /// Exact known peer reports, preserving missing vs false. Viewer only.
    pub permission_reports: Vec<PermissionInfo>,
    pub claims: SessionClaims,
    /// Viewer only: peer-supplied metadata, not validated codec availability.
    pub peer_info: Option<PeerInfo>,
}

/// Consume the authentication driver and give the media runtime independent
/// ownership. No session select loop, echo task, lock, or hidden writer remains.
pub struct AuthenticatedParts {
    pub reader: WireReader,
    pub writer: WireWriter,
    pub context: AuthenticatedContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerState {
    AwaitHash,
    AwaitLogin,
    AwaitApproval,
    Await2Fa,
    Active,
    Closed,
}
pub enum ViewerEvent {
    Challenge,
    LoginError(String),
    /// Peer accepted authentication. Media capability/zero-copy readiness must
    /// still be established by the media provider, never inferred from login.
    Authorized(PeerInfo),
    PermissionChanged(Permissions),
    /// Explicitly allowed peer negotiation/UI control BEFORE login success.
    /// Never an authorization grant and never a media/input payload.
    PreAuthControl(Message),
    /// Authenticated wire payload, not proof of decode/render completion.
    Payload(Message),
    Progress,
    Closed,
}

pub struct ViewerSession {
    io: SessionIo,
    security: Security,
    state: ViewerState,
    hash: Option<Hash>,
    request: Option<LoginRequest>,
    permissions: Permissions,
    permission_reports: Vec<PermissionInfo>,
    peer_info: Option<PeerInfo>,
    auth_deadline: Instant,
    idle_timeout: Duration,
}

impl ViewerSession {
    pub fn from_established(
        wire: Established,
        auth_timeout: Duration,
        idle_timeout: Duration,
    ) -> Self {
        let (wire, security, prefetched) = wire.into_unsplit();
        Self {
            io: SessionIo::new(wire, prefetched),
            security,
            state: ViewerState::AwaitHash,
            hash: None,
            request: None,
            permissions: VIEWER_REMOTE_DEFAULTS,
            permission_reports: Vec::new(),
            peer_info: None,
            auth_deadline: Instant::now() + auth_timeout,
            idle_timeout,
        }
    }
    /// Address is only the TCP route. The caller supplies target username and
    /// stable-ID evidence separately when logging in / selecting handshake.
    pub async fn connect_direct(
        address: SocketAddr,
        identity: ViewerIdentity,
        timeout: Duration,
    ) -> io::Result<Self> {
        let stream = tokio::time::timeout(timeout, TcpStream::connect(address))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TCP connect timeout"))??;
        stream.set_nodelay(true)?;
        let local = stream.local_addr()?;
        let wire = handshake::viewer(FramedStream::from(stream, local), identity, timeout).await?;
        Ok(Self::from_established(
            wire,
            timeout,
            Duration::from_secs(30),
        ))
    }
    pub fn state(&self) -> ViewerState {
        self.state
    }
    pub fn security(&self) -> &Security {
        &self.security
    }
    pub fn permissions(&self) -> Permissions {
        if self.state == ViewerState::Active {
            self.permissions
        } else {
            Permissions::default()
        }
    }

    /// Fails closed (consumes/drops the session) unless LoginResponse.PeerInfo
    /// has been received. Call immediately on Authorized; independently schedule
    /// reads/writes and TestDelay responses in the new runtime thereafter.
    pub fn into_authenticated_parts(mut self) -> io::Result<AuthenticatedParts> {
        if self.state != ViewerState::Active {
            return Err(failure("Session is not authenticated"));
        }
        let claims = self
            .request
            .as_ref()
            .map(SessionClaims::from)
            .ok_or_else(|| failure("Missing authenticated request"))?;
        let (reader, writer) = self.io.take_parts()?;
        Ok(AuthenticatedParts {
            reader,
            writer,
            context: AuthenticatedContext {
                side: AuthenticatedSide::Viewer,
                security: self.security,
                permissions: self.permissions,
                permission_reports: self.permission_reports,
                claims,
                peer_info: self.peer_info,
            },
        })
    }

    fn record_permission(&mut self, info: &PermissionInfo) {
        let Ok(permission) = info.permission.enum_value() else {
            return;
        };
        match permission {
            Permission::Keyboard => self.permissions.keyboard = info.enabled,
            Permission::Clipboard => self.permissions.clipboard = info.enabled,
            Permission::Audio => self.permissions.audio = info.enabled,
            Permission::File => self.permissions.file = info.enabled,
            _ => {}
        }
        if let Some(old) = self
            .permission_reports
            .iter_mut()
            .find(|old| old.permission == info.permission)
        {
            *old = info.clone();
        } else {
            self.permission_reports.push(info.clone());
        }
    }

    fn terminate(&mut self) {
        self.state = ViewerState::Closed;
        self.permissions = Permissions::default();
        self.hash = None;
        self.request = None;
        self.permission_reports.clear();
        self.peer_info = None;
        self.io.terminate();
    }

    /// Empty None credentials preserve the original manual-approval request.
    /// Some(empty password) is rejected rather than mistaken for stored h1.
    /// `request.option.supported_decoding` is passed through from the caller's
    /// real capability provider. No VP9/H265/default decoder is inserted here.
    /// Old peers may interpret an absent advertisement as their historical VP9
    /// baseline; that wire behavior does not create a local decoder or permit
    /// a CPU-copy compatibility exception.
    pub async fn login(
        &mut self,
        mut request: LoginRequest,
        password: Option<&[u8]>,
    ) -> io::Result<()> {
        if Instant::now() >= self.auth_deadline {
            self.terminate();
            return Err(failure("Authentication deadline expired"));
        }
        if !matches!(
            self.state,
            ViewerState::AwaitLogin | ViewerState::AwaitApproval | ViewerState::Await2Fa
        ) {
            return Err(failure("Login is not allowed in this state"));
        }
        let hash = self
            .hash
            .as_ref()
            .ok_or_else(|| failure("No peer challenge"))?;
        if request.union.is_some() || request.username.is_empty() || request.my_id.is_empty() {
            return Err(failure(
                "This authentication slice supports explicit desktop identity only",
            ));
        }
        if !request.os_login.username.is_empty() || !request.os_login.password.is_empty() {
            return Err(failure("OS login unsupported"));
        }
        if let Some(first) = &self.request {
            if first.username != request.username
                || first.my_id != request.my_id
                || first.session_id != request.session_id
            {
                return Err(failure("Login retry cannot change identity/scope"));
            }
        }
        request.password = match password {
            None => Default::default(),
            Some([]) => return Err(failure("Empty supplied password")),
            Some(password) => {
                let mut h1 = authentication::salted_password(password, &hash.salt);
                let response = authentication::password_response(&h1, &hash.challenge);
                utils::memzero(&mut h1);
                response.to_vec().into()
            }
        };
        let mut retained = request.clone();
        retained.password = Default::default();
        let mut message = Message::new();
        message.set_login_request(request);
        if let Err(error) = self.io.send(&message).await {
            self.terminate();
            return Err(error);
        }
        self.request = Some(retained);
        self.state = ViewerState::AwaitLogin;
        Ok(())
    }

    pub async fn send_second_factor(&mut self, code: String) -> io::Result<()> {
        if Instant::now() >= self.auth_deadline {
            self.terminate();
            return Err(failure("Authentication deadline expired"));
        }
        if self.state != ViewerState::Await2Fa {
            return Err(failure("Not awaiting second factor"));
        }
        let mut message = Message::new();
        // No trusted-device enrollment or bypass is implemented by this slice.
        message.set_auth_2fa(Auth2FA {
            code,
            ..Default::default()
        });
        let result = self.io.send(&message).await;
        if result.is_err() {
            self.terminate();
        }
        result
    }

    pub async fn recv(&mut self) -> io::Result<ViewerEvent> {
        let result = self.recv_inner().await;
        if result.is_err() || self.state == ViewerState::Closed {
            self.terminate();
        }
        result
    }

    async fn recv_inner(&mut self) -> io::Result<ViewerEvent> {
        if self.state == ViewerState::Closed {
            return Ok(ViewerEvent::Closed);
        }
        let deadline = if self.state == ViewerState::Active {
            Instant::now() + self.idle_timeout
        } else {
            self.auth_deadline
        };
        let result = tokio::time::timeout_at(deadline, self.io.recv()).await;
        let message = match result {
            Ok(Ok(Some(message))) => message,
            Ok(Ok(None)) => {
                self.state = ViewerState::Closed;
                return Ok(ViewerEvent::Closed);
            }
            Ok(Err(error)) => {
                self.state = ViewerState::Closed;
                return Err(error);
            }
            Err(_) => {
                self.state = ViewerState::Closed;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Session receive timeout",
                ));
            }
        };
        if is_close(&message) {
            self.state = ViewerState::Closed;
            return Ok(ViewerEvent::Closed);
        }
        match &message.union {
            Some(message::Union::TestDelay(probe)) => {
                // Authentication may echo serially; Active runtime owns the
                // scheduling. Never await a write in an Active receive call.
                if self.state == ViewerState::Active {
                    return Ok(ViewerEvent::Payload(message));
                }
                if !probe.from_client {
                    self.io.send(&message).await?;
                }
                return Ok(ViewerEvent::Progress);
            }
            None => return Ok(ViewerEvent::Progress),
            // Original missing-key viewer can encounter the host's earlier
            // SignedId before Hash; it has already sent the empty compatibility M.
            Some(message::Union::SignedId(_))
                if self.state == ViewerState::AwaitHash
                    && matches!(self.security, Security::LegacyPlain { .. }) =>
            {
                return Ok(ViewerEvent::Progress);
            }
            Some(message::Union::Hash(hash)) if self.state == ViewerState::AwaitHash => {
                if hash.salt.is_empty() || hash.challenge.is_empty() {
                    self.state = ViewerState::Closed;
                    return Err(failure("Invalid authentication challenge"));
                }
                self.hash = Some(hash.clone());
                self.state = ViewerState::AwaitLogin;
                return Ok(ViewerEvent::Challenge);
            }
            Some(message::Union::LoginResponse(response)) if self.state != ViewerState::Active => {
                match &response.union {
                    Some(hbb_common::message_proto::login_response::Union::Error(error)) => {
                        // Original on_open/whitelist policy can reject before
                        // Hash or any LoginRequest. Surface the refusal, never
                        // mistake it for an application payload or an auth grant.
                        if self.request.is_none() {
                            self.state = ViewerState::Closed;
                            return Ok(ViewerEvent::LoginError(error.clone()));
                        }
                        self.state = match error.as_str() {
                            "2FA Required" | "Wrong 2FA Code" | "2FA verification unsupported" => {
                                ViewerState::Await2Fa
                            }
                            "No Password Access" => ViewerState::AwaitApproval,
                            _ => ViewerState::AwaitLogin,
                        };
                        return Ok(ViewerEvent::LoginError(error.clone()));
                    }
                    Some(hbb_common::message_proto::login_response::Union::PeerInfo(info)) => {
                        if self.request.is_none() {
                            return Err(failure("Unsolicited login success"));
                        }
                        self.state = ViewerState::Active;
                        self.peer_info = Some(info.clone());
                        return Ok(ViewerEvent::Authorized(info.clone()));
                    }
                    _ => {
                        self.state = ViewerState::Closed;
                        return Err(failure("Empty login response"));
                    }
                }
            }
            _ => {}
        }
        // Verified against 1.4.9 Connection::start: Hash -> denied permissions
        // precedes the login loop. CM may send ChatMessage/SwitchPermission
        // before authorization; Wayland is_inited sends MessageBox before the
        // successful LoginResponse. These are reports/UI only, NOT auth success.
        if let Some(message::Union::Misc(m)) = &message.union {
            if let Some(misc::Union::PermissionInfo(info)) = &m.union {
                self.record_permission(info);
                return Ok(if self.state == ViewerState::Active {
                    ViewerEvent::PermissionChanged(self.permissions)
                } else {
                    ViewerEvent::PreAuthControl(message)
                });
            }
        }
        if self.state != ViewerState::Active
            && (matches!(&message.union, Some(message::Union::MessageBox(_)))
                || matches!(&message.union, Some(message::Union::Misc(m)) if matches!(m.union, Some(misc::Union::ChatMessage(_)))))
        {
            return Ok(ViewerEvent::PreAuthControl(message));
        }
        if self.state != ViewerState::Active {
            self.state = ViewerState::Closed;
            return Err(failure("Application payload before authentication"));
        }
        Ok(ViewerEvent::Payload(message))
    }
    pub async fn close(&mut self) -> io::Result<()> {
        if self.state == ViewerState::Closed {
            return Ok(());
        }
        let result = self.io.send(&close_message()).await;
        self.terminate();
        result
    }
}

pub enum HostEvent {
    Progress,
    AwaitApproval,
    LoginRejected(&'static str),
    /// Authentication grant only, not capture/codec/display readiness.
    Authorized(Permissions),
    Unsupported,
    Closed,
}

pub struct HostSession {
    io: SessionIo,
    auth: HostAuthentication,
    security: Security,
    permissions: Permissions,
    ready: bool,
    auth_deadline: Instant,
    idle_timeout: Duration,
}

impl HostSession {
    pub async fn from_established(
        wire: Established,
        config: HostAuthConfig,
        auth_timeout: Duration,
        idle_timeout: Duration,
    ) -> io::Result<Self> {
        let auth = HostAuthentication::new(config).map_err(failure)?;
        let (wire, security, prefetched) = wire.into_unsplit();
        let mut io = SessionIo::new(wire, prefetched);
        let mut message = Message::new();
        message.set_hash(auth.challenge().clone());
        io.send(&message).await?;
        Ok(Self {
            io,
            auth,
            security,
            permissions: Permissions::default(),
            ready: false,
            auth_deadline: Instant::now() + auth_timeout,
            idle_timeout,
        })
    }
    pub async fn accept_direct(
        stream: TcpStream,
        identity: HostIdentity,
        config: HostAuthConfig,
        timeout: Duration,
    ) -> io::Result<Self> {
        stream.set_nodelay(true)?;
        let local = stream.local_addr()?;
        let wire = handshake::host(FramedStream::from(stream, local), identity, timeout).await?;
        Self::from_established(wire, config, timeout, Duration::from_secs(30)).await
    }
    pub fn state(&self) -> HostAuthState {
        self.auth.state()
    }
    pub fn security(&self) -> &Security {
        &self.security
    }
    pub fn permissions(&self) -> Permissions {
        self.permissions
    }

    /// Consumes the authenticated host driver after its success/permission
    /// records are flushed. Local auth providers/credentials are dropped here;
    /// runtime keeps only the grant and sanitized remote-desktop context.
    pub fn into_authenticated_parts(mut self) -> io::Result<AuthenticatedParts> {
        if self.auth.state() != HostAuthState::Authorized || !self.ready {
            return Err(failure("Session is not authenticated"));
        }
        let claims = self
            .auth
            .request()
            .map(SessionClaims::from)
            .ok_or_else(|| failure("Missing authenticated request"))?;
        let (reader, writer) = self.io.take_parts()?;
        Ok(AuthenticatedParts {
            reader,
            writer,
            context: AuthenticatedContext {
                side: AuthenticatedSide::Host,
                security: self.security,
                permissions: self.permissions,
                permission_reports: Vec::new(),
                claims,
                peer_info: None,
            },
        })
    }

    async fn apply(&mut self, action: AuthAction) -> io::Result<HostEvent> {
        let result = self.apply_inner(action).await;
        if result.is_err() || self.auth.state() == HostAuthState::Closed {
            self.auth.close();
            self.permissions = Permissions::default();
            self.io.terminate();
        }
        result
    }

    async fn apply_inner(&mut self, action: AuthAction) -> io::Result<HostEvent> {
        match action {
            AuthAction::Ignored => Ok(HostEvent::Progress),
            AuthAction::PendingApproval => {
                // Original pre-1.2.0 peers wait without this explanatory error.
                if self
                    .auth
                    .request()
                    .map(|v| {
                        hbb_common::get_version_number(&v.version)
                            >= hbb_common::get_version_number("1.2.0")
                    })
                    .unwrap_or(false)
                {
                    self.io.send(&login_error("No Password Access")).await?;
                }
                Ok(HostEvent::AwaitApproval)
            }
            AuthAction::Error {
                message,
                terminal: _,
            } => {
                self.io.send(&login_error(message)).await?;
                Ok(HostEvent::LoginRejected(message))
            }
            AuthAction::Authorized { info, permissions } => {
                let mut response = LoginResponse::new();
                response.set_peer_info(info);
                let mut message = Message::new();
                message.set_login_response(response);
                self.io.send(&message).await?;
                self.permissions = permissions;
                // Local capability providers authorize features, but this slice
                // has no feature adapters. It never executes those messages.
                for (permission, enabled) in [
                    (Permission::Keyboard, permissions.keyboard),
                    (Permission::Clipboard, permissions.clipboard),
                    (Permission::Audio, permissions.audio),
                    (Permission::File, permissions.file),
                ] {
                    let mut misc = Misc::new();
                    misc.set_permission_info(PermissionInfo {
                        permission: permission.into(),
                        enabled,
                        ..Default::default()
                    });
                    let mut message = Message::new();
                    message.set_misc(misc);
                    self.io.send(&message).await?;
                }
                self.ready = true;
                Ok(HostEvent::Authorized(permissions))
            }
        }
    }

    /// Call only when a trusted local approval provider has changed state.
    pub async fn poll_approval(&mut self) -> io::Result<HostEvent> {
        if Instant::now() >= self.auth_deadline && self.auth.state() != HostAuthState::Authorized {
            self.auth.close();
            self.io.terminate();
            return Err(failure("Authentication deadline expired"));
        }
        let action = self.auth.poll_approval();
        self.apply(action).await
    }

    pub async fn recv(&mut self) -> io::Result<HostEvent> {
        self.recv_notified(None).await
    }

    /// Multiplex local approval ONLY while waiting for a wire record. Once a
    /// record is read, dispatch and all its writes finish before approval can
    /// be polled. In particular, approval must not cancel a partially sent
    /// pre-auth reply (which intentionally makes SessionIo terminal).
    ///
    /// The provider must publish its decision before calling notify_one(), so
    /// a notification during dispatch is retained for the next receive boundary.
    /// Do not externally select this entire method against approval.notified().
    /// Deliberate connection cancellation remains allowed: it closes the wire.
    pub async fn recv_with_approval(
        &mut self,
        approval: &tokio::sync::Notify,
    ) -> io::Result<HostEvent> {
        self.recv_notified(Some(approval)).await
    }

    async fn recv_notified(
        &mut self,
        approval: Option<&tokio::sync::Notify>,
    ) -> io::Result<HostEvent> {
        let result = self.recv_inner(approval).await;
        if result.is_err() || self.auth.state() == HostAuthState::Closed {
            self.auth.close();
            self.permissions = Permissions::default();
            self.io.terminate();
        }
        result
    }

    async fn recv_inner(
        &mut self,
        approval: Option<&tokio::sync::Notify>,
    ) -> io::Result<HostEvent> {
        if self.auth.state() == HostAuthState::Closed {
            return Ok(HostEvent::Closed);
        }
        let deadline = if self.auth.state() == HostAuthState::Authorized {
            Instant::now() + self.idle_timeout
        } else {
            self.auth_deadline
        };
        // SessionIo.recv is cancel-safe: it only waits on FramedStream::next;
        // decrypt/parse and consuming the single prefetched record have no await
        // after consuming a frame. No send or auth dispatch occurs in this select.
        let incoming = if let Some(approval) = approval {
            tokio::select! {
                biased;
                _ = approval.notified() => None,
                incoming = tokio::time::timeout_at(deadline, self.io.recv()) => Some(incoming),
            }
        } else {
            Some(tokio::time::timeout_at(deadline, self.io.recv()).await)
        };
        let Some(incoming) = incoming else {
            // The receive future is already dropped here. Approval dispatch and
            // its success/permission writes are not raced against another wake.
            return self.poll_approval().await;
        };
        let message = match incoming {
            Ok(Ok(Some(message))) => message,
            Ok(Ok(None)) => {
                self.auth.close();
                return Ok(HostEvent::Closed);
            }
            Ok(Err(error)) => {
                self.auth.close();
                return Err(error);
            }
            Err(_) => {
                self.auth.close();
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Session receive timeout",
                ));
            }
        };
        if is_close(&message) {
            self.auth.close();
            return Ok(HostEvent::Closed);
        }
        match message.union {
            None => Ok(HostEvent::Progress),
            Some(message::Union::TestDelay(probe)) => {
                if probe.from_client {
                    let mut m = Message::new();
                    m.set_test_delay(probe);
                    self.io.send(&m).await?;
                }
                Ok(HostEvent::Progress)
            }
            Some(message::Union::LoginRequest(request)) => {
                let action = self.auth.login(request);
                self.apply(action).await
            }
            Some(message::Union::Auth2fa(code)) => {
                let action = self.auth.second_factor(code);
                self.apply(action).await
            }
            _ if self.auth.state() != HostAuthState::Authorized => {
                self.io.send(&login_error("Connection not allowed")).await?;
                self.auth.close();
                Err(failure("Application payload before authentication"))
            }
            // No input/media/file/clipboard adapter exists yet. Do not report
            // success or call a legacy runtime simply because auth succeeded.
            _ => Ok(HostEvent::Unsupported),
        }
    }
    pub async fn close(&mut self) -> io::Result<()> {
        if self.auth.state() == HostAuthState::Closed {
            return Ok(());
        }
        self.auth.close();
        self.permissions = Permissions::default();
        let result = self.io.send(&close_message()).await;
        self.io.terminate();
        result
    }
}
