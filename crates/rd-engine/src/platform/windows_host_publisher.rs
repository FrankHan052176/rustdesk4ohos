//! Host-facing asynchronous adapter for Desktop Duplication + direct NVENC.
//! Pixel resources remain D3D11 textures; only compressed Annex-B bytes are queued.

use crate::{
    media_color::{ColorPrimaries, ColorRange, MatrixCoefficients, MediaColorContract},
    windows_native::{
        CaptureInfo, DesktopDuplicationCapture, DesktopDuplicationOpenQuarantine, NativeNvenc,
        NativeNvencOpenQuarantine, OutputInfo, enumerate_outputs,
    },
    windows_publisher::{
        CloseQuarantine, Codec as NativeCodec, DxgiFormat, ProducerConfig, ProducerError,
        WindowsPublisher as NativePublisher,
    },
};
use hbb_common::bytes::Bytes;
use std::{
    collections::VecDeque,
    num::NonZeroU32,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::JoinHandle,
};
use tokio::sync::Notify;
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
#[derive(Debug, Clone, Copy)]
pub struct SupportedCodecs {
    pub h264: bool,
    pub h265: bool,
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
    BackendUnavailable,
    OutputNotFound,
    CaptureFailed,
    EncoderFailed,
    InvalidAccessUnit,
    Closed,
    WorkerStartFailed,
    WorkerFailed,
    ReclamationUnconfirmed,
}
impl std::fmt::Display for PublisherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for PublisherError {}
impl PublisherError {
    pub fn resources_unconfirmed(&self) -> bool {
        matches!(self, Self::WorkerFailed | Self::ReclamationUnconfirmed)
    }
}

pub fn probe_display(
    backend: PublisherBackend,
    output_index: usize,
) -> Result<PublisherDisplay, PublisherError> {
    select_backend(backend)?;
    let output = output(output_index)?;
    let width = output.desktop_rect[2] - output.desktop_rect[0];
    let height = output.desktop_rect[3] - output.desktop_rect[1];
    if width <= 0 || height <= 0 {
        return Err(PublisherError::CaptureFailed);
    }
    Ok(PublisherDisplay {
        width,
        height,
        name: output.device_name,
    })
}
pub fn supported_codecs() -> Result<SupportedCodecs, PublisherError> {
    let display = probe_display(PublisherBackend::Auto, 0)?;
    supported_codecs_for(&PublisherConfig {
        codec: Codec::H264,
        width: display.width,
        height: display.height,
        fps: 60,
        bitrate: 20_000_000,
        max_queued_units: 1,
        max_queued_bytes: 32 * 1024 * 1024,
        backend: PublisherBackend::Auto,
        output_index: 0,
    })
}
pub fn supported_codecs_for(config: &PublisherConfig) -> Result<SupportedCodecs, PublisherError> {
    validate_config(config)?;
    select_backend(config.backend)?;
    let h264 = probe_codec(*config, Codec::H264);
    let h265 = probe_codec(*config, Codec::H265);
    if !h264 && !h265 {
        return Err(PublisherError::BackendUnavailable);
    }
    Ok(SupportedCodecs { h264, h265 })
}
fn probe_codec(mut config: PublisherConfig, codec: Codec) -> bool {
    config.codec = codec;
    let Ok(publisher) = open_native(config) else {
        return false;
    };
    close_native(publisher).is_ok()
}
#[derive(Clone, Copy)]
enum SelectedBackend {
    DxgiNvenc,
}
fn select_backend(backend: PublisherBackend) -> Result<SelectedBackend, PublisherError> {
    match backend {
        // Auto is a provider-selection boundary. At present it has one eligible
        // zero-copy provider and deliberately has no copy/software fallback.
        PublisherBackend::Auto => Ok(SelectedBackend::DxgiNvenc),
        PublisherBackend::DxgiNvenc => Ok(SelectedBackend::DxgiNvenc),
    }
}
fn output(index: usize) -> Result<OutputInfo, PublisherError> {
    enumerate_outputs()
        .map_err(map_producer)?
        .into_iter()
        .nth(index)
        .ok_or(PublisherError::OutputNotFound)
}
fn validate_config(config: &PublisherConfig) -> Result<(), PublisherError> {
    if config.width <= 0
        || config.height <= 0
        || config.fps == 0
        || config.fps > 240
        || config.bitrate <= 0
        || config.bitrate > 200_000_000
        || config.max_queued_units == 0
        || config.max_queued_units > 64
        || config.max_queued_bytes == 0
        || config.max_queued_bytes > 64 * 1024 * 1024
    {
        return Err(PublisherError::InvalidConfig);
    }
    Ok(())
}
fn encoded_color(source: MediaColorContract) -> Result<MediaColorContract, PublisherError> {
    let mut encoded = source;
    encoded.storage = match source.storage.bit_depth {
        8 => DxgiFormat::Nv12.storage(),
        10 => DxgiFormat::P010.storage(),
        _ => return Err(PublisherError::InvalidConfig),
    };
    encoded.colorimetry.range = ColorRange::Limited;
    encoded.colorimetry.matrix = match source.colorimetry.primaries {
        ColorPrimaries::Bt709 => MatrixCoefficients::Bt709,
        ColorPrimaries::Bt2020 => MatrixCoefficients::Bt2020NonConstantLuminance,
        _ => return Err(PublisherError::InvalidConfig),
    };
    Ok(encoded)
}
fn native_config(
    config: PublisherConfig,
    info: CaptureInfo,
) -> Result<ProducerConfig, PublisherError> {
    let width = NonZeroU32::new(config.width as u32).ok_or(PublisherError::InvalidConfig)?;
    let height = NonZeroU32::new(config.height as u32).ok_or(PublisherError::InvalidConfig)?;
    if width != info.descriptor.width || height != info.descriptor.height {
        return Err(PublisherError::InvalidConfig);
    }
    Ok(ProducerConfig {
        codec: match config.codec {
            Codec::H264 => NativeCodec::H264,
            Codec::H265 => NativeCodec::H265,
        },
        width,
        height,
        fps_numerator: NonZeroU32::new(config.fps).ok_or(PublisherError::InvalidConfig)?,
        fps_denominator: NonZeroU32::new(1).unwrap(),
        bitrate: NonZeroU32::new(config.bitrate as u32).ok_or(PublisherError::InvalidConfig)?,
        source_color: info.descriptor.source_color,
        encoded_color: encoded_color(info.descriptor.source_color)?,
    })
}
type Pipeline = NativePublisher<DesktopDuplicationCapture, NativeNvenc>;
type PipelineCloseQuarantine = CloseQuarantine<DesktopDuplicationCapture, NativeNvenc>;
enum OpenQuarantine {
    Capture(DesktopDuplicationOpenQuarantine),
    Encoder(NativeNvencOpenQuarantine),
}
impl OpenQuarantine {
    fn retry(self) -> Result<(), Self> {
        match self {
            Self::Capture(value) => value.retry().map_err(Self::Capture),
            Self::Encoder(value) => value.retry().map_err(Self::Encoder),
        }
    }
}
struct OpenFailure {
    error: PublisherError,
    quarantine: Option<OpenQuarantine>,
}
fn open_native(config: PublisherConfig) -> Result<Pipeline, OpenFailure> {
    select_backend(config.backend).map_err(OpenFailure::plain)?;
    let selected = output(config.output_index).map_err(OpenFailure::plain)?;
    let capture = DesktopDuplicationCapture::open(selected.id, 100).map_err(|error| {
        let mapped = if error.error() == ProducerError::ReclamationUnconfirmed {
            PublisherError::ReclamationUnconfirmed
        } else {
            PublisherError::CaptureFailed
        };
        OpenFailure {
            error: mapped,
            quarantine: error.into_quarantine().map(OpenQuarantine::Capture),
        }
    })?;
    let native = native_config(config, capture.capture_info()).map_err(OpenFailure::plain)?;
    let encoder = NativeNvenc::new(native, &capture).map_err(|error| {
        let mapped = if error.error() == ProducerError::ReclamationUnconfirmed {
            PublisherError::ReclamationUnconfirmed
        } else {
            PublisherError::EncoderFailed
        };
        OpenFailure {
            error: mapped,
            quarantine: error.into_quarantine().map(OpenQuarantine::Encoder),
        }
    })?;
    NativePublisher::open(capture, encoder, native)
        .map_err(map_producer)
        .map_err(OpenFailure::plain)
}
impl OpenFailure {
    fn plain(error: PublisherError) -> Self {
        Self {
            error,
            quarantine: None,
        }
    }
}
fn close_native(publisher: Pipeline) -> Result<(), PublisherError> {
    match publisher.close() {
        Ok(()) => Ok(()),
        Err(quarantine) => quarantine
            .retry()
            .map_err(|_| PublisherError::ReclamationUnconfirmed),
    }
}
fn map_producer(error: ProducerError) -> PublisherError {
    match error {
        ProducerError::ReclamationUnconfirmed => PublisherError::ReclamationUnconfirmed,
        ProducerError::Closed => PublisherError::Closed,
        ProducerError::InvalidAnnexB | ProducerError::EmptyAccessUnit => {
            PublisherError::InvalidAccessUnit
        }
        ProducerError::EncoderFailed => PublisherError::EncoderFailed,
        _ => PublisherError::CaptureFailed,
    }
}

struct QueuedUnit {
    data: Bytes,
    pts_us: i64,
    key: bool,
}
struct State {
    queue: VecDeque<QueuedUnit>,
    charged_units: usize,
    queued_bytes: usize,
    closing: bool,
    closed: bool,
    keyframe_requested: bool,
    failure: Option<PublisherError>,
    open_quarantine: Option<OpenQuarantine>,
    close_quarantine: Option<PipelineCloseQuarantine>,
}
struct Shared {
    state: Mutex<State>,
    changed: Notify,
    space: Condvar,
    max_units: usize,
    max_bytes: usize,
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
impl Shared {
    fn request_close(&self) {
        let mut state = lock(&self.state);
        state.closing = true;
        drop(state);
        self.space.notify_all();
        self.changed.notify_waiters();
    }
}
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
        state.charged_units = state.charged_units.saturating_sub(1);
        state.queued_bytes = state.queued_bytes.saturating_sub(self.charged_bytes);
        drop(state);
        self.shared.space.notify_one();
    }
}
pub struct Publisher {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    receiver: tokio::sync::Mutex<()>,
}
impl Publisher {
    pub async fn open(config: PublisherConfig) -> Result<Self, PublisherError> {
        validate_config(&config)?;
        select_backend(config.backend)?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                queue: VecDeque::new(),
                charged_units: 0,
                queued_bytes: 0,
                closing: false,
                closed: false,
                keyframe_requested: false,
                failure: None,
                open_quarantine: None,
                close_quarantine: None,
            }),
            changed: Notify::new(),
            space: Condvar::new(),
            max_units: config.max_queued_units,
            max_bytes: config.max_queued_bytes,
        });
        let worker_shared = Arc::clone(&shared);
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("rd-windows-publisher".into())
            .spawn(move || worker_main(config, worker_shared, started_tx))
            .map_err(|_| PublisherError::WorkerStartFailed)?;
        let started = tokio::task::spawn_blocking(move || started_rx.recv())
            .await
            .map_err(|_| PublisherError::WorkerFailed)?
            .map_err(|_| PublisherError::WorkerFailed)?;
        match started {
            Ok(()) => Ok(Self {
                shared,
                worker: Some(worker),
                receiver: tokio::sync::Mutex::new(()),
            }),
            Err(error) => {
                let _ = tokio::task::spawn_blocking(move || worker.join()).await;
                if error == PublisherError::ReclamationUnconfirmed
                    && lock(&shared.state).open_quarantine.is_some()
                {
                    Ok(Self {
                        shared,
                        worker: None,
                        receiver: tokio::sync::Mutex::new(()),
                    })
                } else {
                    Err(error)
                }
            }
        }
    }
    pub async fn recv(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Option<EncodedUnit>, PublisherError> {
        let _receiver = tokio::select! { biased;
            _ = cancel.cancelled() => return Err(PublisherError::Closed),
            guard = self.receiver.lock() => guard,
        };
        loop {
            let notified = self.shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut state = lock(&self.shared.state);
                if let Some(error) = &state.failure {
                    return Err(error.clone());
                }
                if let Some(unit) = state.queue.pop_front() {
                    return Ok(Some(EncodedUnit {
                        charged_bytes: unit.data.len(),
                        data: unit.data,
                        pts_us: unit.pts_us,
                        key: unit.key,
                        shared: Arc::clone(&self.shared),
                    }));
                }
                if state.closed {
                    return Ok(None);
                }
            }
            tokio::select! { biased;
                _ = cancel.cancelled() => return Err(PublisherError::Closed),
                _ = &mut notified => {}
            }
        }
    }
    pub fn request_keyframe(&self) -> Result<(), PublisherError> {
        let mut state = lock(&self.shared.state);
        if state.closing || state.closed {
            return Err(PublisherError::Closed);
        }
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        state.keyframe_requested = true;
        Ok(())
    }
    pub fn request_close(&self) {
        self.shared.request_close();
    }
    pub async fn close(mut self) -> Result<(), PublisherError> {
        self.request_close();
        if let Some(worker) = self.worker.take() {
            tokio::task::spawn_blocking(move || worker.join())
                .await
                .map_err(|_| PublisherError::ReclamationUnconfirmed)?
                .map_err(|_| PublisherError::ReclamationUnconfirmed)?;
        }
        let mut state = lock(&self.shared.state);
        if let Some(quarantine) = state.open_quarantine.take() {
            drop(state);
            match quarantine.retry() {
                Ok(()) => {
                    state = lock(&self.shared.state);
                    if state.failure == Some(PublisherError::ReclamationUnconfirmed) {
                        state.failure = None;
                    }
                }
                Err(quarantine) => {
                    state = lock(&self.shared.state);
                    state.open_quarantine = Some(quarantine);
                    return Err(PublisherError::ReclamationUnconfirmed);
                }
            }
        }
        if let Some(quarantine) = state.close_quarantine.take() {
            drop(state);
            match quarantine.retry() {
                Ok(()) => {
                    state = lock(&self.shared.state);
                    if state.failure == Some(PublisherError::ReclamationUnconfirmed) {
                        state.failure = None;
                    }
                }
                Err(quarantine) => {
                    state = lock(&self.shared.state);
                    state.close_quarantine = Some(quarantine);
                    return Err(PublisherError::ReclamationUnconfirmed);
                }
            }
        }
        if state.closed && state.failure.as_ref() != Some(&PublisherError::ReclamationUnconfirmed) {
            Ok(())
        } else {
            Err(PublisherError::ReclamationUnconfirmed)
        }
    }
}
impl Drop for Publisher {
    fn drop(&mut self) {
        self.request_close();
        // Dropping JoinHandle detaches without cancelling the worker. The shared
        // close flag wakes it, and its Arc keeps all native owners alive through teardown.
        let _ = self.worker.take();
    }
}
fn worker_main(
    config: PublisherConfig,
    shared: Arc<Shared>,
    started: std::sync::mpsc::SyncSender<Result<(), PublisherError>>,
) {
    let mut publisher = match open_native(config) {
        Ok(value) => {
            let _ = started.send(Ok(()));
            value
        }
        Err(failure) => {
            let _ = started.send(Err(failure.error.clone()));
            let mut state = lock(&shared.state);
            state.failure = Some(failure.error);
            state.open_quarantine = failure.quarantine;
            state.closed = true;
            drop(state);
            shared.changed.notify_waiters();
            return;
        }
    };
    let run_error = loop {
        let force_keyframe = {
            let mut state = lock(&shared.state);
            if state.closing {
                break None;
            }
            let requested = state.keyframe_requested;
            state.keyframe_requested = false;
            requested
        };
        if force_keyframe {
            if let Err(error) = publisher.request_keyframe() {
                break Some(map_producer(error));
            }
        }
        match publisher.next_unit() {
            Ok(Some(unit)) => {
                let bytes = unit.data.len();
                if bytes == 0 || bytes > shared.max_bytes {
                    break Some(PublisherError::InvalidAccessUnit);
                }
                let mut state = lock(&shared.state);
                while !state.closing
                    && (state.charged_units >= shared.max_units
                        || state.queued_bytes.saturating_add(bytes) > shared.max_bytes)
                {
                    state = shared.space.wait(state).unwrap_or_else(|e| e.into_inner());
                }
                if state.closing {
                    break None;
                }
                state.charged_units += 1;
                state.queued_bytes += bytes;
                state.queue.push_back(QueuedUnit {
                    data: unit.data,
                    pts_us: unit.pts_us,
                    key: unit.key,
                });
                drop(state);
                shared.changed.notify_one();
            }
            Ok(None) => {}
            Err(error) => break Some(map_producer(error)),
        }
    };
    let (close_error, close_quarantine) = match publisher.close() {
        Ok(()) => (None, None),
        Err(quarantine) => (
            Some(PublisherError::ReclamationUnconfirmed),
            Some(quarantine),
        ),
    };
    let mut state = lock(&shared.state);
    state.failure = close_error.or(run_error);
    state.close_quarantine = close_quarantine;
    state.closed = true;
    drop(state);
    shared.changed.notify_waiters();
}
