//! SDR screen -> hardware encoder input Surface -> Annex-B access units.
//! No VideoService/scrap, CPU pixel address, pixel upload, conversion shader or
//! software codec. GetAddr below is ONLY for COMPRESSED encoder output.
//!
//! Public API contract: VideoEncoder_GetSurface returns an owned NativeWindow
//! (API9); ScreenCapture_StartScreenCaptureWithSurface accepts that NativeWindow
//! (API12). Pixel format SURFACE_FORMAT=4 means obtain format from that Surface,
//! not reinterpret RGBA as YUV. Configure/GetSurface/Start failures are terminal,
//! never a copy fallback. Actual device interoperability remains to be verified.
//! References: capi-native-avcodec-videoencoder-h, capi-native-avscreen-capture-h,
//! capi-native-avformat-h on developer.huawei.com/consumer/cn/doc/harmonyos-references/.
//! Installed headers26; every bound function/key is introduced <=22.
//!
//! Caller provides CURRENT full home-screen pixel geometry, never tablet-relative
//! negotiated downscaling. Close/reopen on display geometry/orientation changes.
//! Capture uses the system consent UI; no privacy bypass/hidden picker permission.
//! This is RGBA8/SDR with documented screen-source <=60, NOT HDR/120 evidence.

#![cfg_attr(not(target_env = "ohos"), allow(dead_code))]

use hbb_common::bytes::Bytes;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    H265,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublisherBackend {
    Auto,
    DxgiNvenc,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecSelection {
    Auto,
    H264,
    H265,
}
#[derive(Debug, Clone)]
pub struct PublisherDisplay {
    pub width: i32,
    pub height: i32,
    pub name: String,
}
pub fn probe_display(
    _backend: PublisherBackend,
    _output_index: usize,
) -> Result<PublisherDisplay, PublisherError> {
    Err(PublisherError::UnsupportedPlatform)
}
#[derive(Debug, Clone, Copy)]
pub struct SupportedCodecs {
    pub h264: bool,
    pub h265: bool,
}
/// Synchronous metadata-only query; no encoder/capture instance or consent UI.
/// Checks hardware identity, RGBA native Surface format AND SDR profile support.
/// These are advertised capabilities, not measured throughput or proof of start.
/// Call off UI if the caller cannot tolerate platform metadata IPC.
pub fn supported_codecs() -> Result<SupportedCodecs, PublisherError> {
    #[cfg(target_env = "ohos")]
    {
        let avc = native::hardware_name(Codec::H264);
        let hevc = native::hardware_name(Codec::H265);
        let supported = SupportedCodecs {
            h264: avc.is_ok(),
            h265: hevc.is_ok(),
        };
        if !supported.h264 && !supported.h265 {
            return Err(hevc.err().unwrap_or(PublisherError::NoHardwareEncoder));
        }
        Ok(supported)
    }
    #[cfg(not(target_env = "ohos"))]
    {
        Err(PublisherError::UnsupportedPlatform)
    }
}
pub fn supported_codecs_for(config: &PublisherConfig) -> Result<SupportedCodecs, PublisherError> {
    if config.backend == PublisherBackend::DxgiNvenc || config.output_index != 0 {
        return Err(PublisherError::BackendUnavailable);
    }
    supported_codecs()
}
#[derive(Debug, Clone, Copy)]
pub struct PublisherConfig {
    pub codec: Codec,
    pub width: i32,
    pub height: i32,
    pub fps: u32,
    pub bitrate: i64,
    pub max_queued_units: usize,
    pub max_queued_bytes: usize,
    pub backend: PublisherBackend,
    pub output_index: usize,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublisherError {
    UnsupportedPlatform,
    InvalidConfig,
    ScreenSourceLimitedTo60,
    NoHardwareEncoder,
    SurfaceInputUnsupported,
    Native {
        api: &'static str,
        code: i32,
    },
    NativeNull {
        api: &'static str,
    },
    InvalidBuffer,
    InvalidAnnexB,
    MissingParameterSets,
    OutputTooLarge,
    UnexpectedDiscard,
    CallbackOverflow,
    DuplicateOutput,
    ConsentEnded {
        state: i32,
    },
    ContentUnavailable,
    GeometryChanged,
    Closed,
    /// OS refused to start the worker; no native instance was created.
    WorkerStartFailed,
    WorkerFailed,
    OwnerLimit,
    QuarantinePresent,
    ReclamationUnconfirmed,
    BackendUnavailable,
    OutputNotFound,
}
impl std::fmt::Display for PublisherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for PublisherError {}
impl PublisherError {
    /// True only when this failure means a native owner may still exist or a
    /// prior publisher quarantine makes reclamation globally unconfirmed.
    pub fn resources_unconfirmed(&self) -> bool {
        matches!(
            self,
            Self::WorkerFailed | Self::QuarantinePresent | Self::ReclamationUnconfirmed
        )
    }
}

#[derive(Debug, Clone)]
pub struct PublisherStats {
    pub consent_state: String,
    pub capture_state_code: i32,
    pub width: i32,
    pub height: i32,
    pub fps_limit: u32,
    pub encoder_name: String,
    pub encoded_frames: u64,
    pub encoded_bytes: u64,
    /// Includes the AU currently leased to the network writer.
    pub queued_units: usize,
    pub queued_bytes: usize,
    pub closed: bool,
    pub quarantined: bool,
    pub error: Option<PublisherError>,
}

/// Compressed data only. No Debug (do not log captured content).
/// Keep this guard alive THROUGH the network send. Clone Bytes into protobuf;
/// Drop releases FIFO admission credit. pts_us is the SDK microsecond timestamp,
/// NOT necessarily wall-clock epoch; wire adapter owns ms conversion/timebase.
pub struct EncodedUnit {
    pub data: Bytes,
    pub pts_us: i64,
    pub key: bool,
    shared: Arc<Shared>,
    charged_bytes: usize,
}
impl Drop for EncodedUnit {
    fn drop(&mut self) {
        let mut state = lock(&self.shared.state);
        state.units -= 1;
        state.bytes -= self.charged_bytes;
        self.shared.wake.notify_one();
    }
}

pub struct Publisher {
    shared: Arc<Shared>,
    worker: Option<std::thread::JoinHandle<()>>,
    receiver: tokio::sync::Mutex<()>,
    runtime: tokio::runtime::Handle,
}
impl Publisher {
    /// Open/permission-start and native destruction never run on UI/IO threads.
    /// Returns after start REQUEST; stats/recv still await actual system consent.
    pub async fn open(config: PublisherConfig) -> Result<Self, PublisherError> {
        if config.backend == PublisherBackend::DxgiNvenc || config.output_index != 0 {
            return Err(PublisherError::BackendUnavailable);
        }
        if config.fps > 60 {
            return Err(PublisherError::ScreenSourceLimitedTo60);
        }
        if config.width <= 0
            || config.height <= 0
            || config.fps == 0
            || config.bitrate <= 0
            || config.max_queued_units == 0
            || config.max_queued_units > 64
            || config.max_queued_bytes == 0
            || config.max_queued_bytes > 64 * 1024 * 1024
        {
            return Err(PublisherError::InvalidConfig);
        }
        #[cfg(not(target_env = "ohos"))]
        {
            let _ = config;
            Err(PublisherError::UnsupportedPlatform)
        }
        #[cfg(target_env = "ohos")]
        {
            let shared = Arc::new(Shared::new(config));
            let mut opening = Opening {
                shared: shared.clone(),
                armed: true,
            };
            let result = tokio::task::spawn_blocking(move || native::open(config, shared))
                .await
                .map_err(|_| PublisherError::WorkerFailed)?;
            opening.armed = false;
            result
        }
    }
    /// Exactly one ordered consumer. Waiting for FIFO data AND consumer ownership
    /// is cancellable; no per-frame task, timeout polling or sleep retry.
    pub async fn recv(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Option<EncodedUnit>, PublisherError> {
        let _consumer = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(PublisherError::Closed),
            guard = self.receiver.lock() => guard,
        };
        loop {
            let changed = self.shared.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = lock(&self.shared.state);
                if let Some(error) = &state.failure {
                    return Err(error.clone());
                }
                if state.closing {
                    return Ok(None);
                }
                if let Some(unit) = state.queue.pop_front() {
                    return Ok(Some(EncodedUnit {
                        charged_bytes: unit.data.len(),
                        data: unit.data,
                        pts_us: unit.pts_us,
                        key: unit.key,
                        shared: self.shared.clone(),
                    }));
                }
            }
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(PublisherError::Closed),
                _ = &mut changed => {},
            }
        }
    }
    pub fn request_close(&self) {
        self.shared.request_close();
    }
    /// Latch a protocol refresh until capture has started. Repeated refreshes
    /// coalesce; they never flush or discard queued access units.
    pub fn request_keyframe(&self) -> Result<(), PublisherError> {
        let mut state = lock(&self.shared.state);
        if state.closing {
            return Err(PublisherError::Closed);
        }
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        state.keyframe_requested = true;
        self.shared.wake.notify_one();
        Ok(())
    }
    pub fn stats(&self) -> PublisherStats {
        self.shared.stats()
    }
    /// Abort capture and join teardown; Result ONLY confirms native reclamation.
    /// Ordinary capture/codec errors remain stats.error, not a false close error.
    pub async fn close(mut self) -> Result<(), PublisherError> {
        self.request_close();
        if let Some(worker) = self.worker.take() {
            self.runtime
                .spawn_blocking(move || worker.join())
                .await
                .map_err(|_| PublisherError::ReclamationUnconfirmed)?
                .map_err(|_| PublisherError::ReclamationUnconfirmed)?;
        }
        let state = lock(&self.shared.state);
        if state.closed && !state.quarantined {
            Ok(())
        } else {
            Err(PublisherError::ReclamationUnconfirmed)
        }
    }
}
impl Drop for Publisher {
    fn drop(&mut self) {
        self.shared.request_close();
        if let Some(worker) = self.worker.take() {
            // Retain/join the shutdown thread even if the receiver is dropped.
            // The application-wide runtime from open owns this blocking job;
            // dropping a caller's future does not abort spawn_blocking work.
            let shared = self.shared.clone();
            let reaper = self.runtime.spawn_blocking(move || {
                if worker.join().is_err() {
                    shared.fail(PublisherError::WorkerFailed);
                    shared.clear(true);
                }
            });
            *lock(&self.shared.reaper) = Some(reaper);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
const MAX_EVENTS: usize = 64;
const MAX_HEADER: usize = 256 * 1024;
struct Packet {
    data: Bytes,
    pts_us: i64,
    key: bool,
}
#[derive(Clone, Copy)]
struct Buffer {
    index: u32,
    address: usize,
}
struct State {
    queue: std::collections::VecDeque<Packet>,
    outputs: std::collections::VecDeque<Buffer>,
    owned: std::collections::HashSet<u32>,
    units: usize,
    bytes: usize,
    consent: bool,
    capture_state: i32,
    rate_set: bool,
    keyframe_requested: bool,
    closing: bool,
    closed: bool,
    quarantined: bool,
    failure: Option<PublisherError>,
    encoded_frames: u64,
    encoded_bytes: u64,
    encoder_name: String,
}
struct Shared {
    state: Mutex<State>,
    wake: std::sync::Condvar,
    changed: tokio::sync::Notify,
    config: PublisherConfig,
    reaper: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Shared {
    #[cfg(target_env = "ohos")]
    fn new(config: PublisherConfig) -> Self {
        Self {
            state: Mutex::new(State {
                queue: std::collections::VecDeque::with_capacity(config.max_queued_units),
                outputs: std::collections::VecDeque::with_capacity(MAX_EVENTS),
                owned: std::collections::HashSet::with_capacity(MAX_EVENTS),
                units: 0,
                bytes: 0,
                consent: false,
                capture_state: -1,
                rate_set: false,
                keyframe_requested: false,
                closing: false,
                closed: false,
                quarantined: false,
                failure: None,
                encoded_frames: 0,
                encoded_bytes: 0,
                encoder_name: String::new(),
            }),
            wake: std::sync::Condvar::new(),
            changed: tokio::sync::Notify::new(),
            config,
            reaper: Mutex::new(None),
        }
    }
    fn request_close(&self) {
        lock(&self.state).closing = true;
        self.wake.notify_one();
        self.changed.notify_waiters();
    }
    fn stats(&self) -> PublisherStats {
        let s = lock(&self.state);
        PublisherStats {
            consent_state: if s.quarantined {
                "quarantined"
            } else if s.closed {
                "stopped"
            } else if s.closing {
                "closing"
            } else if s.consent {
                "granted"
            } else {
                "awaiting_system_consent"
            }
            .into(),
            capture_state_code: s.capture_state,
            width: self.config.width,
            height: self.config.height,
            fps_limit: self.config.fps,
            encoder_name: s.encoder_name.clone(),
            encoded_frames: s.encoded_frames,
            encoded_bytes: s.encoded_bytes,
            queued_units: s.units,
            queued_bytes: s.bytes,
            closed: s.closed,
            quarantined: s.quarantined,
            error: s.failure.clone(),
        }
    }
    fn fail(&self, error: PublisherError) {
        let mut s = lock(&self.state);
        if s.failure.is_none() {
            s.failure = Some(error);
        }
        self.wake.notify_one();
        self.changed.notify_waiters();
    }
    fn clear(&self, quarantined: bool) {
        let mut s = lock(&self.state);
        s.closing = true;
        s.quarantined = quarantined;
        s.closed = !quarantined;
        // In-flight EncodedUnit guards keep their own credit until network send
        // releases them. Do not zero those counters and underflow a later Drop.
        while let Some(unit) = s.queue.pop_front() {
            s.units -= 1;
            s.bytes -= unit.data.len();
        }
        s.outputs.clear();
        s.owned.clear();
        self.changed.notify_waiters();
        self.wake.notify_one();
    }
}
#[cfg(target_env = "ohos")]
struct Opening {
    shared: Arc<Shared>,
    armed: bool,
}
#[cfg(target_env = "ohos")]
impl Drop for Opening {
    fn drop(&mut self) {
        if self.armed {
            self.shared.request_close();
        }
    }
}

#[cfg(target_env = "ohos")]
mod native {
    use super::*;
    use std::{
        ffi::{CStr, c_char, c_void},
        ptr,
        sync::{
            OnceLock,
            atomic::{AtomicUsize, Ordering},
        },
    };
    type Handle = *mut c_void;
    #[repr(C)]
    #[derive(Default)]
    struct Attr {
        pts: i64,
        size: i32,
        offset: i32,
        flags: u32,
    }
    #[repr(C)]
    struct Callbacks {
        error: unsafe extern "C" fn(Handle, i32, Handle),
        changed: unsafe extern "C" fn(Handle, Handle, Handle),
        input: unsafe extern "C" fn(Handle, u32, Handle, Handle),
        output: unsafe extern "C" fn(Handle, u32, Handle, Handle),
    }
    #[repr(C)]
    #[derive(Default)]
    struct AudioCapture {
        rate: i32,
        channels: i32,
        source: i32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct AudioEncoding {
        bitrate: i32,
        codec: i32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct AudioInfo {
        mic: AudioCapture,
        inner: AudioCapture,
        encoding: AudioEncoding,
    }
    #[repr(C)]
    struct VideoCapture {
        display: u64,
        missions: *mut i32,
        mission_count: i32,
        width: i32,
        height: i32,
        source: i32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct VideoEncoding {
        codec: i32,
        bitrate: i32,
        fps: i32,
    }
    #[repr(C)]
    struct VideoInfo {
        capture: VideoCapture,
        encoding: VideoEncoding,
    }
    #[repr(C)]
    struct Recorder {
        url: *mut c_char,
        len: u32,
        format: i32,
    }
    #[repr(C)]
    struct CaptureConfig {
        mode: i32,
        data_type: i32,
        audio: AudioInfo,
        video: VideoInfo,
        recorder: Recorder,
    }
    #[repr(C)]
    struct Rect {
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    }

    #[link(name = "native_media_codecbase")]
    unsafe extern "C" {
        fn OH_AVCodec_GetCapabilityByCategory(
            mime: *const c_char,
            encoder: bool,
            category: i32,
        ) -> Handle;
        fn OH_AVCapability_IsHardware(cap: Handle) -> bool;
        fn OH_AVCapability_GetName(cap: Handle) -> *const c_char;
        fn OH_AVCapability_GetSupportedProfiles(
            cap: Handle,
            values: *mut *const i32,
            count: *mut u32,
        ) -> i32;
        fn OH_AVCapability_GetVideoSupportedPixelFormats(
            cap: Handle,
            values: *mut *const i32,
            count: *mut u32,
        ) -> i32;
        fn OH_AVCapability_GetVideoSupportedNativeBufferFormats(
            cap: Handle,
            values: *mut *const i32,
            count: *mut u32,
        ) -> i32;
        static OH_MD_KEY_PIXEL_FORMAT: *const c_char;
        static OH_MD_KEY_BITRATE: *const c_char;
        static OH_MD_KEY_FRAME_RATE: *const c_char;
        static OH_MD_KEY_PROFILE: *const c_char;
        static OH_MD_KEY_VIDEO_ENCODE_BITRATE_MODE: *const c_char;
        static OH_MD_KEY_I_FRAME_INTERVAL: *const c_char;
        static OH_MD_KEY_VIDEO_ENCODER_MAX_B_FRAMES: *const c_char;
        static OH_MD_KEY_REQUEST_I_FRAME: *const c_char;
    }
    #[link(name = "native_media_core")]
    unsafe extern "C" {
        fn OH_AVFormat_CreateVideoFormat(mime: *const c_char, width: i32, height: i32) -> Handle;
        fn OH_AVFormat_Create() -> Handle;
        fn OH_AVFormat_Destroy(format: Handle);
        fn OH_AVFormat_SetIntValue(format: Handle, key: *const c_char, value: i32) -> bool;
        fn OH_AVFormat_SetLongValue(format: Handle, key: *const c_char, value: i64) -> bool;
        fn OH_AVFormat_SetDoubleValue(format: Handle, key: *const c_char, value: f64) -> bool;
        fn OH_AVBuffer_GetBufferAttr(buffer: Handle, attr: *mut Attr) -> i32;
        fn OH_AVBuffer_GetCapacity(buffer: Handle) -> i32;
        fn OH_AVBuffer_GetAddr(buffer: Handle) -> *mut u8;
    }
    #[link(name = "native_media_venc")]
    unsafe extern "C" {
        fn OH_VideoEncoder_CreateByName(name: *const c_char) -> Handle;
        fn OH_VideoEncoder_RegisterCallback(
            codec: Handle,
            callback: Callbacks,
            data: Handle,
        ) -> i32;
        fn OH_VideoEncoder_Configure(codec: Handle, format: Handle) -> i32;
        fn OH_VideoEncoder_GetSurface(codec: Handle, window: *mut Handle) -> i32;
        fn OH_VideoEncoder_Prepare(codec: Handle) -> i32;
        fn OH_VideoEncoder_Start(codec: Handle) -> i32;
        fn OH_VideoEncoder_Stop(codec: Handle) -> i32;
        fn OH_VideoEncoder_Destroy(codec: Handle) -> i32;
        fn OH_VideoEncoder_FreeOutputBuffer(codec: Handle, index: u32) -> i32;
        fn OH_VideoEncoder_SetParameter(codec: Handle, format: Handle) -> i32;
    }
    #[link(name = "native_window")]
    unsafe extern "C" {
        fn OH_NativeWindow_DestroyNativeWindow(window: Handle);
    }
    #[link(name = "native_display_manager")]
    unsafe extern "C" {
        fn OH_NativeDisplayManager_GetDefaultDisplayWidth(width: *mut i32) -> i32;
        fn OH_NativeDisplayManager_GetDefaultDisplayHeight(height: *mut i32) -> i32;
    }
    #[link(name = "native_avscreen_capture")]
    unsafe extern "C" {
        fn OH_AVScreenCapture_Create() -> Handle;
        fn OH_AVScreenCapture_Init(capture: Handle, config: CaptureConfig) -> i32;
        fn OH_AVScreenCapture_SetStateCallback(
            capture: Handle,
            callback: unsafe extern "C" fn(Handle, i32, Handle),
            data: Handle,
        ) -> i32;
        fn OH_AVScreenCapture_SetErrorCallback(
            capture: Handle,
            callback: unsafe extern "C" fn(Handle, i32, Handle),
            data: Handle,
        ) -> i32;
        fn OH_AVScreenCapture_SetCaptureContentChangedCallback(
            capture: Handle,
            callback: unsafe extern "C" fn(Handle, i32, *mut Rect, Handle),
            data: Handle,
        ) -> i32;
        fn OH_AVScreenCapture_SetMicrophoneEnabled(capture: Handle, enabled: bool) -> i32;
        fn OH_AVScreenCapture_StartScreenCaptureWithSurface(capture: Handle, window: Handle)
        -> i32;
        fn OH_AVScreenCapture_SetMaxVideoFrameRate(capture: Handle, fps: i32) -> i32;
        fn OH_AVScreenCapture_StopScreenCapture(capture: Handle) -> i32;
        fn OH_AVScreenCapture_Release(capture: Handle) -> i32;
    }
    fn check(api: &'static str, code: i32) -> Result<(), PublisherError> {
        if code == 0 {
            Ok(())
        } else {
            Err(PublisherError::Native { api, code })
        }
    }
    struct Context {
        shared: Arc<Shared>,
        active: AtomicUsize,
        idle: std::sync::Condvar,
        idle_lock: Mutex<()>,
    }
    impl Context {
        fn wait_idle(&self) {
            let mut guard = lock(&self.idle_lock);
            while self.active.load(Ordering::Acquire) != 0 {
                guard = self.idle.wait(guard).unwrap_or_else(|e| e.into_inner());
            }
        }
    }
    unsafe fn callback(data: Handle, f: impl FnOnce(&Shared)) {
        if data.is_null() {
            return;
        }
        let context = unsafe { &*(data as *const Context) };
        context.active.fetch_add(1, Ordering::AcqRel);
        f(&context.shared);
        let _guard = lock(&context.idle_lock);
        context.active.fetch_sub(1, Ordering::AcqRel);
        context.idle.notify_all();
    }
    unsafe extern "C" fn encoder_error(_: Handle, code: i32, data: Handle) {
        unsafe {
            callback(data, |s| {
                s.fail(PublisherError::Native {
                    api: "encoder.onError",
                    code,
                })
            })
        };
    }
    unsafe extern "C" fn capture_error(_: Handle, code: i32, data: Handle) {
        unsafe {
            callback(data, |s| {
                s.fail(PublisherError::Native {
                    api: "capture.onError",
                    code,
                })
            })
        };
    }
    unsafe extern "C" fn format_changed(_: Handle, _: Handle, _: Handle) { /* transient format never retained */
    }
    unsafe extern "C" fn input_unused(_: Handle, _: u32, _: Handle, _: Handle) { /* Surface input: no PushInputBuffer */
    }
    unsafe extern "C" fn output(_: Handle, index: u32, buffer: Handle, data: Handle) {
        unsafe {
            callback(data, |shared| {
                let mut s = lock(&shared.state);
                if s.closing || s.failure.is_some() {
                    return;
                }
                if s.owned.len() >= MAX_EVENTS {
                    s.failure = Some(PublisherError::CallbackOverflow);
                } else if !s.owned.insert(index) {
                    s.failure = Some(PublisherError::DuplicateOutput);
                } else {
                    s.outputs.push_back(Buffer {
                        index,
                        address: buffer as usize,
                    });
                }
                shared.wake.notify_one();
                shared.changed.notify_waiters();
            })
        };
    }
    unsafe extern "C" fn capture_state(_: Handle, code: i32, data: Handle) {
        unsafe {
            callback(data, |shared| {
                let mut s = lock(&shared.state);
                s.capture_state = code;
                if code == 0 && !s.closing {
                    s.consent = true;
                }
                if matches!(code, 1..=4 | 10) {
                    s.consent = false;
                    if !s.closing && s.failure.is_none() {
                        s.failure = Some(PublisherError::ConsentEnded { state: code });
                    }
                }
                shared.wake.notify_one();
                shared.changed.notify_waiters();
            })
        };
    }
    unsafe extern "C" fn content_changed(_: Handle, event: i32, area: *mut Rect, data: Handle) {
        unsafe {
            callback(data, |s| {
                if event == 2 {
                    s.fail(PublisherError::ContentUnavailable);
                }
                if !area.is_null() && event == 1 {
                    let r = &*area;
                    if r.width > 0
                        && r.height > 0
                        && (r.width != s.config.width || r.height != s.config.height)
                    {
                        s.fail(PublisherError::GeometryChanged);
                    }
                }
            })
        };
    }

    // Capacity reserved before either SDK instance is created. Quarantine is
    // bounded to TWO starting/live/retained publishers, and latches creation off.
    struct Gate {
        live: usize,
        retained: Vec<Allocation>,
    }
    fn gate() -> &'static Mutex<Gate> {
        static GATE: OnceLock<Mutex<Gate>> = OnceLock::new();
        GATE.get_or_init(|| {
            Mutex::new(Gate {
                live: 0,
                retained: Vec::with_capacity(2),
            })
        })
    }
    struct Permit;
    impl Permit {
        fn acquire() -> Result<Self, PublisherError> {
            let mut gate = lock(gate());
            if !gate.retained.is_empty() {
                return Err(PublisherError::QuarantinePresent);
            }
            if gate.live >= 2 {
                return Err(PublisherError::OwnerLimit);
            }
            gate.live += 1;
            Ok(Self)
        }
    }
    impl Drop for Permit {
        fn drop(&mut self) {
            lock(gate()).live -= 1;
        }
    }
    struct Allocation {
        encoder: usize,
        capture: usize,
        window: usize,
        encoder_started: bool,
        capture_attempted: bool,
        context: Box<Context>,
        _permit: Permit,
    }
    fn quarantine(allocation: Allocation) {
        allocation.context.shared.clear(true);
        lock(gate()).retained.push(allocation);
    }
    struct Owner(Option<Allocation>);
    impl Drop for Owner {
        fn drop(&mut self) {
            if let Some(allocation) = self.0.take() {
                quarantine(allocation);
            }
        }
    }
    impl Owner {
        fn shutdown(mut self) -> Result<(), PublisherError> {
            // Keep ownership INSIDE the unwind guard throughout native teardown.
            // A panic must not drop userdata while either SDK instance survives.
            let a = self.0.as_mut().unwrap();
            a.context.shared.request_close();
            // Producer must be RELEASED before destroying its encoder consumer
            // or NativeWindow. Stop success alone is NOT lifetime confirmation.
            if a.capture != 0 {
                if a.capture_attempted {
                    if let Err(e) = check("OH_AVScreenCapture_StopScreenCapture", unsafe {
                        OH_AVScreenCapture_StopScreenCapture(a.capture as Handle)
                    }) {
                        a.context.shared.fail(e);
                    }
                }
                if let Err(e) = check("OH_AVScreenCapture_Release", unsafe {
                    OH_AVScreenCapture_Release(a.capture as Handle)
                }) {
                    a.context.shared.fail(e);
                    return Err(PublisherError::ReclamationUnconfirmed);
                }
                a.capture = 0;
            }
            if a.encoder != 0 {
                if a.encoder_started {
                    if let Err(e) = check("OH_VideoEncoder_Stop", unsafe {
                        OH_VideoEncoder_Stop(a.encoder as Handle)
                    }) {
                        a.context.shared.fail(e);
                    }
                }
                if let Err(e) = check("OH_VideoEncoder_Destroy", unsafe {
                    OH_VideoEncoder_Destroy(a.encoder as Handle)
                }) {
                    a.context.shared.fail(e);
                    return Err(PublisherError::ReclamationUnconfirmed);
                }
                a.encoder = 0;
            }
            // Successful Release/Destroy stop future callbacks; also wait out
            // already-entered callbacks before freeing the shared userdata.
            a.context.wait_idle();
            if a.window != 0 {
                unsafe { OH_NativeWindow_DestroyNativeWindow(a.window as Handle) };
                a.window = 0;
            }
            a.context.shared.clear(false);
            drop(self.0.take());
            Ok(())
        }
    }
    pub(super) fn open(
        config: PublisherConfig,
        shared: Arc<Shared>,
    ) -> Result<Publisher, PublisherError> {
        let permit = Permit::acquire()?;
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let worker_shared = shared.clone();
        let worker = std::thread::Builder::new()
            .name("rd-ohos-publisher".into())
            .spawn(move || {
                let mut owner = Owner(Some(Allocation {
                    encoder: 0,
                    capture: 0,
                    window: 0,
                    encoder_started: false,
                    capture_attempted: false,
                    context: Box::new(Context {
                        shared: worker_shared.clone(),
                        active: AtomicUsize::new(0),
                        idle: std::sync::Condvar::new(),
                        idle_lock: Mutex::new(()),
                    }),
                    _permit: permit,
                }));
                if let Err(error) = setup(&mut owner, config) {
                    worker_shared.fail(error.clone());
                    let error = owner.shutdown().err().unwrap_or(error);
                    let _ = ready_tx.send(Err(error));
                    return;
                }
                if ready_tx.send(Ok(())).is_err() {
                    worker_shared.request_close();
                }
                if let Err(error) = pump(&owner, &worker_shared, config) {
                    worker_shared.fail(error);
                }
                let _ = owner.shutdown();
            })
            .map_err(|_| PublisherError::WorkerStartFailed)?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Publisher {
                shared,
                worker: Some(worker),
                receiver: tokio::sync::Mutex::new(()),
                runtime: tokio::runtime::Handle::current(),
            }),
            Ok(Err(error)) => {
                worker
                    .join()
                    .map_err(|_| PublisherError::ReclamationUnconfirmed)?;
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err(PublisherError::ReclamationUnconfirmed)
            }
        }
    }

    pub(super) fn hardware_name(codec: Codec) -> Result<std::ffi::CString, PublisherError> {
        static QUERY: Mutex<()> = Mutex::new(());
        let _guard = lock(&QUERY);
        let mime = match codec {
            Codec::H264 => c"video/avc",
            Codec::H265 => c"video/hevc",
        };
        let capability = unsafe { OH_AVCodec_GetCapabilityByCategory(mime.as_ptr(), true, 0) };
        if capability.is_null() || !unsafe { OH_AVCapability_IsHardware(capability) } {
            return Err(PublisherError::NoHardwareEncoder);
        }
        let name = unsafe { OH_AVCapability_GetName(capability) };
        if name.is_null() {
            return Err(PublisherError::NativeNull {
                api: "OH_AVCapability_GetName",
            });
        }
        let name = unsafe { CStr::from_ptr(name) }.to_owned();
        if name.as_bytes().is_empty() {
            return Err(PublisherError::NoHardwareEncoder);
        }
        fn contains(
            api: &'static str,
            cap: Handle,
            wanted: i32,
            query: unsafe extern "C" fn(Handle, *mut *const i32, *mut u32) -> i32,
        ) -> Result<bool, PublisherError> {
            let (mut values, mut count) = (ptr::null(), 0u32);
            check(api, unsafe { query(cap, &mut values, &mut count) })?;
            if count == 0 {
                return Ok(false);
            }
            if values.is_null() || !values.is_aligned() || count > 4096 {
                return Err(PublisherError::InvalidBuffer);
            }
            Ok(unsafe { std::slice::from_raw_parts(values, count as usize) }.contains(&wanted))
        }
        // buffer_common.h RGBA_8888=12; native_avformat.h RGBA=5.
        // Checking real native format support avoids advertising a merely
        // buffer-mode hardware encoder as this Surface producer/consumer path.
        if !contains(
            "OH_AVCapability_GetVideoSupportedNativeBufferFormats",
            capability,
            12,
            OH_AVCapability_GetVideoSupportedNativeBufferFormats,
        )? || !contains(
            "OH_AVCapability_GetVideoSupportedPixelFormats",
            capability,
            5,
            OH_AVCapability_GetVideoSupportedPixelFormats,
        )? || !contains(
            "OH_AVCapability_GetSupportedProfiles",
            capability,
            0,
            OH_AVCapability_GetSupportedProfiles,
        )? {
            return Err(PublisherError::SurfaceInputUnsupported);
        }
        Ok(name)
    }
    fn verify_geometry(config: PublisherConfig) -> Result<(), PublisherError> {
        // API12 whole-display px, not available-area/vp/widget dimensions. Refuse
        // a stale/downsized caller geometry rather than silently scale capture.
        let (mut width, mut height) = (0, 0);
        check("OH_NativeDisplayManager_GetDefaultDisplayWidth", unsafe {
            OH_NativeDisplayManager_GetDefaultDisplayWidth(&mut width)
        })?;
        check("OH_NativeDisplayManager_GetDefaultDisplayHeight", unsafe {
            OH_NativeDisplayManager_GetDefaultDisplayHeight(&mut height)
        })?;
        if width != config.width || height != config.height {
            return Err(PublisherError::GeometryChanged);
        }
        Ok(())
    }
    fn setup(owner: &mut Owner, config: PublisherConfig) -> Result<(), PublisherError> {
        let a = owner.0.as_mut().unwrap();
        verify_geometry(config)?;
        let mime = match config.codec {
            Codec::H264 => c"video/avc",
            Codec::H265 => c"video/hevc",
        };
        let name = hardware_name(config.codec)?;
        let encoder = unsafe { OH_VideoEncoder_CreateByName(name.as_ptr()) };
        a.encoder = encoder as usize;
        if encoder.is_null() {
            return Err(PublisherError::NativeNull {
                api: "OH_VideoEncoder_CreateByName",
            });
        }
        lock(&a.context.shared.state).encoder_name = name.to_string_lossy().into_owned();
        let data = &*a.context as *const Context as Handle;
        check("OH_VideoEncoder_RegisterCallback", unsafe {
            OH_VideoEncoder_RegisterCallback(
                encoder,
                Callbacks {
                    error: encoder_error,
                    changed: format_changed,
                    input: input_unused,
                    output,
                },
                data,
            )
        })?;
        let format =
            unsafe { OH_AVFormat_CreateVideoFormat(mime.as_ptr(), config.width, config.height) };
        if format.is_null() {
            return Err(PublisherError::NativeNull {
                api: "OH_AVFormat_CreateVideoFormat",
            });
        }
        let configured = unsafe {
            OH_AVFormat_SetIntValue(format, OH_MD_KEY_PIXEL_FORMAT, 4)
            && OH_AVFormat_SetLongValue(format, OH_MD_KEY_BITRATE, config.bitrate)
            && OH_AVFormat_SetDoubleValue(format, OH_MD_KEY_FRAME_RATE, config.fps as f64)
            && OH_AVFormat_SetIntValue(format, OH_MD_KEY_PROFILE, 0) // AVC Baseline / HEVC Main, not Main10.
            && OH_AVFormat_SetIntValue(format, OH_MD_KEY_VIDEO_ENCODE_BITRATE_MODE, 0)
            && OH_AVFormat_SetIntValue(format, OH_MD_KEY_I_FRAME_INTERVAL, 1000) // milliseconds, periodic IDR.
            && OH_AVFormat_SetIntValue(format, OH_MD_KEY_VIDEO_ENCODER_MAX_B_FRAMES, 0)
        };
        let result = if configured {
            check("OH_VideoEncoder_Configure", unsafe {
                OH_VideoEncoder_Configure(encoder, format)
            })
        } else {
            Err(PublisherError::InvalidConfig)
        };
        unsafe { OH_AVFormat_Destroy(format) };
        result?;
        let mut window = ptr::null_mut();
        let code = unsafe { OH_VideoEncoder_GetSurface(encoder, &mut window) };
        a.window = window as usize;
        check("OH_VideoEncoder_GetSurface", code)?;
        if window.is_null() {
            return Err(PublisherError::NativeNull {
                api: "OH_VideoEncoder_GetSurface",
            });
        }
        check("OH_VideoEncoder_Prepare", unsafe {
            OH_VideoEncoder_Prepare(encoder)
        })?;
        a.encoder_started = true;
        check("OH_VideoEncoder_Start", unsafe {
            OH_VideoEncoder_Start(encoder)
        })?;
        if lock(&a.context.shared.state).closing {
            return Err(PublisherError::Closed);
        }
        let capture = unsafe { OH_AVScreenCapture_Create() };
        a.capture = capture as usize;
        if capture.is_null() {
            return Err(PublisherError::NativeNull {
                api: "OH_AVScreenCapture_Create",
            });
        }
        check("OH_AVScreenCapture_SetStateCallback", unsafe {
            OH_AVScreenCapture_SetStateCallback(capture, capture_state, data)
        })?;
        check("OH_AVScreenCapture_SetErrorCallback", unsafe {
            OH_AVScreenCapture_SetErrorCallback(capture, capture_error, data)
        })?;
        check(
            "OH_AVScreenCapture_SetCaptureContentChangedCallback",
            unsafe {
                OH_AVScreenCapture_SetCaptureContentChangedCallback(capture, content_changed, data)
            },
        )?;
        // No microphone/internal audio: original-stream audio rates/channels=0.
        // Do not bind or call any raw video/audio buffer acquisition API.
        let config = CaptureConfig {
            mode: 0,
            data_type: 0,
            audio: AudioInfo::default(),
            video: VideoInfo {
                capture: VideoCapture {
                    display: 0,
                    missions: ptr::null_mut(),
                    mission_count: 0,
                    width: config.width,
                    height: config.height,
                    source: 2,
                },
                encoding: VideoEncoding::default(),
            },
            recorder: Recorder {
                url: ptr::null_mut(),
                len: 0,
                format: 0,
            },
        };
        check("OH_AVScreenCapture_Init", unsafe {
            OH_AVScreenCapture_Init(capture, config)
        })?;
        check("OH_AVScreenCapture_SetMicrophoneEnabled", unsafe {
            OH_AVScreenCapture_SetMicrophoneEnabled(capture, false)
        })?;
        if lock(&a.context.shared.state).closing {
            return Err(PublisherError::Closed);
        }
        a.capture_attempted = true;
        // The ONE producer/consumer connection. No intermediate Surface copy.
        check("OH_AVScreenCapture_StartScreenCaptureWithSurface", unsafe {
            OH_AVScreenCapture_StartScreenCaptureWithSurface(capture, window)
        })
    }

    struct Parameters {
        sets: [Option<Vec<u8>>; 3],
        // Preserve standalone CSD/SEI/AUD until a real picture arrives; metadata
        // is attached to that AU, never emitted/countable as a synthetic frame.
        pending: Vec<u8>,
    }
    impl Parameters {
        fn inspect(&mut self, codec: Codec, data: &[u8]) -> Result<(bool, bool), PublisherError> {
            fn start(data: &[u8], i: usize) -> usize {
                if data.get(i..i + 4) == Some(&[0, 0, 0, 1]) {
                    4
                } else if data.get(i..i + 3) == Some(&[0, 0, 1]) {
                    3
                } else {
                    0
                }
            }
            if start(data, 0) == 0 {
                return Err(PublisherError::InvalidAnnexB);
            }
            let (mut offset, mut vcl, mut key) = (0, false, false);
            while offset < data.len() {
                let prefix = start(data, offset);
                if prefix == 0 || offset + prefix >= data.len() {
                    return Err(PublisherError::InvalidAnnexB);
                }
                let header = offset + prefix;
                let mut end = header + 1;
                while end < data.len() && start(data, end) == 0 {
                    end += 1;
                }
                let kind = match codec {
                    Codec::H264 => data[header] & 31,
                    Codec::H265 => (data[header] >> 1) & 63,
                };
                if data[header] & 0x80 != 0
                    || (codec == Codec::H264 && kind == 0)
                    || (codec == Codec::H265 && (header + 1 >= end || data[header + 1] & 7 == 0))
                {
                    return Err(PublisherError::InvalidAnnexB);
                }
                let parameter = match codec {
                    Codec::H264 => {
                        vcl |= (1..=5).contains(&kind);
                        key |= kind == 5;
                        match kind {
                            7 => Some(1),
                            8 => Some(2),
                            _ => None,
                        }
                    }
                    Codec::H265 => {
                        vcl |= kind <= 31;
                        key |= (16..=23).contains(&kind);
                        match kind {
                            32 => Some(0),
                            33 => Some(1),
                            34 => Some(2),
                            _ => None,
                        }
                    }
                };
                if let Some(slot) = parameter {
                    if end - offset > MAX_HEADER / 3 {
                        return Err(PublisherError::OutputTooLarge);
                    }
                    self.sets[slot] = Some(data[offset..end].to_vec());
                }
                offset = end;
            }
            Ok((vcl, key))
        }
        fn length(&self, codec: Codec) -> Result<usize, PublisherError> {
            let start = if codec == Codec::H264 { 1 } else { 0 };
            self.sets[start..].iter().try_fold(0usize, |n, set| {
                set.as_ref()
                    .map(|v| n + v.len())
                    .ok_or(PublisherError::MissingParameterSets)
            })
        }
        fn prepend(&self, out: &mut Vec<u8>) {
            for set in self.sets.iter().flatten() {
                out.extend_from_slice(set);
            }
        }
    }
    fn free(a: &Allocation, shared: &Shared, index: u32) -> Result<(), PublisherError> {
        // Drop ownership before SDK call: synchronous reuse of index is legal.
        lock(&shared.state).owned.remove(&index);
        check("OH_VideoEncoder_FreeOutputBuffer", unsafe {
            OH_VideoEncoder_FreeOutputBuffer(a.encoder as Handle, index)
        })
    }
    fn request_keyframe(a: &Allocation) -> Result<(), PublisherError> {
        let format = unsafe { OH_AVFormat_Create() };
        if format.is_null() {
            return Err(PublisherError::NativeNull {
                api: "OH_AVFormat_Create",
            });
        }
        let configured = unsafe { OH_AVFormat_SetIntValue(format, OH_MD_KEY_REQUEST_I_FRAME, 1) };
        let result = if configured {
            check("OH_VideoEncoder_SetParameter", unsafe {
                OH_VideoEncoder_SetParameter(a.encoder as Handle, format)
            })
        } else {
            Err(PublisherError::InvalidConfig)
        };
        unsafe { OH_AVFormat_Destroy(format) };
        result
    }
    fn pump(owner: &Owner, shared: &Shared, config: PublisherConfig) -> Result<(), PublisherError> {
        let a = owner.0.as_ref().unwrap();
        let mut parameters = Parameters {
            sets: [None, None, None],
            pending: Vec::new(),
        };
        loop {
            let buffer = {
                let mut s = lock(&shared.state);
                loop {
                    if s.closing || s.failure.is_some() {
                        return Ok(());
                    }
                    if s.consent && !s.rate_set {
                        drop(s);
                        // Consent may have taken long enough for rotation/resize.
                        verify_geometry(config)?;
                        // API14 explicitly requires AFTER capture has started.
                        check("OH_AVScreenCapture_SetMaxVideoFrameRate", unsafe {
                            OH_AVScreenCapture_SetMaxVideoFrameRate(
                                a.capture as Handle,
                                config.fps as i32,
                            )
                        })?;
                        s = lock(&shared.state);
                        s.rate_set = true;
                    }
                    if s.closing || s.failure.is_some() {
                        return Ok(());
                    }
                    if s.consent && s.rate_set && s.keyframe_requested {
                        s.keyframe_requested = false;
                        drop(s);
                        request_keyframe(a)?;
                        s = lock(&shared.state);
                    }
                    if s.closing || s.failure.is_some() {
                        return Ok(());
                    }
                    if s.consent && s.rate_set {
                        if let Some(buffer) = s.outputs.pop_front() {
                            break buffer;
                        }
                    }
                    s = shared.wake.wait(s).unwrap_or_else(|e| e.into_inner());
                }
            };
            let pointer = buffer.address as Handle;
            if pointer.is_null() {
                return Err(PublisherError::InvalidBuffer);
            }
            let mut attr = Attr::default();
            check("OH_AVBuffer_GetBufferAttr", unsafe {
                OH_AVBuffer_GetBufferAttr(pointer, &mut attr)
            })?;
            if attr.offset < 0 || attr.size < 0 {
                return Err(PublisherError::InvalidBuffer);
            }
            let size = attr.size as usize;
            if size == 0 {
                free(a, shared, buffer.index)?;
                if attr.flags & 1 != 0 {
                    return Ok(());
                }
                if attr.flags & 8 != 0 {
                    continue;
                }
                return Err(PublisherError::InvalidBuffer);
            }
            if size > config.max_queued_bytes {
                return Err(PublisherError::OutputTooLarge);
            }
            let capacity = unsafe { OH_AVBuffer_GetCapacity(pointer) };
            if capacity < 0
                || (attr.offset as usize)
                    .checked_add(size)
                    .is_none_or(|end| end > capacity as usize)
            {
                return Err(PublisherError::InvalidBuffer);
            }
            // COMPRESSED bytes only; capture never calls GetAddr/Map/AcquireVideoBuffer.
            let address = unsafe { OH_AVBuffer_GetAddr(pointer) };
            if address.is_null() {
                return Err(PublisherError::InvalidBuffer);
            }
            let data =
                unsafe { std::slice::from_raw_parts(address.add(attr.offset as usize), size) };
            let (vcl, key) = parameters.inspect(config.codec, data)?;
            if !vcl {
                if attr.flags & 1 == 0 {
                    if size > MAX_HEADER.saturating_sub(parameters.pending.len()) {
                        return Err(PublisherError::OutputTooLarge);
                    }
                    parameters.pending.extend_from_slice(data);
                }
                free(a, shared, buffer.index)?; // codec config/SEI/EOS is not a frame.
                if attr.flags & 1 != 0 {
                    return Ok(());
                }
                continue;
            }
            if attr.flags & (4 | 16) != 0 {
                return Err(PublisherError::UnexpectedDiscard);
            }
            let header = if key {
                parameters.length(config.codec)?
            } else {
                0
            };
            let length = size
                .checked_add(header + parameters.pending.len())
                .ok_or(PublisherError::OutputTooLarge)?;
            if length > config.max_queued_bytes {
                return Err(PublisherError::OutputTooLarge);
            }
            {
                let mut s = lock(&shared.state);
                while s.units >= config.max_queued_units
                    || length > config.max_queued_bytes.saturating_sub(s.bytes)
                {
                    if s.closing || s.failure.is_some() {
                        return Ok(());
                    }
                    if s.keyframe_requested && s.consent && s.rate_set {
                        s.keyframe_requested = false;
                        drop(s);
                        request_keyframe(a)?;
                        s = lock(&shared.state);
                        continue;
                    }
                    // Retain this encoded native output loan. No dropped frame,
                    // flushing, copy fallback or TCP-backpressure frame thinning.
                    s = shared.wake.wait(s).unwrap_or_else(|e| e.into_inner());
                }
                if s.closing || s.failure.is_some() {
                    return Ok(());
                }
                s.units += 1;
                s.bytes += length; // includes copy/Free in progress
            }
            let mut owned = Vec::with_capacity(length);
            owned.extend_from_slice(&parameters.pending);
            if key {
                parameters.prepend(&mut owned);
            }
            owned.extend_from_slice(data);
            let freed = free(a, shared, buffer.index);
            let mut s = lock(&shared.state);
            if freed.is_err() || s.closing || s.failure.is_some() {
                s.units -= 1;
                s.bytes -= length;
                freed?;
                return Ok(());
            }
            s.queue.push_back(Packet {
                data: Bytes::from(owned),
                pts_us: attr.pts,
                key,
            });
            parameters.pending.clear();
            s.encoded_frames += 1;
            s.encoded_bytes += length as u64;
            shared.changed.notify_waiters();
            if attr.flags & 1 != 0 {
                return Ok(());
            }
        }
    }
}
