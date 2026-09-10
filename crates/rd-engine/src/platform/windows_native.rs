//! Native Desktop Duplication and NVENC bindings for the modern Windows producer.
//!
//! This module is compiled only for Windows with the default
//! `windows-modern-producer` feature. Runtime validation on representative NVIDIA
//! hardware, drivers, displays, SDR/HDR modes, and target frame rates is still
//! required; macOS host checks cannot establish Windows runtime support.

use crate::media_color::{
    ColorPrimaries, ColorRange, Colorimetry, HdrStaticMetadata, MatrixCoefficients,
    MediaColorContract, TransferCharacteristics,
};
use crate::windows_publisher::{
    AdapterLuid, CaptureApi, Codec, D3d11TextureLease, DirectNvenc, DxgiFormat, EncodedUnit,
    EncoderCapabilities, EncoderMode, ProducerConfig, ProducerError, TextureCapture,
    TextureDescriptor, validate_encoded_transport,
};
use hbb_common::bytes::Bytes;
use std::ffi::c_void;
use std::mem::{offset_of, size_of};
use std::num::NonZeroU32;
use std::ptr::{self, NonNull};
use std::sync::{Arc, Mutex, MutexGuard};

pub const NATIVE_BACKEND_IMPLEMENTED: bool = true;

const DD_OK: i32 = 0;
const DD_WOULD_BLOCK: i32 = 1;
const DD_ACCESS_LOST: i32 = 2;
const DD_GEOMETRY_CHANGED: i32 = 3;
const DD_UNSUPPORTED_FORMAT: i32 = 4;
const DD_LEASE_OUTSTANDING: i32 = 5;
const DD_RECLAMATION_UNCERTAIN: i32 = 6;
const DD_NOT_FOUND: i32 = 7;
const DD_INVALID_ARGUMENT: i32 = 8;
const DD_UNSUPPORTED: i32 = 9;
const DD_D3D_FAILURE: i32 = 10;
const DD_COLOR_CHANGED: i32 = 11;

const DD_FORMAT_BGRA8: u32 = 1;
const DD_FORMAT_RGB10A2: u32 = 2;

// DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709 and
// DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 respectively. Unknown or YCbCr
// values are never promoted to a known RGB transfer/primaries contract.
const DXGI_RGB_FULL_G22_P709: u32 = 0;
const DXGI_RGB_FULL_G2084_P2020: u32 = 12;

const NV_OK: i32 = 0;
const NV_INVALID_ARGUMENT: i32 = 1;
const NV_UNSUPPORTED: i32 = 2;
const NV_DLL_NOT_FOUND: i32 = 3;
const NV_DRIVER_TOO_OLD: i32 = 4;
const NV_ERROR: i32 = 5;
const NV_D3D11_ERROR: i32 = 6;
const NV_OUT_OF_MEMORY: i32 = 7;
const NV_BUSY: i32 = 8;
const NV_CLEANUP_FAILED: i32 = 9;
const NV_INTERNAL_ERROR: i32 = 10;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RdDdLuid {
    low_part: u32,
    high_part: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RdDdOutputId {
    adapter_luid: RdDdLuid,
    output_index: u32,
    reserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RdDdOutputInfo {
    id: RdDdOutputId,
    attached_to_desktop: u32,
    rotation: u32,
    desktop_left: i32,
    desktop_top: i32,
    desktop_right: i32,
    desktop_bottom: i32,
    device_name: [u16; 32],
    dxgi_color_space_type: u32,
    bits_per_color: u32,
    display_min_luminance_nits: f32,
    display_max_luminance_nits: f32,
    display_max_full_frame_luminance_nits: f32,
    reserved_color: u32,
}

impl Default for RdDdOutputInfo {
    fn default() -> Self {
        // All-zero is the documented initialization for this C output struct.
        unsafe { std::mem::zeroed() }
    }
}

#[repr(C)]
struct RdDdOpenOptions {
    struct_size: u32,
    flags: u32,
    output: RdDdOutputId,
}

#[repr(C)]
struct RdDdFrame {
    struct_size: u32,
    pixel_format: u32,
    width: u32,
    height: u32,
    adapter_luid: RdDdLuid,
    qpc_timestamp: u64,
    qpc_frequency: u64,
    qpc_is_dxgi_present_time: u32,
    accumulated_frames: u32,
    dxgi_color_space_type: u32,
    bits_per_color: u32,
    display_min_luminance_nits: f32,
    display_max_luminance_nits: f32,
    display_max_full_frame_luminance_nits: f32,
    reserved: u32,
    d3d11_texture2d: *mut c_void,
    d3d11_device: *mut c_void,
    lease: *mut RdDdLease,
}

impl Default for RdDdFrame {
    fn default() -> Self {
        let mut value: Self = unsafe { std::mem::zeroed() };
        value.struct_size = size_of::<Self>() as u32;
        value
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RdDdCaptureInfo {
    struct_size: u32,
    pixel_format: u32,
    width: u32,
    height: u32,
    adapter_luid: RdDdLuid,
    qpc_frequency: u64,
    dxgi_color_space_type: u32,
    bits_per_color: u32,
    display_min_luminance_nits: f32,
    display_max_luminance_nits: f32,
    display_max_full_frame_luminance_nits: f32,
    reserved: u32,
    d3d11_device: *mut c_void,
}

impl Default for RdDdCaptureInfo {
    fn default() -> Self {
        let mut value: Self = unsafe { std::mem::zeroed() };
        value.struct_size = size_of::<Self>() as u32;
        value
    }
}

enum RdDdCapture {}
enum RdDdLease {}
enum RdNvencEncoder {}

#[repr(C)]
#[derive(Default)]
struct RdNvencColorConfig {
    enabled: u32,
    video_full_range: u32,
    color_primaries: u32,
    transfer_characteristics: u32,
    matrix_coefficients: u32,
    video_format: u32,
}

#[repr(C)]
#[derive(Default)]
struct RdNvencHdrStaticMetadata {
    enabled: u32,
    display_primaries_x_gbr: [u16; 3],
    display_primaries_y_gbr: [u16; 3],
    white_point_x: u16,
    white_point_y: u16,
    max_display_mastering_luminance: u32,
    min_display_mastering_luminance: u32,
    max_content_light_level: u16,
    max_frame_average_light_level: u16,
}

#[repr(C)]
struct RdNvencCreateDesc {
    struct_size: u32,
    codec: u32,
    input_format: u32,
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
    bitrate_bps: u32,
    gop_length: u32,
    color: RdNvencColorConfig,
    hdr_static: RdNvencHdrStaticMetadata,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RdNvencOutputLoan {
    struct_size: u32,
    data: *const u8,
    size: u64,
    timestamp: u64,
    picture_type: u32,
    reserved: u32,
}

impl Default for RdNvencOutputLoan {
    fn default() -> Self {
        Self {
            struct_size: size_of::<Self>() as u32,
            data: ptr::null(),
            size: 0,
            timestamp: 0,
            picture_type: 0,
            reserved: 0,
        }
    }
}

const _: () = {
    assert!(size_of::<RdDdLuid>() == 8);
    assert!(size_of::<RdDdOutputId>() == 16);
    assert!(size_of::<RdDdOutputInfo>() == 128);
    assert!(offset_of!(RdDdOutputInfo, dxgi_color_space_type) == 104);
    assert!(offset_of!(RdDdOutputInfo, display_max_full_frame_luminance_nits) == 120);
    assert!(size_of::<RdDdOpenOptions>() == 24);
    assert!(offset_of!(RdDdFrame, adapter_luid) == 16);
    assert!(offset_of!(RdDdFrame, qpc_timestamp) == 24);
    assert!(offset_of!(RdDdFrame, accumulated_frames) == 44);
    assert!(offset_of!(RdDdFrame, dxgi_color_space_type) == 48);
    assert!(offset_of!(RdDdFrame, display_min_luminance_nits) == 56);
    assert!(offset_of!(RdDdFrame, d3d11_texture2d) == 72);
    assert!(offset_of!(RdDdCaptureInfo, adapter_luid) == 16);
    assert!(offset_of!(RdDdCaptureInfo, qpc_frequency) == 24);
    assert!(offset_of!(RdDdCaptureInfo, dxgi_color_space_type) == 32);
    assert!(offset_of!(RdDdCaptureInfo, d3d11_device) == 56);
    assert!(size_of::<RdNvencColorConfig>() == 24);
    assert!(size_of::<RdNvencHdrStaticMetadata>() == 32);
    assert!(size_of::<RdNvencCreateDesc>() == 92);
};

#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(size_of::<RdDdFrame>() == 96);
    assert!(size_of::<RdDdCaptureInfo>() == 64);
    assert!(size_of::<RdNvencOutputLoan>() == 40);
};

#[cfg(target_pointer_width = "32")]
const _: () = {
    assert!(size_of::<RdDdFrame>() == 88);
    assert!(size_of::<RdNvencOutputLoan>() == 32);
};

unsafe extern "C" {
    fn rd_dd_enumerate_outputs(outputs: *mut RdDdOutputInfo, capacity: u32, count: *mut u32)
    -> i32;
    fn rd_dd_open(options: *const RdDdOpenOptions, capture: *mut *mut RdDdCapture) -> i32;
    fn rd_dd_get_capture_info(capture: *mut RdDdCapture, info: *mut RdDdCaptureInfo) -> i32;
    fn rd_dd_acquire(capture: *mut RdDdCapture, timeout_ms: u32, frame: *mut RdDdFrame) -> i32;
    fn rd_dd_release(lease: *mut RdDdLease) -> i32;
    fn rd_dd_close(capture: *mut RdDdCapture) -> i32;

    fn rd_nvenc_create(
        d3d11_device: *mut c_void,
        desc: *const RdNvencCreateDesc,
        out_encoder: *mut *mut RdNvencEncoder,
    ) -> i32;
    fn rd_nvenc_encode_texture(
        encoder: *mut RdNvencEncoder,
        d3d11_texture: *mut c_void,
        timestamp: u64,
        force_idr_and_headers: u32,
        out_loan: *mut RdNvencOutputLoan,
    ) -> i32;
    fn rd_nvenc_release_output(encoder: *mut RdNvencEncoder, loan: *const RdNvencOutputLoan)
    -> i32;
    fn rd_nvenc_shutdown(encoder: *mut RdNvencEncoder) -> i32;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputId {
    pub adapter_luid: AdapterLuid,
    pub output_index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayDiagnostics {
    pub raw_dxgi_color_space_type: u32,
    pub bits_per_color: u32,
    pub min_luminance_nits: f32,
    pub max_luminance_nits: f32,
    pub max_full_frame_luminance_nits: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutputInfo {
    pub id: OutputId,
    pub rotation: u32,
    pub desktop_rect: [i32; 4],
    pub device_name: String,
    pub display: DisplayDiagnostics,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaptureInfo {
    pub descriptor: TextureDescriptor,
    pub qpc_frequency: u64,
    pub display: DisplayDiagnostics,
}

pub fn enumerate_outputs() -> Result<Vec<OutputInfo>, ProducerError> {
    let mut count = 0;
    map_dd(unsafe { rd_dd_enumerate_outputs(ptr::null_mut(), 0, &mut count) })?;
    loop {
        let mut raw = vec![RdDdOutputInfo::default(); count as usize];
        let mut required = count;
        map_dd(unsafe { rd_dd_enumerate_outputs(raw.as_mut_ptr(), count, &mut required) })?;
        if required > count {
            count = required;
            continue;
        }
        raw.truncate(required as usize);
        return Ok(raw.into_iter().map(output_info).collect());
    }
}

fn output_info(raw: RdDdOutputInfo) -> OutputInfo {
    let name_len = raw
        .device_name
        .iter()
        .position(|&value| value == 0)
        .unwrap_or(raw.device_name.len());
    OutputInfo {
        id: OutputId {
            adapter_luid: luid(raw.id.adapter_luid),
            output_index: raw.id.output_index,
        },
        rotation: raw.rotation,
        desktop_rect: [
            raw.desktop_left,
            raw.desktop_top,
            raw.desktop_right,
            raw.desktop_bottom,
        ],
        device_name: String::from_utf16_lossy(&raw.device_name[..name_len]),
        display: DisplayDiagnostics {
            raw_dxgi_color_space_type: raw.dxgi_color_space_type,
            bits_per_color: raw.bits_per_color,
            min_luminance_nits: raw.display_min_luminance_nits,
            max_luminance_nits: raw.display_max_luminance_nits,
            max_full_frame_luminance_nits: raw.display_max_full_frame_luminance_nits,
        },
    }
}

fn capture_info(raw: RdDdCaptureInfo) -> Result<CaptureInfo, ProducerError> {
    if raw.qpc_frequency == 0 || raw.reserved != 0 {
        return Err(ProducerError::InvalidTimestamp);
    }
    let format = native_format(raw.pixel_format)?;
    let width = NonZeroU32::new(raw.width).ok_or(ProducerError::SourceResolutionChanged)?;
    let height = NonZeroU32::new(raw.height).ok_or(ProducerError::SourceResolutionChanged)?;
    let source_color = source_color(format, raw.dxgi_color_space_type, raw.bits_per_color)?;
    Ok(CaptureInfo {
        descriptor: TextureDescriptor {
            width,
            height,
            format,
            array_slice: 0,
            mip_level: 0,
            adapter_luid: luid(raw.adapter_luid),
            source_color,
        },
        qpc_frequency: raw.qpc_frequency,
        display: DisplayDiagnostics {
            raw_dxgi_color_space_type: raw.dxgi_color_space_type,
            bits_per_color: raw.bits_per_color,
            min_luminance_nits: raw.display_min_luminance_nits,
            max_luminance_nits: raw.display_max_luminance_nits,
            max_full_frame_luminance_nits: raw.display_max_full_frame_luminance_nits,
        },
    })
}

fn native_format(pixel_format: u32) -> Result<DxgiFormat, ProducerError> {
    match pixel_format {
        DD_FORMAT_BGRA8 => Ok(DxgiFormat::B8G8R8A8Unorm),
        DD_FORMAT_RGB10A2 => Ok(DxgiFormat::R10G10B10A2Unorm),
        _ => Err(ProducerError::SourceFormatChanged),
    }
}

struct CaptureOwnerState {
    capture: Option<NonNull<RdDdCapture>>,
    orphaned_lease: Option<NonNull<RdDdLease>>,
}

unsafe impl Send for CaptureOwnerState {}

struct CaptureOwner {
    state: Mutex<CaptureOwnerState>,
}

impl CaptureOwner {
    fn new(capture: NonNull<RdDdCapture>) -> Self {
        Self {
            state: Mutex::new(CaptureOwnerState {
                capture: Some(capture),
                orphaned_lease: None,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, CaptureOwnerState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn retry_orphaned_locked(state: &mut CaptureOwnerState) -> Result<(), ProducerError> {
        let Some(lease) = state.orphaned_lease else {
            return Ok(());
        };
        let status = unsafe { rd_dd_release(lease.as_ptr()) };
        if status == DD_OK {
            state.orphaned_lease = None;
            Ok(())
        } else {
            Err(map_dd_status(status))
        }
    }

    fn retry_reclamation(&self) -> Result<(), ProducerError> {
        let mut state = self.lock();
        Self::retry_orphaned_locked(&mut state)
    }

    fn acquire(&self, timeout_ms: u32, frame: &mut RdDdFrame) -> Result<i32, ProducerError> {
        let mut state = self.lock();
        Self::retry_orphaned_locked(&mut state)?;
        let capture = state.capture.ok_or(ProducerError::Closed)?;
        Ok(unsafe { rd_dd_acquire(capture.as_ptr(), timeout_ms, frame) })
    }

    fn release_lease(&self, lease: NonNull<RdDdLease>) -> LeaseReleaseResult {
        let mut state = self.lock();
        if state.orphaned_lease.is_some() {
            return LeaseReleaseResult::TransferredToOwner(ProducerError::ReclamationUnconfirmed);
        }
        let status = unsafe { rd_dd_release(lease.as_ptr()) };
        if status == DD_OK {
            LeaseReleaseResult::Released
        } else {
            // The current C contract reports terminal graph loss as OK after
            // consuming the lease. Therefore every non-OK result is treated as
            // ambiguous here: retain the exact pointer and clear only after a
            // later retry returns OK.
            state.orphaned_lease = Some(lease);
            LeaseReleaseResult::TransferredToOwner(map_dd_status(status))
        }
    }

    fn close(&self) -> Result<(), ProducerError> {
        let mut state = self.lock();
        Self::retry_orphaned_locked(&mut state)?;
        let capture = state.capture.ok_or(ProducerError::Closed)?;
        let status = unsafe { rd_dd_close(capture.as_ptr()) };
        if status == DD_OK {
            state.capture = None;
            Ok(())
        } else {
            Err(map_dd_status(status))
        }
    }
}

impl Drop for CaptureOwner {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if Self::retry_orphaned_locked(state).is_err() {
            return;
        }
        if let Some(capture) = state.capture {
            if unsafe { rd_dd_close(capture.as_ptr()) } == DD_OK {
                state.capture = None;
            }
        }
    }
}

enum LeaseReleaseResult {
    Released,
    TransferredToOwner(ProducerError),
}

pub struct DesktopDuplicationCapture {
    // Shared with every emitted frame so neither the capture pointer nor an
    // ambiguous exact lease can outlive its Rust owner state.
    owner: Arc<CaptureOwner>,
    device: NonNull<c_void>,
    info: CaptureInfo,
    first_emitted_qpc: Option<u64>,
    timeout_ms: u32,
}

unsafe impl Send for DesktopDuplicationCapture {}

impl DesktopDuplicationCapture {
    pub fn open(output: OutputId, timeout_ms: u32) -> Result<Self, DesktopDuplicationOpenError> {
        let options = RdDdOpenOptions {
            struct_size: size_of::<RdDdOpenOptions>() as u32,
            flags: 0,
            output: RdDdOutputId {
                adapter_luid: raw_luid(output.adapter_luid),
                output_index: output.output_index,
                reserved: 0,
            },
        };
        let mut raw = ptr::null_mut();
        map_dd(unsafe { rd_dd_open(&options, &mut raw) })
            .map_err(DesktopDuplicationOpenError::plain)?;
        let raw = NonNull::new(raw)
            .ok_or_else(|| DesktopDuplicationOpenError::plain(ProducerError::CaptureFailed))?;
        let mut native_info = RdDdCaptureInfo::default();
        if let Err(error) =
            map_dd(unsafe { rd_dd_get_capture_info(raw.as_ptr(), &mut native_info) })
        {
            return Err(DesktopDuplicationOpenError::after_open(raw, error));
        }
        let info = match capture_info(native_info) {
            Ok(info) => info,
            Err(error) => {
                return Err(DesktopDuplicationOpenError::after_open(raw, error));
            }
        };
        let device = match NonNull::new(native_info.d3d11_device) {
            Some(device) => device,
            None => {
                return Err(DesktopDuplicationOpenError::after_open(
                    raw,
                    ProducerError::NullD3dResource,
                ));
            }
        };
        Ok(Self {
            owner: Arc::new(CaptureOwner::new(raw)),
            device,
            info,
            first_emitted_qpc: None,
            timeout_ms,
        })
    }

    pub fn capture_info(&self) -> CaptureInfo {
        self.info
    }

    pub fn retry_reclamation(&self) -> Result<(), ProducerError> {
        self.owner.retry_reclamation()
    }
}

pub struct DesktopDuplicationOpenError {
    error: ProducerError,
    validation_error: ProducerError,
    quarantine: Option<DesktopDuplicationOpenQuarantine>,
}

impl DesktopDuplicationOpenError {
    fn plain(error: ProducerError) -> Self {
        Self {
            error,
            validation_error: error,
            quarantine: None,
        }
    }

    fn after_open(raw: NonNull<RdDdCapture>, validation_error: ProducerError) -> Self {
        if unsafe { rd_dd_close(raw.as_ptr()) } == DD_OK {
            Self::plain(validation_error)
        } else {
            Self {
                error: ProducerError::ReclamationUnconfirmed,
                validation_error,
                quarantine: Some(DesktopDuplicationOpenQuarantine { raw: Some(raw) }),
            }
        }
    }

    pub fn error(&self) -> ProducerError {
        self.error
    }

    pub fn validation_error(&self) -> ProducerError {
        self.validation_error
    }

    pub fn into_quarantine(self) -> Option<DesktopDuplicationOpenQuarantine> {
        self.quarantine
    }
}

impl std::fmt::Debug for DesktopDuplicationOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopDuplicationOpenError")
            .field("error", &self.error)
            .field("validation_error", &self.validation_error)
            .field("has_quarantine", &self.quarantine.is_some())
            .finish()
    }
}

impl std::fmt::Display for DesktopDuplicationOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for DesktopDuplicationOpenError {}

pub struct DesktopDuplicationOpenQuarantine {
    raw: Option<NonNull<RdDdCapture>>,
}

unsafe impl Send for DesktopDuplicationOpenQuarantine {}

impl DesktopDuplicationOpenQuarantine {
    pub fn retry(mut self) -> Result<(), Self> {
        let Some(raw) = self.raw else {
            return Ok(());
        };
        if unsafe { rd_dd_close(raw.as_ptr()) } != DD_OK {
            return Err(self);
        }
        self.raw = None;
        Ok(())
    }
}

impl std::fmt::Debug for DesktopDuplicationOpenQuarantine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopDuplicationOpenQuarantine")
            .field("retained", &self.raw.is_some())
            .finish()
    }
}

impl Drop for DesktopDuplicationOpenQuarantine {
    fn drop(&mut self) {
        if let Some(raw) = self.raw {
            if unsafe { rd_dd_close(raw.as_ptr()) } == DD_OK {
                self.raw = None;
            }
        }
    }
}

impl TextureCapture for DesktopDuplicationCapture {
    type Frame = NativeFrame;

    fn api(&self) -> CaptureApi {
        CaptureApi::DesktopDuplication
    }

    fn next_texture(&mut self) -> Result<Option<Self::Frame>, ProducerError> {
        let mut raw = RdDdFrame::default();
        let status = self.owner.acquire(self.timeout_ms, &mut raw)?;
        if status == DD_WOULD_BLOCK {
            return Ok(None);
        }
        map_dd(status)?;
        let qpc = raw.qpc_timestamp;
        let frame = NativeFrame::from_raw(
            raw,
            self.info,
            self.device,
            self.first_emitted_qpc,
            Arc::clone(&self.owner),
        )?;
        if self.first_emitted_qpc.is_none() {
            self.first_emitted_qpc = Some(qpc);
        }
        Ok(Some(frame))
    }

    fn close(&mut self) -> Result<(), ProducerError> {
        self.owner.close()
    }
}

pub struct NativeFrame {
    // Keeps the native capture allocation and orphan-reclamation slot alive
    // even when DesktopDuplicationCapture is dropped before this frame.
    owner: Arc<CaptureOwner>,
    texture: NonNull<c_void>,
    device: NonNull<c_void>,
    lease: Option<NonNull<RdDdLease>>,
    descriptor: TextureDescriptor,
    pts_us: i64,
    accumulated_frames: u32,
    display: DisplayDiagnostics,
}

unsafe impl Send for NativeFrame {}

impl NativeFrame {
    fn from_raw(
        raw: RdDdFrame,
        expected: CaptureInfo,
        expected_device: NonNull<c_void>,
        first_emitted_qpc: Option<u64>,
        owner: Arc<CaptureOwner>,
    ) -> Result<Self, ProducerError> {
        if raw.qpc_is_dxgi_present_time != 1 || raw.qpc_frequency == 0 {
            retain_failed_frame(raw, &owner);
            return Err(ProducerError::InvalidTimestamp);
        }
        let format = match native_format(raw.pixel_format) {
            Ok(format) => format,
            Err(error) => {
                retain_failed_frame(raw, &owner);
                return Err(error);
            }
        };
        let Some(width) = NonZeroU32::new(raw.width) else {
            retain_failed_frame(raw, &owner);
            return Err(ProducerError::SourceResolutionChanged);
        };
        let Some(height) = NonZeroU32::new(raw.height) else {
            retain_failed_frame(raw, &owner);
            return Err(ProducerError::SourceResolutionChanged);
        };
        let prepared = (|| {
            let source_color = source_color(format, raw.dxgi_color_space_type, raw.bits_per_color)?;
            if raw.qpc_frequency != expected.qpc_frequency
                || raw.width != expected.descriptor.width.get()
                || raw.height != expected.descriptor.height.get()
                || format != expected.descriptor.format
                || luid(raw.adapter_luid) != expected.descriptor.adapter_luid
                || source_color != expected.descriptor.source_color
                || raw.dxgi_color_space_type != expected.display.raw_dxgi_color_space_type
                || raw.bits_per_color != expected.display.bits_per_color
            {
                return Err(ProducerError::ColorContractChanged);
            }
            let base = first_emitted_qpc.unwrap_or(raw.qpc_timestamp);
            let relative_qpc = raw
                .qpc_timestamp
                .checked_sub(base)
                .ok_or(ProducerError::InvalidTimestamp)?;
            let ticks = u128::from(relative_qpc)
                .checked_mul(1_000_000)
                .ok_or(ProducerError::InvalidTimestamp)?;
            let pts = ticks / u128::from(raw.qpc_frequency);
            let pts_us = i64::try_from(pts).map_err(|_| ProducerError::InvalidTimestamp)?;
            let texture =
                NonNull::new(raw.d3d11_texture2d).ok_or(ProducerError::NullD3dResource)?;
            let device = NonNull::new(raw.d3d11_device).ok_or(ProducerError::NullD3dResource)?;
            if device != expected_device {
                return Err(ProducerError::AdapterMismatch);
            }
            let lease = NonNull::new(raw.lease).ok_or(ProducerError::NullD3dResource)?;
            Ok::<_, ProducerError>((source_color, pts_us, texture, device, lease))
        })();
        let (source_color, pts_us, texture, device, lease) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                retain_failed_frame(raw, &owner);
                return Err(error);
            }
        };
        Ok(Self {
            owner,
            texture,
            device,
            lease: Some(lease),
            descriptor: TextureDescriptor {
                width,
                height,
                format,
                array_slice: 0,
                mip_level: 0,
                adapter_luid: luid(raw.adapter_luid),
                source_color,
            },
            pts_us,
            accumulated_frames: raw.accumulated_frames,
            display: DisplayDiagnostics {
                raw_dxgi_color_space_type: raw.dxgi_color_space_type,
                bits_per_color: raw.bits_per_color,
                min_luminance_nits: raw.display_min_luminance_nits,
                max_luminance_nits: raw.display_max_luminance_nits,
                max_full_frame_luminance_nits: raw.display_max_full_frame_luminance_nits,
            },
        })
    }

    pub fn accumulated_frames(&self) -> u32 {
        self.accumulated_frames
    }

    pub fn display_diagnostics(&self) -> DisplayDiagnostics {
        self.display
    }

    fn release(&mut self) -> Result<(), ProducerError> {
        let lease = self.lease.ok_or(ProducerError::Closed)?;
        match self.owner.release_lease(lease) {
            LeaseReleaseResult::Released => {
                self.lease = None;
                Ok(())
            }
            LeaseReleaseResult::TransferredToOwner(error) => {
                self.lease = None;
                Err(error)
            }
        }
    }
}

impl Drop for NativeFrame {
    fn drop(&mut self) {
        if let Some(lease) = self.lease {
            match self.owner.release_lease(lease) {
                LeaseReleaseResult::Released | LeaseReleaseResult::TransferredToOwner(_) => {
                    self.lease = None;
                }
            }
        }
    }
}

unsafe impl D3d11TextureLease for NativeFrame {
    fn texture_ptr(&self) -> *mut c_void {
        self.texture.as_ptr()
    }

    fn device_ptr(&self) -> *mut c_void {
        self.device.as_ptr()
    }

    fn descriptor(&self) -> TextureDescriptor {
        self.descriptor
    }

    fn capture_pts_us(&self) -> i64 {
        self.pts_us
    }
}

pub struct NativeNvenc {
    raw: Option<NonNull<RdNvencEncoder>>,
    capability: EncoderCapabilities,
    config: ProducerConfig,
    pending_output: Option<RdNvencOutputLoan>,
    quarantined_frame: Option<NativeFrame>,
    frame_requires_shutdown: bool,
}

unsafe impl Send for NativeNvenc {}

impl NativeNvenc {
    pub fn new(
        config: ProducerConfig,
        capture: &DesktopDuplicationCapture,
    ) -> Result<Self, NativeNvencOpenError> {
        let descriptor = capture.info.descriptor;
        if descriptor.width != config.width
            || descriptor.height != config.height
            || descriptor.source_color != config.source_color
        {
            return Err(NativeNvencOpenError::plain(ProducerError::InvalidConfig));
        }
        // NVENC may convert matrix/range/storage/subsampling, but this path has
        // no gamut, transfer-function, or HDR-content-metadata conversion.
        if config.source_color.colorimetry.primaries != config.encoded_color.colorimetry.primaries
            || config.source_color.colorimetry.transfer != config.encoded_color.colorimetry.transfer
            || config.source_color.hdr_static != config.encoded_color.hdr_static
            || config.source_color.storage.bit_depth != config.encoded_color.storage.bit_depth
        {
            return Err(NativeNvencOpenError::plain(
                ProducerError::ColorContractChanged,
            ));
        }
        validate_encoded_transport(config.encoded_color).map_err(NativeNvencOpenError::plain)?;
        let create = RdNvencCreateDesc {
            struct_size: size_of::<RdNvencCreateDesc>() as u32,
            codec: match config.codec {
                Codec::H264 => 1,
                Codec::H265 => 2,
            },
            input_format: nv_input_format(descriptor.format)
                .map_err(NativeNvencOpenError::plain)?,
            width: config.width.get(),
            height: config.height.get(),
            fps_num: config.fps_numerator.get(),
            fps_den: config.fps_denominator.get(),
            bitrate_bps: config.bitrate.get(),
            gop_length: config
                .fps_numerator
                .get()
                .div_ceil(config.fps_denominator.get()),
            color: vui(config.encoded_color).map_err(NativeNvencOpenError::plain)?,
            hdr_static: hdr_static(config.encoded_color.hdr_static)
                .map_err(NativeNvencOpenError::plain)?,
        };
        let mut raw = ptr::null_mut();
        let status = unsafe { rd_nvenc_create(capture.device.as_ptr(), &create, &mut raw) };
        if status != NV_OK {
            if status == NV_CLEANUP_FAILED {
                return Err(NativeNvencOpenError {
                    error: ProducerError::ReclamationUnconfirmed,
                    quarantine: NonNull::new(raw)
                        .map(|raw| NativeNvencOpenQuarantine { raw: Some(raw) }),
                });
            }
            return Err(NativeNvencOpenError::plain(map_nv(status)));
        }
        Ok(Self {
            raw: Some(
                NonNull::new(raw)
                    .ok_or_else(|| NativeNvencOpenError::plain(ProducerError::EncoderFailed))?,
            ),
            capability: capability(config, descriptor),
            config,
            pending_output: None,
            quarantined_frame: None,
            frame_requires_shutdown: false,
        })
    }

    /// Retries a staged output-loan release. The capture frame remains owned by
    /// this encoder until unlock, unmap, and unregister all succeed.
    pub fn retry_output_reclamation(&mut self) -> Result<(), ProducerError> {
        let Some(loan) = self.pending_output else {
            if self.frame_requires_shutdown {
                return Err(ProducerError::ReclamationUnconfirmed);
            }
            if let Some(frame) = self.quarantined_frame.take() {
                return release_frame_or_restore(&mut self.quarantined_frame, frame);
            }
            return Ok(());
        };
        let encoder = self.raw.ok_or(ProducerError::Closed)?;
        let status = unsafe { rd_nvenc_release_output(encoder.as_ptr(), &loan) };
        if status != NV_OK {
            return Err(map_nv(status));
        }
        self.pending_output = None;
        let frame = self
            .quarantined_frame
            .take()
            .ok_or(ProducerError::ReclamationUnconfirmed)?;
        let result = release_frame_or_restore(&mut self.quarantined_frame, frame);
        if result.is_ok() {
            self.frame_requires_shutdown = false;
        }
        result
    }
}

pub struct NativeNvencOpenError {
    error: ProducerError,
    quarantine: Option<NativeNvencOpenQuarantine>,
}

impl std::fmt::Debug for NativeNvencOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeNvencOpenError")
            .field("error", &self.error)
            .field("has_quarantine", &self.quarantine.is_some())
            .finish()
    }
}

impl std::fmt::Display for NativeNvencOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for NativeNvencOpenError {}

impl NativeNvencOpenError {
    fn plain(error: ProducerError) -> Self {
        Self {
            error,
            quarantine: None,
        }
    }

    pub fn error(&self) -> ProducerError {
        self.error
    }

    pub fn into_quarantine(self) -> Option<NativeNvencOpenQuarantine> {
        self.quarantine
    }
}

pub struct NativeNvencOpenQuarantine {
    raw: Option<NonNull<RdNvencEncoder>>,
}

impl std::fmt::Debug for NativeNvencOpenQuarantine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeNvencOpenQuarantine")
            .field("retained", &self.raw.is_some())
            .finish()
    }
}

unsafe impl Send for NativeNvencOpenQuarantine {}

impl NativeNvencOpenQuarantine {
    pub fn retry(mut self) -> Result<(), Self> {
        let Some(raw) = self.raw else {
            return Ok(());
        };
        if unsafe { rd_nvenc_shutdown(raw.as_ptr()) } != NV_OK {
            return Err(self);
        }
        self.raw = None;
        Ok(())
    }
}

impl Drop for NativeNvencOpenQuarantine {
    fn drop(&mut self) {
        if let Some(raw) = self.raw {
            if unsafe { rd_nvenc_shutdown(raw.as_ptr()) } == NV_OK {
                self.raw = None;
            }
        }
    }
}

impl DirectNvenc<NativeFrame> for NativeNvenc {
    fn capability(&self) -> &EncoderCapabilities {
        &self.capability
    }

    fn encode_texture(
        &mut self,
        frame: NativeFrame,
        force_keyframe: bool,
    ) -> Result<EncodedUnit, ProducerError> {
        if self.pending_output.is_some() || self.quarantined_frame.is_some() {
            return Err(ProducerError::ReclamationUnconfirmed);
        }
        let encoder = self.raw.ok_or(ProducerError::Closed)?;
        let mut loan = RdNvencOutputLoan::default();
        let status = unsafe {
            rd_nvenc_encode_texture(
                encoder.as_ptr(),
                frame.texture_ptr(),
                frame.capture_pts_us() as u64,
                u32::from(force_keyframe),
                &mut loan,
            )
        };
        if status != NV_OK {
            if status == NV_CLEANUP_FAILED {
                self.quarantined_frame = Some(frame);
                self.frame_requires_shutdown = true;
                return Err(ProducerError::ReclamationUnconfirmed);
            }
            release_frame_or_restore(&mut self.quarantined_frame, frame)?;
            return Err(map_nv(status));
        }
        if loan.data.is_null() || loan.size == 0 || loan.size > usize::MAX as u64 {
            let release_status = unsafe { rd_nvenc_release_output(encoder.as_ptr(), &loan) };
            if release_status != NV_OK {
                self.pending_output = Some(loan);
                self.quarantined_frame = Some(frame);
                return Err(ProducerError::ReclamationUnconfirmed);
            }
            release_frame_or_restore(&mut self.quarantined_frame, frame)?;
            return Err(ProducerError::EmptyAccessUnit);
        }

        // The C++ output remains a loan. Copy compressed bytes only, then return it.
        let data = Bytes::copy_from_slice(unsafe {
            std::slice::from_raw_parts(loan.data, loan.size as usize)
        });
        let output_status = unsafe { rd_nvenc_release_output(encoder.as_ptr(), &loan) };
        if output_status != NV_OK {
            self.pending_output = Some(loan);
            self.quarantined_frame = Some(frame);
            return Err(ProducerError::ReclamationUnconfirmed);
        }
        release_frame_or_restore(&mut self.quarantined_frame, frame)?;

        Ok(EncodedUnit {
            data,
            pts_us: i64::try_from(loan.timestamp).map_err(|_| ProducerError::InvalidTimestamp)?,
            // Only IDR is an independently decodable random-access point.
            key: loan.picture_type == 3,
            codec: self.config.codec,
            width: self.config.width,
            height: self.config.height,
            encoded_color: self.config.encoded_color,
        })
    }

    fn close(&mut self) -> Result<(), ProducerError> {
        if let Some(loan) = self.pending_output {
            let encoder = self.raw.ok_or(ProducerError::ReclamationUnconfirmed)?;
            let status = unsafe { rd_nvenc_release_output(encoder.as_ptr(), &loan) };
            if status != NV_OK {
                return Err(ProducerError::ReclamationUnconfirmed);
            }
            self.pending_output = None;
        }
        if let Some(encoder) = self.raw {
            let status = unsafe { rd_nvenc_shutdown(encoder.as_ptr()) };
            if status != NV_OK {
                // The raw encoder and any capture frame remain retained for retry.
                return Err(map_nv(status));
            }
            self.raw = None;
        }
        if let Some(frame) = self.quarantined_frame.take() {
            release_frame_or_restore(&mut self.quarantined_frame, frame)?;
        }
        self.frame_requires_shutdown = false;
        Ok(())
    }
}

impl Drop for NativeNvenc {
    fn drop(&mut self) {
        if self.pending_output.is_some() || self.quarantined_frame.is_some() {
            if let Some(frame) = self.quarantined_frame.take() {
                std::mem::forget(frame);
            }
            return;
        }
        if let Some(encoder) = self.raw {
            if unsafe { rd_nvenc_shutdown(encoder.as_ptr()) } == NV_OK {
                self.raw = None;
            }
        }
    }
}

fn source_color(
    format: DxgiFormat,
    raw_color_space: u32,
    bits_per_color: u32,
) -> Result<MediaColorContract, ProducerError> {
    let colorimetry = match format {
        DxgiFormat::B8G8R8A8Unorm
            if raw_color_space == DXGI_RGB_FULL_G22_P709 && bits_per_color >= 8 =>
        {
            Colorimetry {
                primaries: ColorPrimaries::Bt709,
                transfer: TransferCharacteristics::Gamma22,
                matrix: MatrixCoefficients::Identity,
                range: ColorRange::Full,
            }
        }
        DxgiFormat::R10G10B10A2Unorm
            if raw_color_space == DXGI_RGB_FULL_G2084_P2020 && bits_per_color >= 10 =>
        {
            Colorimetry {
                primaries: ColorPrimaries::Bt2020,
                transfer: TransferCharacteristics::Pq,
                matrix: MatrixCoefficients::Identity,
                range: ColorRange::Full,
            }
        }
        DxgiFormat::R10G10B10A2Unorm
            if raw_color_space == DXGI_RGB_FULL_G22_P709 && bits_per_color >= 10 =>
        {
            Colorimetry {
                primaries: ColorPrimaries::Bt709,
                transfer: TransferCharacteristics::Gamma22,
                matrix: MatrixCoefficients::Identity,
                range: ColorRange::Full,
            }
        }
        _ => return Err(ProducerError::ColorContractChanged),
    };
    Ok(MediaColorContract {
        storage: format.storage(),
        colorimetry,
        // Display luminance is diagnostic capability data, not content metadata.
        hdr_static: None,
        source_preserved: true,
    })
}

fn capability(config: ProducerConfig, descriptor: TextureDescriptor) -> EncoderCapabilities {
    EncoderCapabilities {
        codec: config.codec,
        adapter_luid: descriptor.adapter_luid,
        input_format: descriptor.format,
        input_color: descriptor.source_color,
        output_color: config.encoded_color,
        supported_modes: vec![EncoderMode {
            width: config.width,
            height: config.height,
            fps_numerator: config.fps_numerator,
            fps_denominator: config.fps_denominator,
        }],
        accepts_direct_d3d11_resource: true,
        supports_main10: matches!(
            descriptor.format,
            DxgiFormat::P010 | DxgiFormat::R10G10B10A2Unorm
        ),
        supports_hdr_static_metadata: config.codec == Codec::H265
            && matches!(
                descriptor.format,
                DxgiFormat::P010 | DxgiFormat::R10G10B10A2Unorm
            ),
    }
}

fn vui(color: MediaColorContract) -> Result<RdNvencColorConfig, ProducerError> {
    Ok(RdNvencColorConfig {
        enabled: 1,
        video_full_range: u32::from(color.colorimetry.range == ColorRange::Full),
        color_primaries: match color.colorimetry.primaries {
            ColorPrimaries::Bt709 => 1,
            ColorPrimaries::Bt2020 => 9,
            _ => return Err(ProducerError::ColorContractChanged),
        },
        transfer_characteristics: match color.colorimetry.transfer {
            TransferCharacteristics::Bt709 => 1,
            TransferCharacteristics::Gamma22 => 4,
            TransferCharacteristics::Srgb => 13,
            TransferCharacteristics::Pq => 16,
            TransferCharacteristics::Hlg => 18,
            _ => return Err(ProducerError::ColorContractChanged),
        },
        matrix_coefficients: match color.colorimetry.matrix {
            MatrixCoefficients::Identity => 0,
            MatrixCoefficients::Bt709 => 1,
            MatrixCoefficients::Bt2020NonConstantLuminance => 9,
            MatrixCoefficients::Bt2020ConstantLuminance => 10,
            MatrixCoefficients::Unspecified => return Err(ProducerError::ColorContractChanged),
        },
        video_format: 5, // H.273 unspecified video source format.
    })
}

fn hdr_static(
    metadata: Option<HdrStaticMetadata>,
) -> Result<RdNvencHdrStaticMetadata, ProducerError> {
    let Some(metadata) = metadata else {
        return Ok(RdNvencHdrStaticMetadata::default());
    };
    if !metadata.is_valid() {
        return Err(ProducerError::UnsupportedHdrMetadata);
    }
    let display = metadata.mastering_display;
    Ok(RdNvencHdrStaticMetadata {
        enabled: 1,
        display_primaries_x_gbr: [display.green.x, display.blue.x, display.red.x],
        display_primaries_y_gbr: [display.green.y, display.blue.y, display.red.y],
        white_point_x: display.white_point.x,
        white_point_y: display.white_point.y,
        // MediaColorContract uses the exact ST 2086 units expected by the SEI:
        // chromaticity 0.00002 and mastering luminance 0.0001 cd/m².
        max_display_mastering_luminance: display.max_luminance,
        min_display_mastering_luminance: display.min_luminance,
        max_content_light_level: metadata.content_light.max_cll,
        max_frame_average_light_level: metadata.content_light.max_fall,
    })
}

fn nv_input_format(format: DxgiFormat) -> Result<u32, ProducerError> {
    match format {
        DxgiFormat::Nv12 => Ok(1),
        DxgiFormat::P010 => Ok(2),
        DxgiFormat::B8G8R8A8Unorm => Ok(3),
        DxgiFormat::R8G8B8A8Unorm => Ok(4),
        DxgiFormat::R10G10B10A2Unorm => Ok(5),
        DxgiFormat::R16G16B16A16Float => Err(ProducerError::UnsupportedDirectInput),
    }
}

fn release_frame_or_restore(
    slot: &mut Option<NativeFrame>,
    mut frame: NativeFrame,
) -> Result<(), ProducerError> {
    if let Err(error) = frame.release() {
        // RECLAMATION_UNCERTAIN transfers the lease into the shared capture
        // owner and clears frame.lease; only a still-frame-owned pointer belongs
        // back in the encoder quarantine slot.
        if frame.lease.is_some() {
            *slot = Some(frame);
        }
        return Err(error);
    }
    Ok(())
}

fn retain_failed_frame(raw: RdDdFrame, owner: &Arc<CaptureOwner>) {
    if let Some(lease) = NonNull::new(raw.lease) {
        // On RECLAMATION_UNCERTAIN ownership of the exact lease pointer moves
        // into the shared capture owner and remains retryable there.
        let _ = owner.release_lease(lease);
    }
}

fn luid(raw: RdDdLuid) -> AdapterLuid {
    AdapterLuid(((raw.high_part as i64) << 32) | i64::from(raw.low_part))
}

fn raw_luid(luid: AdapterLuid) -> RdDdLuid {
    RdDdLuid {
        low_part: luid.0 as u32,
        high_part: (luid.0 >> 32) as i32,
    }
}

fn map_dd(status: i32) -> Result<(), ProducerError> {
    if status == DD_OK {
        Ok(())
    } else {
        Err(map_dd_status(status))
    }
}

fn map_dd_status(status: i32) -> ProducerError {
    match status {
        DD_RECLAMATION_UNCERTAIN | DD_LEASE_OUTSTANDING => ProducerError::ReclamationUnconfirmed,
        DD_GEOMETRY_CHANGED => ProducerError::SourceResolutionChanged,
        DD_UNSUPPORTED_FORMAT => ProducerError::SourceFormatChanged,
        DD_COLOR_CHANGED => ProducerError::ColorContractChanged,
        DD_INVALID_ARGUMENT => ProducerError::InvalidConfig,
        DD_UNSUPPORTED => ProducerError::UnsupportedDirectInput,
        DD_ACCESS_LOST => ProducerError::CaptureAccessLost,
        DD_NOT_FOUND | DD_D3D_FAILURE | DD_WOULD_BLOCK | DD_OK => ProducerError::CaptureFailed,
        _ => ProducerError::CaptureFailed,
    }
}

fn map_nv(status: i32) -> ProducerError {
    match status {
        NV_INVALID_ARGUMENT => ProducerError::InvalidConfig,
        NV_UNSUPPORTED => ProducerError::CapabilityExceeded,
        NV_DLL_NOT_FOUND | NV_DRIVER_TOO_OLD => ProducerError::BackendUnavailable,
        NV_D3D11_ERROR => ProducerError::AdapterMismatch,
        NV_BUSY | NV_CLEANUP_FAILED => ProducerError::ReclamationUnconfirmed,
        NV_OUT_OF_MEMORY | NV_ERROR | NV_INTERNAL_ERROR | NV_OK => ProducerError::EncoderFailed,
        _ => ProducerError::EncoderFailed,
    }
}
