// Minimal OpenHarmony adapter for native frontend callbacks and mobile clipboard state.
mod input;

use crate::client::{Data, Interface};
use crate::flutter_ffi::{EventToUI, SessionID};
use base::message_proto::{
    key_event, message, Clipboard, ClipboardFormat, KeyEvent, Message, MultiClipboards,
};
use hbb_common::ResultType;
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::Read,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
};

pub async fn query_online_states_result(
    ids: Vec<String>,
) -> ResultType<(Vec<String>, Vec<String>)> {
    crate::client::peer_online::query_online_states_result(ids).await
}

pub fn discover_lan_blocking() -> ResultType<()> {
    crate::lan::discover()
}

pub fn validate_api_server(api_server: &str, use_proxy: bool) -> ResultType<()> {
    let url = format!("{}/api/login-options", api_server.trim_end_matches('/'));
    let response = if use_proxy {
        crate::hbbs_http::create_http_client_with_url(&url)
            .get(&url)
            .timeout(std::time::Duration::from_millis(2_500))
            .send()?
    } else {
        reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_millis(2_500))
            .build()?
            .get(&url)
            .send()?
    };
    if !response.status().is_success() {
        hbb_common::bail!(
            "RustDesk API /api/login-options returned HTTP {}",
            response.status().as_u16()
        );
    }
    let _: Vec<String> = response.json()?;
    Ok(())
}

pub type SessionEventCallback = fn(SessionID, EventToUI);
pub use scrap::ohos::DirectRenderTarget;

impl flutter_rust_bridge::support::IntoDart for EventToUI {
    fn into_dart(self) -> flutter_rust_bridge::support::DartAbi {
        use flutter_rust_bridge::rust2dart::IntoIntoDart;

        match self {
            Self::Event(value) => vec![0.into_dart(), value.into_into_dart().into_dart()],
            Self::Rgba(display) => vec![1.into_dart(), display.into_into_dart().into_dart()],
            Self::Texture(display, gpu_texture) => vec![
                2.into_dart(),
                display.into_into_dart().into_dart(),
                gpu_texture.into_into_dart().into_dart(),
            ],
        }
        .into_dart()
    }
}

impl flutter_rust_bridge::support::IntoDartExceptPrimitive for EventToUI {}

impl flutter_rust_bridge::rust2dart::IntoIntoDart<EventToUI> for EventToUI {
    fn into_into_dart(self) -> Self {
        self
    }
}

impl flutter_rust_bridge::support::IntoDart for crate::flutter_ffi::OhosClipboardData {
    fn into_dart(self) -> flutter_rust_bridge::support::DartAbi {
        use flutter_rust_bridge::rust2dart::IntoIntoDart;

        vec![
            self.text.into_dart(),
            self.html.into_dart(),
            self.image.into_into_dart().into_dart(),
            self.image_format.into_into_dart().into_dart(),
            self.width.into_into_dart().into_dart(),
            self.height.into_into_dart().into_dart(),
        ].into_dart()
    }
}

impl flutter_rust_bridge::support::IntoDartExceptPrimitive for crate::flutter_ffi::OhosClipboardData {}

impl flutter_rust_bridge::rust2dart::IntoIntoDart<crate::flutter_ffi::OhosClipboardData>
    for crate::flutter_ffi::OhosClipboardData
{
    fn into_into_dart(self) -> Self {
        self
    }
}

pub fn get_active_username() -> String {
    "ohos".into()
}

pub fn check_super_user_permission() -> ResultType<bool> {
    Ok(true)
}

#[derive(Default)]
pub struct WakeLock;

impl WakeLock {
    pub fn new(_display: bool, _idle: bool, _sleep: bool) -> Self {
        Self
    }
}

lazy_static::lazy_static! {
    static ref SESSION_EVENT_CALLBACK: Mutex<Option<SessionEventCallback>> = Default::default();
    static ref STARTED_SESSIONS: Mutex<HashSet<SessionID>> = Default::default();
    static ref CLIPBOARDS_HOST: Mutex<Option<MultiClipboards>> = Default::default();
    static ref CLIENT_CLIPBOARD: Mutex<ClientClipboardState> = Default::default();
    static ref CLIENT_RECEIVED_CLIPBOARDS: Mutex<HashMap<SessionID, VecDeque<MultiClipboards>>> = Default::default();
    static ref FLUTTER_CLIENT_RECEIVED_TEXT: Mutex<Option<String>> = Default::default();
    static ref CLIENT_CLIPBOARD_FILE_ROOTS: Mutex<HashMap<SessionID, PathBuf>> = Default::default();
    static ref CLIENT_CLIPBOARD_CONN_IDS: Mutex<HashMap<String, i32>> = Default::default();
    static ref HOST_INPUT_EVENTS: Mutex<VecDeque<HostInputEvent>> = Default::default();
    static ref HOST_POINTER_POSITION: Mutex<(i32, i32)> = Default::default();
    static ref HOST_RECEIVED_CLIPBOARD: Mutex<Option<MultiClipboards>> = Default::default();
    static ref HOST_SERVER: Mutex<Option<crate::server::ServerPtr>> = Default::default();
    static ref HOST_CLIPBOARD_FILE_ROOT: Mutex<Option<PathBuf>> = Default::default();
    static ref HOST_CLIPBOARD_FILES: Mutex<Vec<String>> = Default::default();
    static ref HOST_CLIPBOARD_FILE_PENDING: Mutex<Option<Vec<String>>> = Default::default();
}

static HOST_THREAD_STARTED: AtomicBool = AtomicBool::new(false);
static HOST_ENABLED: AtomicBool = AtomicBool::new(false);
static HOST_CLIPBOARD_AVAILABLE: AtomicBool = AtomicBool::new(false);
static HOST_DISPLAY_ID: AtomicU64 = AtomicU64::new(0);
const HOST_INPUT_EVENTS_CAPACITY: usize = 256;

/// Frontend sentinel that selects the controlled-host clipboard channel instead of one
/// UI session.
const HOST_CLIPBOARD_SESSION: &str = "host";
const CLIPBOARD_IMAGE_FORMAT_PNG: &str = "png";
const CLIPBOARD_IMAGE_FORMAT_RGBA: &str = "rgba";

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostInputEvent {
    Pointer {
        kind: String,
        mask: i32,
        x: i32,
        y: i32,
    },
    Key(HostKeyEvent),
}

impl HostInputEvent {
    fn is_pointer_move(&self) -> bool {
        matches!(
            self,
            Self::Pointer { kind, mask, .. }
                if (kind == "mouse" && *mask & crate::common::input::MOUSE_TYPE_MASK
                    == crate::common::input::MOUSE_TYPE_MOVE)
                    || (kind == "touch" && *mask == 5)
        )
    }

    fn same_pointer_stream(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (
                Self::Pointer { kind: first, .. },
                Self::Pointer { kind: second, .. }
            ) if first == second
        )
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyEvent {
    pub mode: i32,
    pub mode_name: String,
    pub down: bool,
    pub press: bool,
    pub union_kind: String,
    pub control_key: Option<i32>,
    pub control_key_name: Option<String>,
    pub chr: Option<u32>,
    pub unicode: Option<u32>,
    pub seq: Option<String>,
    pub modifiers: Vec<i32>,
    pub modifier_names: Vec<String>,
}

impl HostKeyEvent {
    fn from_proto(event: &KeyEvent) -> Self {
        let mode = event.mode.value();
        let mode_name = event
            .mode
            .enum_value()
            .map(|value| format!("{value:?}"))
            .unwrap_or_else(|value| format!("Unknown({value})"));
        let (union_kind, control_key, control_key_name, chr, unicode, seq) =
            match event.union.as_ref() {
                Some(key_event::Union::ControlKey(value)) => (
                    "controlKey".to_owned(),
                    Some(value.value()),
                    Some(
                        value
                            .enum_value()
                            .map(|value| format!("{value:?}"))
                            .unwrap_or_else(|value| format!("Unknown({value})")),
                    ),
                    None,
                    None,
                    None,
                ),
                Some(key_event::Union::Chr(value)) => {
                    ("chr".to_owned(), None, None, Some(*value), None, None)
                }
                Some(key_event::Union::Unicode(value)) => {
                    ("unicode".to_owned(), None, None, None, Some(*value), None)
                }
                Some(key_event::Union::Seq(value)) => (
                    "seq".to_owned(),
                    None,
                    None,
                    None,
                    None,
                    Some(value.clone()),
                ),
                None => ("none".to_owned(), None, None, None, None, None),
                Some(_) => ("unknown".to_owned(), None, None, None, None, None),
            };
        let modifiers = event.modifiers.iter().map(|value| value.value()).collect();
        let modifier_names = event
            .modifiers
            .iter()
            .map(|value| {
                value
                    .enum_value()
                    .map(|value| format!("{value:?}"))
                    .unwrap_or_else(|value| format!("Unknown({value})"))
            })
            .collect();
        Self {
            mode,
            mode_name,
            down: event.down,
            press: event.press,
            union_kind,
            control_key,
            control_key_name,
            chr,
            unicode,
            seq,
            modifiers,
            modifier_names,
        }
    }
}

pub fn push_host_screen_frame_rgba(rgba: &[u8], width: usize, height: usize) -> bool {
    if !configure_host_screen(width, height) {
        return false;
    }
    scrap::ohos::push_screen_frame_rgba(rgba, width, height)
}

/// Configure controlled-host display geometry before host startup or capture consent.
/// Returns false for invalid geometry.
pub fn configure_host_screen(width: usize, height: usize) -> bool {
    let Some(changed) = scrap::ohos::configure_screen_size(width, height) else {
        return false;
    };
    if changed {
        crate::server::video_service::refresh();
        let server = HOST_SERVER.lock().unwrap().clone();
        if let Some(server) = server {
            server.read().unwrap().set_video_service_opt(
                None,
                crate::server::video_service::OPTION_REFRESH,
                "1",
            );
        }
    }
    true
}

pub(crate) fn register_host_server(server: crate::server::ServerPtr) {
    *HOST_SERVER.lock().unwrap() = Some(server);
}

pub fn host_screen_size() -> (usize, usize) {
    scrap::ohos::screen_size()
}

fn normalize_host_pointer(
    kind: &str,
    mask: i32,
    x: i32,
    y: i32,
    position: &mut (i32, i32),
) -> (i32, i32) {
    if kind != "mouse" {
        return (x, y);
    }
    let event_type = mask & crate::common::input::MOUSE_TYPE_MASK;
    match event_type {
        crate::common::input::MOUSE_TYPE_MOVE => {
            *position = (x, y);
            (x, y)
        }
        crate::common::input::MOUSE_TYPE_DOWN | crate::common::input::MOUSE_TYPE_UP => {
            // RustDesk's button packets intentionally carry no pointer coordinates;
            // their x/y fields are zero. Button injection must use the most recent
            // absolute move position instead of moving the host cursor to (0, 0).
            *position
        }
        _ => (x, y),
    }
}

pub(crate) fn queue_host_pointer(kind: &str, mask: i32, x: i32, y: i32) {
    let (x, y) =
        normalize_host_pointer(kind, mask, x, y, &mut HOST_POINTER_POSITION.lock().unwrap());
    if host_input_authorized() {
        if matches!(
            input::inject_pointer(kind, mask, x, y, HOST_DISPLAY_ID.load(Ordering::Acquire)),
            input::InjectionResult::RetryInFrontend
        ) {
            push_host_input_event(HostInputEvent::Pointer {
                kind: kind.to_owned(),
                mask,
                x,
                y,
            });
        }
    } else {
        push_host_input_event(HostInputEvent::Pointer {
            kind: kind.to_owned(),
            mask,
            x,
            y,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_packets_reuse_last_mouse_position() {
        let mut position = (0, 0);
        assert_eq!(
            normalize_host_pointer("mouse", 0, 960, 640, &mut position),
            (960, 640)
        );
        assert_eq!(
            normalize_host_pointer(
                "mouse",
                crate::common::input::MOUSE_BUTTON_RIGHT << 3
                    | crate::common::input::MOUSE_TYPE_DOWN,
                0,
                0,
                &mut position,
            ),
            (960, 640)
        );
        assert_eq!(
            normalize_host_pointer(
                "mouse",
                crate::common::input::MOUSE_BUTTON_RIGHT << 3 | crate::common::input::MOUSE_TYPE_UP,
                0,
                0,
                &mut position,
            ),
            (960, 640)
        );
    }
}

pub(crate) fn queue_host_key(event: &KeyEvent) {
    if host_input_authorized() {
        if matches!(
            input::inject_key(event, HOST_DISPLAY_ID.load(Ordering::Acquire)),
            input::InjectionResult::RetryInFrontend
        ) {
            push_host_input_event(HostInputEvent::Key(HostKeyEvent::from_proto(event)));
        }
    } else {
        push_host_input_event(HostInputEvent::Key(HostKeyEvent::from_proto(event)));
    }
}

pub(crate) fn set_host_display_id(display_id: u64) {
    HOST_DISPLAY_ID.store(display_id, Ordering::Release);
}

pub(crate) fn request_host_input_authorization() -> Result<(), String> {
    if !host_input_capable() {
        return Err("HarmonyOS host input is not enabled for this frontend profile".to_owned());
    }
    input::request_authorization()
}

pub(crate) fn host_input_capable() -> bool {
    cfg!(feature = "ohos-flutter")
        && hbb_common::config::LocalConfig::get_option("ohos-host-input-capable") == "Y"
}

pub(crate) fn host_input_authorized() -> bool {
    host_input_capable() && input::is_authorized()
}

pub(crate) fn set_host_clipboard_available(available: bool) {
    HOST_CLIPBOARD_AVAILABLE.store(available, Ordering::Release);
    if !available {
        CLIPBOARDS_HOST.lock().unwrap().take();
        HOST_RECEIVED_CLIPBOARD.lock().unwrap().take();
    }
}

pub(crate) fn host_clipboard_available() -> bool {
    cfg!(feature = "ohos-flutter") && HOST_CLIPBOARD_AVAILABLE.load(Ordering::Acquire)
}

pub fn update_host_text_clipboard(content: String) -> bool {
    let mut clipboards = CLIPBOARDS_HOST.lock().unwrap();
    if !host_clipboard_available() {
        return false;
    }
    *clipboards = Some(MultiClipboards {
        clipboards: vec![Clipboard {
            content: content.into_bytes().into(),
            format: ClipboardFormat::Text.into(),
            ..Default::default()
        }],
        ..Default::default()
    });
    true
}

/// Replace the pending host clipboard with a rich payload (text, html or image).
///
/// The host clipboard is a single slot owned by the controlled side, so the newest
/// local copy wins.
pub fn update_host_clipboards(clipboards: MultiClipboards) -> bool {
    if !host_clipboard_available() {
        return false;
    }
    *CLIPBOARDS_HOST.lock().unwrap() = Some(clipboards);
    true
}

pub fn take_host_received_text_clipboard() -> Option<String> {
    let clipboards = take_host_received_clipboards()?;
    clipboards.clipboards.into_iter().find_map(|clipboard| {
        (clipboard.format.enum_value() == Ok(ClipboardFormat::Text))
            .then(|| String::from_utf8(clipboard.content.to_vec()).ok())
            .flatten()
    })
}

fn push_host_input_event(event: HostInputEvent) {
    let mut events = HOST_INPUT_EVENTS.lock().unwrap();
    if event.is_pointer_move() {
        if let Some(last) = events.back_mut() {
            if last.is_pointer_move() && last.same_pointer_stream(&event) {
                *last = event;
                return;
            }
        }
    }
    if events.len() >= HOST_INPUT_EVENTS_CAPACITY {
        if let Some(position) = events.iter().position(HostInputEvent::is_pointer_move) {
            events.remove(position);
        } else if event.is_pointer_move() {
            return;
        } else {
            events.pop_front();
        }
    }
    events.push_back(event);
}

pub fn poll_host_input_event() -> Option<HostInputEvent> {
    HOST_INPUT_EVENTS.lock().unwrap().pop_front()
}

pub fn poll_host_input_event_json() -> Option<String> {
    poll_host_input_event().and_then(|event| serde_json::to_string(&event).ok())
}

pub fn start_host() -> bool {
    enable_host(false)
}

fn enable_host(force_restart: bool) -> bool {
    hbb_common::config::Config::set_option("stop-service".to_owned(), String::new());
    hbb_common::config::Config::set_option("direct-server".to_owned(), "Y".to_owned());
    let was_enabled = HOST_ENABLED.swap(true, Ordering::SeqCst);
    crate::common::set_server_running(true);

    if HOST_THREAD_STARTED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        let spawn_result = std::thread::Builder::new()
            .name("ohos-host".to_owned())
            .spawn(|| {
                crate::start_server(true);
                HOST_THREAD_STARTED.store(false, Ordering::SeqCst);
                HOST_ENABLED.store(false, Ordering::SeqCst);
                crate::common::set_server_running(false);
            });
        if let Err(err) = spawn_result {
            hbb_common::log::error!("Failed to start OHOS host thread: {err}");
            hbb_common::config::Config::set_option("stop-service".to_owned(), "Y".to_owned());
            HOST_THREAD_STARTED.store(false, Ordering::SeqCst);
            HOST_ENABLED.store(false, Ordering::SeqCst);
            crate::common::set_server_running(false);
            return false;
        }
    } else if force_restart || !was_enabled {
        crate::RendezvousMediator::restart();
    }
    true
}

pub fn restart_host() {
    let _ = enable_host(true);
}


pub fn stop_host() {
    hbb_common::config::Config::set_option("stop-service".to_owned(), "Y".to_owned());
    let was_enabled = HOST_ENABLED.swap(false, Ordering::SeqCst);
    crate::common::set_server_running(false);
    crate::ui_cm_interface::clear_host_clients();
    HOST_INPUT_EVENTS.lock().unwrap().clear();
    *HOST_POINTER_POSITION.lock().unwrap() = (0, 0);
    scrap::ohos::reset_screen_state();
    set_host_clipboard_available(false);
    input::cancel_authorization();
    if was_enabled && HOST_THREAD_STARTED.load(Ordering::SeqCst) {
        crate::RendezvousMediator::restart();
    }
}

pub fn reset_host_screen() {
    scrap::ohos::reset_screen_state();
}

pub fn host_is_started() -> bool {
    HOST_ENABLED.load(Ordering::SeqCst)
}

pub fn host_clients_state() -> String {
    crate::ui_cm_interface::get_clients_state()
}
pub fn host_client_count() -> usize {
    crate::ui_cm_interface::get_clients_length()
}
pub fn host_authorize_client(id: i32) -> bool {
    crate::ui_cm_interface::authorize_pending(id)
}
pub fn host_close_client(id: i32) -> bool {
    crate::ui_cm_interface::reject_pending(id)
}

pub(crate) fn receive_host_clipboards(mut clipboards: MultiClipboards) {
    if !bound_received_clipboards(&mut clipboards) {
        return;
    }
    let mut pending = HOST_RECEIVED_CLIPBOARD.lock().unwrap();
    if host_clipboard_available() {
        *pending = Some(clipboards);
    }
}

pub fn take_host_received_clipboards() -> Option<MultiClipboards> {
    HOST_RECEIVED_CLIPBOARD.lock().unwrap().take()
}

struct ClientClipboardState {
    enabled: bool,
    clipboards: Option<MultiClipboards>,
}

impl Default for ClientClipboardState {
    fn default() -> Self {
        Self {
            enabled: true,
            clipboards: None,
        }
    }
}

pub fn set_client_clipboard_enabled(enabled: bool) {
    let mut state = CLIENT_CLIPBOARD.lock().unwrap();
    state.enabled = enabled;
    if !enabled {
        state.clipboards.take();
        FLUTTER_CLIENT_RECEIVED_TEXT.lock().unwrap().take();
    }
}

pub fn update_client_text_clipboard(content: String) -> bool {
    let mut state = CLIENT_CLIPBOARD.lock().unwrap();
    if !state.enabled {
        return false;
    }
    state.clipboards = Some(MultiClipboards {
        clipboards: vec![Clipboard {
            content: content.into_bytes().into(),
            format: ClipboardFormat::Text.into(),
            ..Default::default()
        }],
        ..Default::default()
    });
    true
}

pub fn update_client_clipboards(clipboards: MultiClipboards) -> bool {
    let mut state = CLIENT_CLIPBOARD.lock().unwrap();
    if !state.enabled {
        return false;
    }
    state.clipboards = Some(clipboards);
    true
}


pub(crate) fn take_client_received_text_clipboard() -> Option<String> {
    let required = crate::flutter::sessions::is_ohos_client_clipboard_required();
    let text = FLUTTER_CLIENT_RECEIVED_TEXT.lock().unwrap().take();
    if required { text } else { None }
}

pub(crate) fn receive_client_clipboards(session_id: &SessionID, mut clipboards: MultiClipboards) {
    if !bound_received_clipboards(&mut clipboards) {
        return;
    }
    #[cfg(feature = "ohos-flutter")]
    {
        if let Some(text) = clipboards.clipboards.iter().find_map(|clipboard| {
            (clipboard.format.enum_value() == Ok(ClipboardFormat::Text))
                .then(|| String::from_utf8(clipboard.content.to_vec()).ok())
                .flatten()
        }) {
            *FLUTTER_CLIENT_RECEIVED_TEXT.lock().unwrap() = Some(text);
        }
        // Images and files are large and only the newest remote copy is meaningful, so
        // the frontend drains one payload per session instead of a history.
        let mut queues = CLIENT_RECEIVED_CLIPBOARDS.lock().unwrap();
        let queue = queues.entry(*session_id).or_default();
        queue.clear();
        queue.push_back(clipboards);
    }
    #[cfg(not(feature = "ohos-flutter"))]
    {
        let mut queues = CLIENT_RECEIVED_CLIPBOARDS.lock().unwrap();
        let queue = queues.entry(*session_id).or_default();
        if queue.len() >= 4 {
            queue.pop_front();
        }
        queue.push_back(clipboards);
    }
}

/// Upper bound for one received clipboard update, applied on both clipboard sides.
const MAX_RECEIVED_CLIPBOARD_BYTES: usize = 64 * 1024 * 1024;
const MAX_RECEIVED_CLIPBOARD_FORMATS: usize = 16;

fn decompress_clipboard_content(data: &[u8], limit: usize) -> Result<Vec<u8>, String> {
    let decoder = zstd::Decoder::new(data).map_err(|error| error.to_string())?;
    let mut content = Vec::new();
    decoder
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut content)
        .map_err(|error| error.to_string())?;
    if content.len() > limit {
        return Err("decompressed clipboard content exceeds the size limit".to_owned());
    }
    Ok(content)
}

/// Decompress in place and keep only payloads inside the receive limits.
///
/// Returns `false` when nothing usable remains, so the caller drops the update instead of
/// publishing an empty clipboard.
fn bound_received_clipboards(clipboards: &mut MultiClipboards) -> bool {
    let mut aggregate_size = 0usize;
    clipboards.clipboards.truncate(MAX_RECEIVED_CLIPBOARD_FORMATS);
    clipboards.clipboards.retain_mut(|clipboard| {
        if clipboard.compress {
            let Ok(content) =
                decompress_clipboard_content(&clipboard.content, MAX_RECEIVED_CLIPBOARD_BYTES)
            else {
                return false;
            };
            clipboard.content = content.into();
            clipboard.compress = false;
        }
        let Some(next_size) = aggregate_size.checked_add(clipboard.content.len()) else {
            return false;
        };
        if next_size > MAX_RECEIVED_CLIPBOARD_BYTES {
            return false;
        }
        aggregate_size = next_size;
        true
    });
    !clipboards.clipboards.is_empty()
}

pub fn take_client_received_clipboards(session_id: &SessionID) -> Option<MultiClipboards> {
    let mut queues = CLIENT_RECEIVED_CLIPBOARDS.lock().unwrap();
    let queue = queues.get_mut(session_id)?;
    let clipboards = queue.pop_front();
    if queue.is_empty() {
        queues.remove(session_id);
    }
    clipboards
}

/// Register the private incoming-file root the frontend prepared for a session.
///
/// The frontend addresses sessions by UI session id while the incoming-file materializer
/// is keyed by the native core session identity the client loop carries, so the root is
/// stored under `core_session_id`.
#[cfg(feature = "cliprdr-file-service")]
pub fn set_client_clipboard_file_root(core_session_id: &str, root: String) -> Result<(), String> {
    let session_id: SessionID = core_session_id
        .parse()
        .map_err(|_| "clipboard session has no native identity".to_owned())?;
    let root = validated_clipboard_file_root(&root)?;
    CLIENT_CLIPBOARD_FILE_ROOTS
        .lock()
        .unwrap()
        .insert(session_id, root);
    Ok(())
}

/// Rejects roots that are not the dedicated session directory the frontend prepared.
#[cfg(feature = "cliprdr-file-service")]
fn validated_clipboard_file_root(root: &str) -> Result<PathBuf, String> {
    let root = PathBuf::from(root);
    let has_unsafe_component = root.components().any(|component| {
        matches!(
            component,
            std::path::Component::CurDir | std::path::Component::ParentDir
        )
    });
    let has_dedicated_parent = root
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        == Some("rustdesk-clipboard-in");
    if !root.is_absolute()
        || has_unsafe_component
        || root.file_name().is_none()
        || !has_dedicated_parent
    {
        return Err("clipboard file root must be a dedicated session directory".to_owned());
    }
    std::fs::create_dir_all(&root)
        .map_err(|error| format!("failed to create clipboard file root: {error}"))?;
    Ok(root)
}

#[cfg(feature = "cliprdr-file-service")]
pub(crate) fn get_client_clipboard_file_root(session_id: &SessionID) -> Option<PathBuf> {
    CLIENT_CLIPBOARD_FILE_ROOTS
        .lock()
        .unwrap()
        .get(session_id)
        .cloned()
}

/// Register the incoming-file root the frontend prepared for the controlled side.
///
/// The controlled side has a single clipboard channel, so every host connection shares
/// the root the frontend armed for `host`.
#[cfg(feature = "cliprdr-file-service")]
pub fn set_host_clipboard_file_root(root: String) -> Result<(), String> {
    let root = validated_clipboard_file_root(&root)?;
    *HOST_CLIPBOARD_FILE_ROOT.lock().unwrap() = Some(root);
    Ok(())
}

#[cfg(feature = "cliprdr-file-service")]
pub(crate) fn get_host_clipboard_file_root() -> Option<PathBuf> {
    HOST_CLIPBOARD_FILE_ROOT.lock().unwrap().clone()
}

/// Queue the files a controller pasted into the controlled device's clipboard.
#[cfg(feature = "cliprdr-file-service")]
pub fn push_host_clipboard_files(paths: Vec<String>) {
    if paths.is_empty() {
        return;
    }
    HOST_CLIPBOARD_FILES.lock().unwrap().extend(paths);
}

/// Take the queued controlled-side clipboard files so the frontend can publish them.
#[cfg(feature = "cliprdr-file-service")]
pub fn take_host_clipboard_files() -> Vec<String> {
    std::mem::take(&mut *HOST_CLIPBOARD_FILES.lock().unwrap())
}

static RECORDING_DIRECTORY: Mutex<Option<String>> = Mutex::new(None);

/// Register the directory the frontend granted for recordings.
///
/// A HarmonyOS app cannot open a public directory on its own, so the frontend asks the user
/// for one with the system folder picker and registers the result here.
pub fn set_recording_directory(path: String) -> bool {
    let path = path.trim().to_owned();
    if path.is_empty() || !PathBuf::from(&path).is_absolute() {
        return false;
    }
    *RECORDING_DIRECTORY.lock().unwrap() = Some(path);
    true
}

/// Directory recordings go to, when the frontend granted one.
pub fn recording_directory() -> Option<String> {
    RECORDING_DIRECTORY.lock().unwrap().clone()
}

/// Queue files the controlled device copied so its connection can announce them.
#[cfg(feature = "cliprdr-file-service")]
pub fn update_host_file_clipboard(paths: Vec<String>) -> bool {
    if paths.is_empty() || !host_clipboard_available() {
        return false;
    }
    *HOST_CLIPBOARD_FILE_PENDING.lock().unwrap() = Some(paths);
    true
}

/// Take the pending controlled-side file selection, if the device copied files since the
/// last announcement.
#[cfg(feature = "cliprdr-file-service")]
pub(crate) fn take_host_file_clipboard() -> Option<Vec<String>> {
    HOST_CLIPBOARD_FILE_PENDING.lock().unwrap().take()
}

#[cfg(feature = "cliprdr-file-service")]
pub(crate) fn set_client_clipboard_conn_id(core_session_id: String, conn_id: i32) {
    CLIENT_CLIPBOARD_CONN_IDS
        .lock()
        .unwrap()
        .insert(core_session_id, conn_id);
}

#[cfg(feature = "cliprdr-file-service")]
pub(crate) fn clear_client_clipboard_conn_id(core_session_id: &str, conn_id: i32) {
    let mut conn_ids = CLIENT_CLIPBOARD_CONN_IDS.lock().unwrap();
    if conn_ids.get(core_session_id) == Some(&conn_id) {
        conn_ids.remove(core_session_id);
    }
    clipboard::platform::unix::serv_files::clear_conn_files(conn_id);
}

pub fn session_send_clipboards(session_id: SessionID, mut clipboards: MultiClipboards) -> bool {
    let mut msg = Message::new();
    for clipboard in &mut clipboards.clipboards {
        if clipboard.content.len() > 1024 * 1024 {
            clipboard.compress = false;
            continue;
        }
        let compressed = hbb_common::compress::compress(&clipboard.content);
        let use_compressed = compressed.len() < clipboard.content.len();
        if use_compressed {
            clipboard.content = compressed.into();
        }
        clipboard.compress = use_compressed;
    }
    msg.set_multi_clipboards(clipboards);
    session_send_clipboard_msg(session_id, msg, false)
}

pub fn session_core_connection_id(session_id: SessionID) -> Option<String> {
    crate::flutter::sessions::get_session_by_session_id(&session_id)
        .map(|session| session.core_session_id.clone())
}

pub fn session_send_clipboard_msg(session_id: SessionID, msg: Message, is_file: bool) -> bool {
    let Some(session) = crate::flutter::sessions::get_session_by_session_id(&session_id) else {
        return false;
    };
    if !session.is_default() || !session.is_ui_active() {
        return false;
    }
    if is_file {
        #[cfg(any(feature = "unix-file-copy-paste", feature = "cliprdr-file-service"))]
        if crate::is_support_file_copy_paste_num(session.lc.read().unwrap().version)
            && session.is_file_clipboard_required()
        {
            session.send(Data::Message(msg));
            return true;
        }
        return false;
    }
    if !session.is_text_clipboard_required() {
        return false;
    }
    if let Some(message::Union::MultiClipboards(multi_clipboards)) = &msg.union {
        let (version, platform) = session
            .lc
            .read()
            .unwrap()
            .peer_info
            .as_ref()
            .map(|peer| (peer.version.clone(), peer.platform.clone()))
            .unwrap_or_default();
        if let Some(msg_out) = crate::clipboard::get_msg_if_not_support_multi_clip(
            &version,
            &platform,
            multi_clipboards,
        ) {
            session.send(Data::Message(msg_out));
            return true;
        }
    }
    session.send(Data::Message(msg));
    true
}

#[cfg(feature = "cliprdr-file-service")]
pub fn session_send_file_clipboard_snapshot(
    session_id: SessionID,
    conn_id: i32,
    snapshot: clipboard::platform::unix::serv_files::PreparedConnClipFiles,
    msg: Message,
) -> bool {
    let Some(session) = crate::flutter::sessions::get_session_by_session_id(&session_id) else {
        return false;
    };
    if !session.is_default()
        || !session.is_ui_active()
        || !crate::is_support_file_copy_paste_num(session.lc.read().unwrap().version)
        || !session.is_file_clipboard_required()
    {
        return false;
    }
    session.send(Data::ClipboardFileSnapshot((conn_id, snapshot, msg)));
    true
}

#[cfg(feature = "cliprdr-file-service")]
pub fn update_client_file_clipboard(
    session_id: SessionID,
    paths: Vec<String>,
) -> Result<(), String> {
    if paths.is_empty() {
        return Err("file clipboard is empty".to_owned());
    }
    let core_session_id = session_core_connection_id(session_id)
        .ok_or_else(|| "active session clipboard connection is not ready".to_owned())?;
    let conn_id = CLIENT_CLIPBOARD_CONN_IDS
        .lock()
        .unwrap()
        .get(&core_session_id)
        .copied()
        .ok_or_else(|| "active session clipboard connection is not ready".to_owned())?;
    let snapshot = clipboard::platform::unix::serv_files::prepare_files_for_conn(&paths)
        .map_err(|e| format!("failed to stage file clipboard: {e}"))?;
    let msg =
        crate::clipboard_file::clip_2_msg(crate::clipboard_file::unix_file_clip::get_format_list());
    if session_send_file_clipboard_snapshot(session_id, conn_id, snapshot, msg) {
        Ok(())
    } else {
        Err("active session is not ready for file clipboard".to_owned())
    }
}

/// OHOS clipboard transfer requires a connected session whose UI is showing: there is no
/// background clipboard service that could carry a hidden session's clipboard.
fn is_active_clipboard_session(session: &crate::flutter::FlutterSession) -> bool {
    session.is_default()
        && session.is_ui_active()
        && session.connection_round_state.lock().unwrap().is_connected()
}

/// Whether the session's peer still allows any clipboard transfer.
///
/// Mirrors the per-format predicates the send path checks, so the frontend is never
/// handed a session id that can transfer nothing.
fn is_clipboard_authorized(session: &crate::flutter::FlutterSession) -> bool {
    #[cfg(any(feature = "unix-file-copy-paste", feature = "cliprdr-file-service"))]
    let file_required = session.is_file_clipboard_required();
    #[cfg(not(any(feature = "unix-file-copy-paste", feature = "cliprdr-file-service")))]
    let file_required = false;
    session.is_text_clipboard_required() || file_required
}

/// Resolve an id held by the frontend to the session's UI session id and its record.
///
/// Ids of closed or backgrounded sessions, and of sessions whose clipboard permission was
/// revoked, resolve to `None`, so a stale id can neither send nor read clipboard data.
fn active_clipboard_session(
    session_id: &str,
) -> Option<(SessionID, crate::flutter::FlutterSession)> {
    let session_id: SessionID = session_id.parse().ok()?;
    let session = crate::flutter::sessions::get_session_by_session_id(&session_id)?;
    (is_active_clipboard_session(&session) && is_clipboard_authorized(&session))
        .then_some((session_id, session))
}

/// UI session id the frontend should drive rich clipboard sync for.
///
/// A Flutter session records its UI session id in `core_session_id`, which is also the
/// identity the incoming-file materializer is keyed by.
pub fn active_clipboard_ui_session_id() -> Option<SessionID> {
    crate::flutter::sessions::get_sessions()
        .into_iter()
        .find_map(|session| {
            if !is_active_clipboard_session(&session) || !is_clipboard_authorized(&session) {
                return None;
            }
            let session_id = session.core_session_id.parse::<SessionID>().ok()?;
            // An added-but-not-started session is not addressable by the frontend.
            crate::flutter::sessions::get_peer_id_by_session_id(
                &session_id,
                hbb_common::rendezvous_proto::ConnType::DEFAULT_CONN,
            )?;
            Some(session_id)
        })
}

/// Register the incoming-file root prepared for a UI session.
#[cfg(feature = "cliprdr-file-service")]
pub fn set_ui_client_clipboard_file_root(session_id: &str, root: String) -> Result<(), String> {
    let ui_id: SessionID = session_id.parse().map_err(|_| "invalid clipboard UI session id")?;
    let session = crate::flutter::sessions::get_session_by_session_id(&ui_id)
        .filter(|session| session.is_default())
        .ok_or_else(|| "clipboard session does not exist".to_owned())?;
    if session.core_session_id.is_empty() {
        return Err("clipboard session has no native identity".to_owned());
    }
    set_client_clipboard_file_root(&session.core_session_id, root)
}

/// Forward a frontend clipboard text to the `host` channel or to one client session.
pub fn send_ohos_clipboard_text(session_id: &str, text: String, host_active: bool) -> bool {
    if session_id == HOST_CLIPBOARD_SESSION {
        return host_active && update_host_text_clipboard(text);
    }
    let Some((session_id, session)) = active_clipboard_session(session_id) else {
        return false;
    };
    session.is_text_clipboard_required() && session_send_clipboards(session_id, MultiClipboards {
        clipboards: vec![Clipboard {
            content: text.into_bytes().into(),
            format: ClipboardFormat::Text.into(),
            ..Default::default()
        }],
        ..Default::default()
    })
}

/// Forward a frontend HTML clipboard, with its plain-text projection, to the
/// `host` channel or to one client session.
///
/// HTML rides the same multi-clipboard message as text, so peers that only
/// accept text still receive the projection sent alongside it.
pub fn send_ohos_clipboard_html(
    session_id: &str,
    html: String,
    text: String,
    host_active: bool,
) -> bool {
    let mut entries = Vec::new();
    if !html.is_empty() {
        entries.push(Clipboard {
            content: html.into_bytes().into(),
            format: ClipboardFormat::Html.into(),
            ..Default::default()
        });
    }
    if !text.is_empty() {
        entries.push(Clipboard {
            content: text.into_bytes().into(),
            format: ClipboardFormat::Text.into(),
            ..Default::default()
        });
    }
    if entries.is_empty() {
        return false;
    }
    let clipboards = MultiClipboards {
        clipboards: entries,
        ..Default::default()
    };
    if session_id == HOST_CLIPBOARD_SESSION {
        return host_active && update_host_clipboards(clipboards);
    }
    let Some((session_id, session)) = active_clipboard_session(session_id) else {
        return false;
    };
    session.is_text_clipboard_required() && session_send_clipboards(session_id, clipboards)
}

/// Forward a frontend PNG to the `host` channel or to one client session.
///
/// The payload keeps the native multi-clipboard semantics: it becomes one `ImagePng`
/// entry and the existing clipboard send path decides on compression.
pub fn send_ohos_clipboard_image(session_id: &str, png: Vec<u8>, host_active: bool) -> bool {
    if png.is_empty() || png.len() > MAX_RECEIVED_CLIPBOARD_BYTES {
        return false;
    }
    let clipboards = MultiClipboards {
        clipboards: vec![Clipboard {
            content: png.into(),
            format: ClipboardFormat::ImagePng.into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    if session_id == HOST_CLIPBOARD_SESSION {
        return host_active && update_host_clipboards(clipboards);
    }
    let Some((session_id, session)) = active_clipboard_session(session_id) else {
        return false;
    };
    session.is_text_clipboard_required() && session_send_clipboards(session_id, clipboards)
}

/// Forward a frontend file selection to one client session or to the controlled channel.
///
/// A `host` request stages the files for the connected controller, which receives them as a
/// clipboard file offer; a session request announces local files to the peer this device
/// controls.
pub fn send_ohos_clipboard_files(session_id: &str, paths: Vec<String>) -> bool {
    if session_id == HOST_CLIPBOARD_SESSION {
        return update_host_file_clipboard(paths);
    }
    #[cfg(feature = "cliprdr-file-service")]
    {
        let Some((session_id, _)) = active_clipboard_session(session_id) else {
            return false;
        };
        return update_client_file_clipboard(session_id, paths).is_ok();
    }
    #[cfg(not(feature = "cliprdr-file-service"))]
    {
        let _ = (session_id, paths);
        false
    }
}

/// Take one pending rich clipboard payload for the frontend.
///
/// The `host` channel carries text, html and image only; received files arrive through the
/// session-scoped `clipboard_files` event instead.
pub fn take_ohos_clipboard_data(
    session_id: &str,
    host_active: bool,
) -> Option<crate::flutter_ffi::OhosClipboardData> {
    let clipboards = if session_id == HOST_CLIPBOARD_SESSION {
        if !host_active || !host_clipboard_available() {
            return None;
        }
        take_host_received_clipboards()?
    } else {
        let (session_id, session) = active_clipboard_session(session_id)?;
        if !session.is_text_clipboard_required() {
            return None;
        }
        take_client_received_clipboards(&session_id)?
    };
    ohos_clipboard_data(clipboards)
}

/// Convert one received native clipboard into the payload the frontend applies.
///
/// PNG payloads pass through as they are; raw pixels are only handed over together with the
/// dimensions they must match. `bgra` is never produced here: the remote protocol carries
/// RGBA or PNG, and the platform channel only produces BGRA on its own side.
fn ohos_clipboard_data(
    clipboards: MultiClipboards,
) -> Option<crate::flutter_ffi::OhosClipboardData> {
    let mut data = crate::flutter_ffi::OhosClipboardData::default();
    for clipboard in clipboards.clipboards {
        match clipboard.format.enum_value() {
            Ok(ClipboardFormat::Text) if data.text.is_none() => {
                data.text = String::from_utf8(clipboard.content.into()).ok();
            }
            Ok(ClipboardFormat::Html) if data.html.is_none() => {
                data.html = String::from_utf8(clipboard.content.into()).ok();
            }
            Ok(ClipboardFormat::ImagePng) if data.image.is_empty() => {
                let image: Vec<u8> = clipboard.content.into();
                if !image.is_empty() {
                    data.image = image;
                    data.image_format = CLIPBOARD_IMAGE_FORMAT_PNG.to_owned();
                }
            }
            Ok(ClipboardFormat::ImageRgba) if data.image.is_empty() => {
                if let Some((width, height)) = valid_rgba_dimensions(
                    clipboard.width,
                    clipboard.height,
                    clipboard.content.len(),
                ) {
                    data.image = clipboard.content.into();
                    data.image_format = CLIPBOARD_IMAGE_FORMAT_RGBA.to_owned();
                    data.width = width;
                    data.height = height;
                }
            }
            _ => {}
        }
    }
    (data.text.is_some() || data.html.is_some() || !data.image.is_empty()).then_some(data)
}

/// Raw pixels must describe exactly the pixels they carry, as the desktop clipboard path
/// requires before handing RGBA to a platform.
fn valid_rgba_dimensions(width: i32, height: i32, data_len: usize) -> Option<(i32, i32)> {
    let width_usize = usize::try_from(width).ok()?;
    let height_usize = usize::try_from(height).ok()?;
    if width_usize == 0 || height_usize == 0 {
        return None;
    }
    let expected_len = width_usize.checked_mul(height_usize)?.checked_mul(4)?;
    (data_len == expected_len).then_some((width, height))
}

pub fn update_clipboards(client: bool, clipboards: MultiClipboards) {
    if client {
        CLIENT_CLIPBOARD.lock().unwrap().clipboards = Some(clipboards);
    } else {
        *CLIPBOARDS_HOST.lock().unwrap() = Some(clipboards);
    }
}

pub(crate) fn get_clipboards(client: bool) -> Option<MultiClipboards> {
    if client {
        CLIENT_CLIPBOARD.lock().unwrap().clipboards.take()
    } else {
        CLIPBOARDS_HOST.lock().unwrap().take()
    }
}

pub fn register_session_event_callback(callback: SessionEventCallback) {
    *SESSION_EVENT_CALLBACK.lock().unwrap() = Some(callback);
}

#[cfg(feature = "ohos-har")]
pub fn session_start_with_polling_events(session_id: &SessionID, id: &str) -> ResultType<()> {
    let inserted = STARTED_SESSIONS.lock().unwrap().insert(*session_id);
    let already_started = !inserted;
    if let Err(err) =
        crate::flutter::session_start_with_polling_events_(session_id, id, already_started)
    {
        if inserted {
            STARTED_SESSIONS.lock().unwrap().remove(session_id);
        }
        return Err(err);
    }
    Ok(())
}

pub(crate) fn emit_session_event(session_id: &SessionID, event: EventToUI) -> bool {
    let callback = *SESSION_EVENT_CALLBACK.lock().unwrap();
    if let Some(callback) = callback {
        callback(*session_id, event);
        true
    } else {
        false
    }
}

pub(crate) fn finish_session(session_id: &SessionID) {
    STARTED_SESSIONS.lock().unwrap().remove(session_id);
    CLIENT_RECEIVED_CLIPBOARDS
        .lock()
        .unwrap()
        .remove(session_id);
    CLIENT_CLIPBOARD_FILE_ROOTS
        .lock()
        .unwrap()
        .remove(session_id);
}

pub fn register_direct_render_target_lookup(lookup: fn(&str, usize) -> Option<DirectRenderTarget>) {
    scrap::ohos::register_direct_render_target_lookup(lookup);
}

#[cfg(all(test, feature = "ohos-flutter"))]
mod flutter_clipboard_tests {
    use super::*;

    fn image_clipboards(
        format: ClipboardFormat,
        width: i32,
        height: i32,
        content: Vec<u8>,
    ) -> MultiClipboards {
        MultiClipboards {
            clipboards: vec![Clipboard {
                content: content.into(),
                format: format.into(),
                width,
                height,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn remote_text_reaches_flutter_without_har_polling() {
        let session_id = SessionID::new_v4();
        for (text, compressed) in [("Windows → 鸿蒙\nsecond line", false), ("compressed text", true), ("", false)] {
            let content = if compressed {
                hbb_common::compress::compress(text.as_bytes())
            } else {
                text.as_bytes().to_vec()
            };
            receive_client_clipboards(&session_id, MultiClipboards {
                clipboards: vec![Clipboard {
                    content: content.into(),
                    format: ClipboardFormat::Text.into(),
                    compress: compressed,
                    ..Default::default()
                }],
                ..Default::default()
            });
            assert_eq!(FLUTTER_CLIENT_RECEIVED_TEXT.lock().unwrap().take().as_deref(), Some(text));
            // The rich channel keeps the same payload once, newest wins.
            assert!(take_client_received_clipboards(&session_id).is_some());
        }

        receive_client_clipboards(&session_id, MultiClipboards {
            clipboards: vec![Clipboard {
                content: vec![0xff].into(),
                format: ClipboardFormat::Text.into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        assert!(FLUTTER_CLIENT_RECEIVED_TEXT.lock().unwrap().is_none());

        receive_client_clipboards(&session_id, MultiClipboards {
            clipboards: vec![Clipboard {
                content: b"pending remote copy".to_vec().into(),
                format: ClipboardFormat::Text.into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        set_client_clipboard_enabled(false);
        assert!(FLUTTER_CLIENT_RECEIVED_TEXT.lock().unwrap().is_none());
        assert!(!update_client_text_clipboard("disabled local copy".into()));
        set_client_clipboard_enabled(true);
        assert!(update_client_text_clipboard("local copy".into()));
        let outgoing = get_clipboards(true).unwrap();
        assert_eq!(outgoing.clipboards[0].content.as_ref(), b"local copy");
        assert!(get_clipboards(true).is_none());
    }

    #[test]
    fn rich_clipboard_maps_text_html_and_png() {
        let data = ohos_clipboard_data(MultiClipboards {
            clipboards: vec![
                Clipboard {
                    content: b"hello".to_vec().into(),
                    format: ClipboardFormat::Text.into(),
                    ..Default::default()
                },
                Clipboard {
                    content: b"<b>hi</b>".to_vec().into(),
                    format: ClipboardFormat::Html.into(),
                    ..Default::default()
                },
                Clipboard {
                    content: vec![0x89, 0x50].into(),
                    format: ClipboardFormat::ImagePng.into(),
                    ..Default::default()
                },
                Clipboard {
                    content: vec![0xff, 0xfe].into(),
                    format: ClipboardFormat::Rtf.into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        })
        .unwrap();
        assert_eq!(data.text.as_deref(), Some("hello"));
        assert_eq!(data.html.as_deref(), Some("<b>hi</b>"));
        assert_eq!(data.image, vec![0x89, 0x50]);
        assert_eq!(data.image_format, CLIPBOARD_IMAGE_FORMAT_PNG);
        assert_eq!((data.width, data.height), (0, 0));

        assert!(ohos_clipboard_data(MultiClipboards {
            clipboards: vec![Clipboard {
                content: vec![0xff].into(),
                format: ClipboardFormat::Text.into(),
                ..Default::default()
            }],
            ..Default::default()
        })
        .is_none());
    }

    #[test]
    fn raw_image_requires_matching_dimensions() {
        let rgba = vec![7u8; 2 * 3 * 4];
        let data =
            ohos_clipboard_data(image_clipboards(ClipboardFormat::ImageRgba, 2, 3, rgba.clone()))
                .unwrap();
        assert_eq!(data.image_format, CLIPBOARD_IMAGE_FORMAT_RGBA);
        assert_eq!((data.width, data.height), (2, 3));
        assert_eq!(data.image, rgba);

        assert!(ohos_clipboard_data(image_clipboards(
            ClipboardFormat::ImageRgba,
            2,
            3,
            vec![7; 4]
        ))
        .is_none());
        assert!(ohos_clipboard_data(image_clipboards(
            ClipboardFormat::ImageRgba,
            0,
            3,
            Vec::new()
        ))
        .is_none());
    }

    #[test]
    fn received_rich_payload_is_scoped_and_drained_once_per_session() {
        let session_id = SessionID::new_v4();
        let other_session_id = SessionID::new_v4();
        receive_client_clipboards(
            &session_id,
            image_clipboards(ClipboardFormat::ImagePng, 0, 0, vec![1, 2, 3]),
        );
        assert!(take_client_received_clipboards(&other_session_id).is_none());
        let data = ohos_clipboard_data(take_client_received_clipboards(&session_id).unwrap())
            .unwrap();
        assert_eq!(data.image, vec![1, 2, 3]);
        assert!(take_client_received_clipboards(&session_id).is_none());
    }

    #[test]
    fn received_formats_are_bounded() {
        let session_id = SessionID::new_v4();
        receive_client_clipboards(
            &session_id,
            MultiClipboards {
                clipboards: (0..MAX_RECEIVED_CLIPBOARD_FORMATS + 4)
                    .map(|index| Clipboard {
                        content: vec![index as u8].into(),
                        format: ClipboardFormat::ImagePng.into(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            },
        );
        let stored = take_client_received_clipboards(&session_id).unwrap();
        assert_eq!(stored.clipboards.len(), MAX_RECEIVED_CLIPBOARD_FORMATS);

        receive_client_clipboards(
            &session_id,
            MultiClipboards {
                clipboards: vec![Clipboard {
                    content: vec![0xff, 0x00, 0x01].into(),
                    format: ClipboardFormat::ImagePng.into(),
                    compress: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        assert!(take_client_received_clipboards(&session_id).is_none());
    }

    #[test]
    fn unknown_session_id_cannot_send_or_read() {
        let stale = SessionID::new_v4().to_string();
        assert!(!send_ohos_clipboard_text(&stale, "text".to_owned(), false));
        assert!(!send_ohos_clipboard_image(&stale, vec![1], false));
        assert!(!send_ohos_clipboard_files(&stale, vec!["/tmp/a".to_owned()]));
        assert!(take_ohos_clipboard_data(&stale, false).is_none());
        assert!(take_ohos_clipboard_data("not-a-session", false).is_none());
        assert!(take_ohos_clipboard_data("", false).is_none());
        assert!(active_clipboard_ui_session_id().is_none());
    }

    #[test]
    fn host_channel_requires_native_activation() {
        assert!(!send_ohos_clipboard_text(
            HOST_CLIPBOARD_SESSION,
            "blocked".to_owned(),
            true
        ));
        assert!(take_ohos_clipboard_data(HOST_CLIPBOARD_SESSION, true).is_none());
        receive_host_clipboards(image_clipboards(ClipboardFormat::ImagePng, 0, 0, vec![9]));
        assert!(take_ohos_clipboard_data(HOST_CLIPBOARD_SESSION, true).is_none());

        set_host_clipboard_available(true);
        assert!(send_ohos_clipboard_text(
            HOST_CLIPBOARD_SESSION,
            "host copy".to_owned(),
            true
        ));
        let forwarded = get_clipboards(false).unwrap();
        assert_eq!(forwarded.clipboards[0].content.as_ref(), b"host copy");
        assert!(send_ohos_clipboard_image(
            HOST_CLIPBOARD_SESSION,
            vec![1, 2],
            true
        ));
        let forwarded = get_clipboards(false).unwrap();
        assert_eq!(
            forwarded.clipboards[0].format.enum_value(),
            Ok(ClipboardFormat::ImagePng)
        );
        assert!(!send_ohos_clipboard_image(
            HOST_CLIPBOARD_SESSION,
            Vec::new(),
            true
        ));
        assert!(!send_ohos_clipboard_files(
            HOST_CLIPBOARD_SESSION,
            vec!["/tmp/a".to_owned()]
        ));
        assert!(!send_ohos_clipboard_text(
            HOST_CLIPBOARD_SESSION,
            "inactive".to_owned(),
            false
        ));

        receive_host_clipboards(image_clipboards(ClipboardFormat::ImagePng, 0, 0, vec![9]));
        let data = take_ohos_clipboard_data(HOST_CLIPBOARD_SESSION, true).unwrap();
        assert_eq!(data.image, vec![9]);
        assert!(take_ohos_clipboard_data(HOST_CLIPBOARD_SESSION, true).is_none());

        set_host_clipboard_available(false);
        assert!(!send_ohos_clipboard_text(
            HOST_CLIPBOARD_SESSION,
            "blocked".to_owned(),
            true
        ));
        assert!(take_ohos_clipboard_data(HOST_CLIPBOARD_SESSION, true).is_none());
    }
}
