// OHOS platform stubs
use crate::flutter_ffi::{EventToUI, SessionID};
use hbb_common::{message_proto::DisplayInfo, ResultType};
use std::{collections::HashSet, sync::Mutex};

pub type SessionEventCallback = fn(SessionID, EventToUI);
pub type RenderStatsCallback = fn(String, usize, Option<u64>);
pub use scrap::ohos::DirectRenderTarget;

lazy_static::lazy_static! {
    static ref SESSION_EVENT_CALLBACK: Mutex<Option<SessionEventCallback>> = Default::default();
    static ref RENDER_STATS_CALLBACK: Mutex<Option<RenderStatsCallback>> = Default::default();
    static ref STARTED_SESSIONS: Mutex<HashSet<SessionID>> = Default::default();
}

pub fn register_session_event_callback(callback: SessionEventCallback) {
    *SESSION_EVENT_CALLBACK.lock().unwrap() = Some(callback);
}

pub fn session_start_with_polling_events(session_id: &SessionID, id: &str) -> ResultType<()> {
    let already_started = !STARTED_SESSIONS.lock().unwrap().insert(*session_id);
    if let Err(err) = crate::flutter::session_start_(session_id, id, already_started) {
        STARTED_SESSIONS.lock().unwrap().remove(session_id);
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
}

pub fn register_render_stats_callback(callback: RenderStatsCallback) {
    *RENDER_STATS_CALLBACK.lock().unwrap() = Some(callback);
}

pub(crate) fn notify_frame_rendered(session: String, display: usize, latency: Option<u64>) {
    let callback = *RENDER_STATS_CALLBACK.lock().unwrap();
    if let Some(callback) = callback {
        callback(session, display, latency);
    }
}

pub fn register_direct_render_target_lookup(lookup: fn(&str, usize) -> Option<DirectRenderTarget>) {
    scrap::ohos::register_direct_render_target_lookup(lookup);
}

#[inline]
pub fn is_prelogin() -> bool {
    false
}
#[inline]
pub fn is_x11() -> bool {
    false
}
#[inline]
pub fn clip_cursor(_rect: Option<(i32, i32, i32, i32)>) -> bool {
    false
}
#[inline]
pub fn get_cursor() -> ResultType<Option<usize>> {
    Ok(None)
}
#[inline]
pub fn get_cursor_data(_h: usize) -> ResultType<hbb_common::message_proto::CursorData> {
    Ok(Default::default())
}
#[inline]
pub fn get_cursor_pos() -> Option<(i32, i32)> {
    None
}
#[inline]
pub fn set_cursor_pos(_x: i32, _y: i32) -> bool {
    false
}
#[inline]
pub fn get_focused_display(_displays: Vec<DisplayInfo>) -> Option<usize> {
    None
}
#[inline]
pub fn start_os_service() {}
#[inline]
pub fn get_double_click_time() -> u32 {
    500
}
#[inline]
pub fn wide_string(_s: &str) -> Vec<u16> {
    vec![]
}
#[inline]
pub fn get_active_username() -> String {
    String::new()
}
#[inline]
pub fn is_installed() -> bool {
    false
}
#[inline]
pub fn is_root() -> bool {
    false
}
#[inline]
pub fn get_active_user_home() -> Option<std::path::PathBuf> {
    None
}
#[inline]
pub fn quit_gui() {}
#[inline]
pub fn check_super_user_permission() -> bool {
    false
}
#[inline]
pub fn check_autostart_config() -> bool {
    false
}
#[inline]
pub fn install_service() -> bool {
    false
}
#[inline]
pub fn uninstall_service(_show_new_window: bool, _sync: bool) -> bool {
    false
}
pub const PA_SAMPLE_RATE: u32 = 48000;
pub struct WakeLock;
impl WakeLock {
    pub fn new(_a: bool, _b: bool, _c: bool) -> Self {
        WakeLock
    }
}
pub fn get_wakelock(_d: bool) -> WakeLock {
    WakeLock
}
pub struct WallPaperRemover;
impl WallPaperRemover {
    pub fn new() -> ResultType<Self> {
        Ok(Self)
    }
    pub fn support() -> bool {
        false
    }
    pub fn remove_all() {}
    pub fn restore_all() {}
}
#[inline]
#[rustfmt::skip]
pub fn current_resolution(_n: &str) -> ResultType<hbb_common::message_proto::Resolution> {
    Err(hbb_common::anyhow::anyhow!("Display resolution is unavailable on OpenHarmony controller"))
}
#[inline]
#[rustfmt::skip]
pub fn change_resolution_directly(_n: &str, _w: usize, _h: usize) -> ResultType<()> {
    Err(hbb_common::anyhow::anyhow!("Display resolution changes are unavailable on OpenHarmony controller"))
}
#[inline]
pub fn resolutions(_n: &str) -> Vec<hbb_common::message_proto::Resolution> {
    Vec::new()
}
