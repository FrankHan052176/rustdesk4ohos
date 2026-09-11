//! Modern Windows producer contracts: actual D3D11 capture texture -> direct
//! NVENC resource registration -> H.264/H.265 Annex-B. No staging/readback,
//! pixel mapping, GLES, software fallback, scaling, or repeated frames. Capture
//! updates newer than the negotiated frame interval are released before encode.
//! The independently implemented Desktop Duplication + direct NVENC backend is
//! available from `windows_native` on Windows. Runtime capability still fails
//! closed until the actual adapter, format and exact mode initialize correctly.

use crate::media_color::{
    ChromaSubsampling, ColorPrimaries, ColorRange, MatrixCoefficients, MediaColorContract,
    NegotiatedColorClass, NumericRepresentation, PixelStorage, TransferCharacteristics,
    classify_negotiated,
};
use hbb_common::bytes::Bytes;
use std::{ffi::c_void, fmt, mem::ManuallyDrop, num::NonZeroU32};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    H264,
    H265,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DxgiFormat {
    B8G8R8A8Unorm,
    R8G8B8A8Unorm,
    R10G10B10A2Unorm,
    R16G16B16A16Float,
    Nv12,
    P010,
}
impl DxgiFormat {
    pub const fn component_bits(self) -> u8 {
        match self {
            Self::B8G8R8A8Unorm | Self::R8G8B8A8Unorm | Self::Nv12 => 8,
            Self::R10G10B10A2Unorm | Self::P010 => 10,
            Self::R16G16B16A16Float => 16,
        }
    }
    pub const fn storage(self) -> PixelStorage {
        PixelStorage {
            bit_depth: self.component_bits(),
            subsampling: match self {
                Self::Nv12 | Self::P010 => ChromaSubsampling::Cs420,
                _ => ChromaSubsampling::Cs444,
            },
            numeric_representation: match self {
                Self::R16G16B16A16Float => NumericRepresentation::FloatingPoint,
                _ => NumericRepresentation::UnsignedNormalized,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdapterLuid(pub i64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureDescriptor {
    pub width: NonZeroU32,
    pub height: NonZeroU32,
    pub format: DxgiFormat,
    pub array_slice: u32,
    pub mip_level: u32,
    pub adapter_luid: AdapterLuid,
    pub source_color: MediaColorContract,
}

/// # Safety
/// Returned pointers are retained COM references to the actual capture allocation
/// (`ID3D11Texture2D*`) and its `ID3D11Device*`, alive until this lease is dropped.
pub unsafe trait D3d11TextureLease: Send {
    fn texture_ptr(&self) -> *mut c_void;
    fn device_ptr(&self) -> *mut c_void;
    fn descriptor(&self) -> TextureDescriptor;
    fn capture_pts_us(&self) -> i64;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureApi {
    WindowsGraphicsCapture,
    DesktopDuplication,
}
pub trait TextureCapture: Send {
    type Frame: D3d11TextureLease;
    fn api(&self) -> CaptureApi;
    fn next_texture(&mut self) -> Result<Option<Self::Frame>, ProducerError>;
    fn close(&mut self) -> Result<(), ProducerError>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncoderMode {
    pub width: NonZeroU32,
    pub height: NonZeroU32,
    pub fps_numerator: NonZeroU32,
    pub fps_denominator: NonZeroU32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderCapabilities {
    pub codec: Codec,
    pub adapter_luid: AdapterLuid,
    pub input_format: DxgiFormat,
    pub input_color: MediaColorContract,
    pub output_color: MediaColorContract,
    pub supported_modes: Vec<EncoderMode>,
    pub accepts_direct_d3d11_resource: bool,
    pub supports_main10: bool,
    pub supports_hdr_static_metadata: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProducerConfig {
    pub codec: Codec,
    pub width: NonZeroU32,
    pub height: NonZeroU32,
    pub fps_numerator: NonZeroU32,
    pub fps_denominator: NonZeroU32,
    pub bitrate: NonZeroU32,
    pub source_color: MediaColorContract,
    pub encoded_color: MediaColorContract,
}
/// Compressed output only; parameter sets/prefixes and one picture stay together.
pub struct EncodedUnit {
    pub data: Bytes,
    pub pts_us: i64,
    pub key: bool,
    pub codec: Codec,
    pub width: NonZeroU32,
    pub height: NonZeroU32,
    pub encoded_color: MediaColorContract,
}
/// Must register/map the exact DirectX resource, retain the frame lease until
/// encode completion, and unmap/unregister safely. VUI never substitutes for
/// actual pixel format/transfer conversion or HDR metadata propagation.
pub trait DirectNvenc<F: D3d11TextureLease>: Send {
    fn capability(&self) -> &EncoderCapabilities;
    fn encode_texture(
        &mut self,
        frame: F,
        force_keyframe: bool,
    ) -> Result<EncodedUnit, ProducerError>;
    fn close(&mut self) -> Result<(), ProducerError>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProducerError {
    UnsupportedPlatform,
    InvalidConfig,
    BackendUnavailable,
    NullD3dResource,
    AdapterMismatch,
    SourceResolutionChanged,
    SourceFormatChanged,
    ColorContractChanged,
    UnsupportedDirectInput,
    UnsupportedBitDepth,
    UnsupportedHdrMetadata,
    CapabilityExceeded,
    InvalidTimestamp,
    InvalidAnnexB,
    EmptyAccessUnit,
    EncoderChangedResolution,
    EncoderChangedColorContract,
    CaptureAccessLost,
    CaptureFailed,
    EncoderFailed,
    ReclamationUnconfirmed,
    Closed,
}
impl fmt::Display for ProducerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ProducerError {}

/// One frame at a time: the capture lease remains alive through NVENC completion.
/// There is deliberately no drop/thinning queue or fallback path.
pub struct WindowsPublisher<C, E> {
    capture: ManuallyDrop<C>,
    encoder: ManuallyDrop<E>,
    config: ProducerConfig,
    force_keyframe: bool,
    next_encode_pts_us: Option<i64>,
}
impl<C: TextureCapture, E: DirectNvenc<C::Frame>> WindowsPublisher<C, E> {
    pub fn open(capture: C, encoder: E, config: ProducerConfig) -> Result<Self, ProducerError> {
        validate_capability(config, encoder.capability())?;
        Ok(Self {
            capture: ManuallyDrop::new(capture),
            encoder: ManuallyDrop::new(encoder),
            config,
            force_keyframe: true,
            next_encode_pts_us: None,
        })
    }
    pub fn request_keyframe(&mut self) -> Result<(), ProducerError> {
        self.force_keyframe = true;
        Ok(())
    }
    pub fn next_unit(&mut self) -> Result<Option<EncodedUnit>, ProducerError> {
        let Some(frame) = self.capture.next_texture()? else {
            return Ok(None);
        };
        validate_texture(self.config, &frame, self.encoder.capability())?;
        let capture_pts_us = frame.capture_pts_us();
        if capture_pts_us < 0 {
            return Err(ProducerError::InvalidTimestamp);
        }
        let numerator = u64::from(self.config.fps_numerator.get());
        let denominator = u64::from(self.config.fps_denominator.get());
        let interval_us = ((1_000_000_u64 * denominator) / numerator).max(1) as i64;
        if !self.force_keyframe
            && let Some(next_pts_us) = self.next_encode_pts_us
            && capture_pts_us < next_pts_us
        {
            // Drop the capture lease before NVENC registration. This keeps
            // the encoded stream at the negotiated rate without dropping
            // interdependent H.26x access units after encode.
            return Ok(None);
        }
        let requested_keyframe = self.force_keyframe;
        let unit = self.encoder.encode_texture(frame, requested_keyframe)?;
        validate_unit(self.config, &unit)?;
        if requested_keyframe && !unit.key {
            return Err(ProducerError::EncoderFailed);
        }
        self.force_keyframe = false;
        self.next_encode_pts_us = Some(match self.next_encode_pts_us {
            Some(next_pts_us) if !requested_keyframe && capture_pts_us >= next_pts_us => {
                let elapsed_intervals = capture_pts_us
                    .saturating_sub(next_pts_us)
                    .div_euclid(interval_us)
                    .saturating_add(1);
                next_pts_us.saturating_add(elapsed_intervals.saturating_mul(interval_us))
            }
            _ => capture_pts_us.saturating_add(interval_us),
        });
        Ok(Some(unit))
    }
    /// Reclaims encoder/in-flight resources before capture. On failure, native
    /// owners move into a non-dropping quarantine which records the completed
    /// stages and can be retried.
    pub fn close(self) -> Result<(), CloseQuarantine<C, E>> {
        CloseQuarantine {
            capture: self.capture,
            encoder: self.encoder,
            encoder_closed: false,
            capture_closed: false,
            last_error: ProducerError::ReclamationUnconfirmed,
        }
        .retry()
    }
}
pub struct CloseQuarantine<C: TextureCapture, E: DirectNvenc<C::Frame>> {
    capture: ManuallyDrop<C>,
    encoder: ManuallyDrop<E>,
    encoder_closed: bool,
    capture_closed: bool,
    last_error: ProducerError,
}
impl<C: TextureCapture, E: DirectNvenc<C::Frame>> CloseQuarantine<C, E> {
    pub fn last_error(&self) -> ProducerError {
        self.last_error
    }
    pub fn encoder_closed(&self) -> bool {
        self.encoder_closed
    }
    pub fn capture_closed(&self) -> bool {
        self.capture_closed
    }
    pub fn retry(mut self) -> Result<(), Self> {
        if !self.encoder_closed {
            if let Err(error) = self.encoder.close() {
                self.last_error = error;
                return Err(self);
            }
            self.encoder_closed = true;
        }
        if !self.capture_closed {
            if let Err(error) = self.capture.close() {
                self.last_error = error;
                return Err(self);
            }
            self.capture_closed = true;
        }
        unsafe {
            ManuallyDrop::drop(&mut self.encoder);
            ManuallyDrop::drop(&mut self.capture);
        }
        Ok(())
    }
}
fn validate_capability(c: ProducerConfig, p: &EncoderCapabilities) -> Result<(), ProducerError> {
    if p.codec != c.codec {
        return Err(ProducerError::InvalidConfig);
    }
    if !p.accepts_direct_d3d11_resource {
        return Err(ProducerError::UnsupportedDirectInput);
    }
    if p.input_format.storage() != p.input_color.storage || p.input_color != c.source_color {
        return Err(ProducerError::ColorContractChanged);
    }
    let requested_mode = EncoderMode {
        width: c.width,
        height: c.height,
        fps_numerator: c.fps_numerator,
        fps_denominator: c.fps_denominator,
    };
    if !p.supported_modes.contains(&requested_mode) {
        return Err(ProducerError::CapabilityExceeded);
    }
    validate_format_color(p.input_format, p.input_color)?;
    if !c.source_color.source_preserved
        || c.source_color.colorimetry.primaries == ColorPrimaries::Unspecified
        || c.source_color.colorimetry.transfer == TransferCharacteristics::Unspecified
        || c.source_color.colorimetry.matrix == MatrixCoefficients::Unspecified
        || c.source_color.colorimetry.range == ColorRange::Unspecified
    {
        return Err(ProducerError::ColorContractChanged);
    }
    if !matches!(c.source_color.storage.bit_depth, 8 | 10)
        || !matches!(c.encoded_color.storage.bit_depth, 8 | 10)
    {
        return Err(ProducerError::UnsupportedBitDepth);
    }
    // Direct NVENC may perform its native RGB-to-YCbCr/subsampling/range
    // conversion, but this zero-copy path has no transfer-function, gamut or
    // HDR-metadata conversion stage. Reject relabeling SDR as HDR (or vice
    // versa) and reject fabricated mastering/content-light metadata.
    if c.source_color.colorimetry.primaries != c.encoded_color.colorimetry.primaries
        || c.source_color.colorimetry.transfer != c.encoded_color.colorimetry.transfer
        || c.source_color.hdr_static != c.encoded_color.hdr_static
        || c.source_color.storage.bit_depth != c.encoded_color.storage.bit_depth
    {
        return Err(ProducerError::ColorContractChanged);
    }
    if (c.source_color.storage.bit_depth == 10 || c.encoded_color.storage.bit_depth == 10)
        && !p.supports_main10
    {
        return Err(ProducerError::UnsupportedBitDepth);
    }
    validate_encoded_transport(c.encoded_color)?;
    let color_class = classify_negotiated(&c.encoded_color, &p.output_color)
        .ok_or(ProducerError::ColorContractChanged)?;
    if matches!(
        c.encoded_color.colorimetry.transfer,
        TransferCharacteristics::Pq | TransferCharacteristics::Hlg
    ) {
        if !matches!(
            color_class,
            NegotiatedColorClass::Hdr10Pq | NegotiatedColorClass::Hlg10
        ) {
            return Err(ProducerError::ColorContractChanged);
        }
        if c.encoded_color.hdr_static.is_some() && !p.supports_hdr_static_metadata {
            return Err(ProducerError::UnsupportedHdrMetadata);
        }
    } else if c.encoded_color.hdr_static.is_some() {
        return Err(ProducerError::ColorContractChanged);
    }
    Ok(())
}

/// Validates the actual H.264/H.265 transport surface produced by this direct
/// NVENC path. RGB is an accepted input resource, not the encoded chroma layout.
pub(crate) fn validate_encoded_transport(color: MediaColorContract) -> Result<(), ProducerError> {
    let matrix_matches_primaries = match color.colorimetry.primaries {
        ColorPrimaries::Bt709 => color.colorimetry.matrix == MatrixCoefficients::Bt709,
        ColorPrimaries::Bt2020 => matches!(
            color.colorimetry.matrix,
            MatrixCoefficients::Bt2020NonConstantLuminance
                | MatrixCoefficients::Bt2020ConstantLuminance
        ),
        _ => false,
    };
    if color.storage.subsampling != ChromaSubsampling::Cs420
        || color.storage.numeric_representation != NumericRepresentation::UnsignedNormalized
        || !matrix_matches_primaries
        || classify_negotiated(&color, &color).is_none()
    {
        return Err(ProducerError::ColorContractChanged);
    }
    Ok(())
}

fn validate_format_color(
    format: DxgiFormat,
    color: MediaColorContract,
) -> Result<(), ProducerError> {
    let valid = match format {
        DxgiFormat::B8G8R8A8Unorm
        | DxgiFormat::R8G8B8A8Unorm
        | DxgiFormat::R10G10B10A2Unorm
        | DxgiFormat::R16G16B16A16Float => {
            color.storage.subsampling == ChromaSubsampling::Cs444
                && color.colorimetry.matrix == MatrixCoefficients::Identity
                && color.colorimetry.range == ColorRange::Full
        }
        DxgiFormat::Nv12 | DxgiFormat::P010 => {
            color.storage.subsampling == ChromaSubsampling::Cs420
                && matches!(
                    color.colorimetry.matrix,
                    MatrixCoefficients::Bt709
                        | MatrixCoefficients::Bt2020NonConstantLuminance
                        | MatrixCoefficients::Bt2020ConstantLuminance
                )
        }
    };
    valid
        .then_some(())
        .ok_or(ProducerError::ColorContractChanged)
}
fn validate_texture(
    c: ProducerConfig,
    f: &dyn D3d11TextureLease,
    p: &EncoderCapabilities,
) -> Result<(), ProducerError> {
    if f.texture_ptr().is_null() || f.device_ptr().is_null() {
        return Err(ProducerError::NullD3dResource);
    }
    if f.capture_pts_us() < 0 {
        return Err(ProducerError::InvalidTimestamp);
    }
    let d = f.descriptor();
    if d.adapter_luid != p.adapter_luid {
        return Err(ProducerError::AdapterMismatch);
    }
    if d.width != c.width || d.height != c.height {
        return Err(ProducerError::SourceResolutionChanged);
    }
    if d.format != p.input_format {
        return Err(ProducerError::SourceFormatChanged);
    }
    if d.format.storage() != d.source_color.storage
        || d.format != p.input_format
        || d.source_color != p.input_color
        || d.source_color != c.source_color
    {
        return Err(ProducerError::ColorContractChanged);
    }
    Ok(())
}
fn validate_unit(c: ProducerConfig, u: &EncodedUnit) -> Result<(), ProducerError> {
    if u.data.is_empty() {
        return Err(ProducerError::EmptyAccessUnit);
    }
    if !(u.data.starts_with(&[0, 0, 1]) || u.data.starts_with(&[0, 0, 0, 1])) {
        return Err(ProducerError::InvalidAnnexB);
    }
    if u.pts_us < 0 {
        return Err(ProducerError::InvalidTimestamp);
    }
    if u.codec != c.codec || u.width != c.width || u.height != c.height {
        return Err(ProducerError::EncoderChangedResolution);
    }
    if classify_negotiated(&u.encoded_color, &c.encoded_color).is_none() {
        return Err(ProducerError::EncoderChangedColorContract);
    }
    Ok(())
}
/// Compile-time source availability only. Runtime support still requires the
/// selected Windows output, D3D11 adapter and exact NVENC mode to initialize.
pub const NATIVE_BACKEND_IMPLEMENTED: bool = cfg!(all(
    target_os = "windows",
    feature = "windows-modern-producer"
));

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_color::{
        ColorPrimaries, ColorRange, Colorimetry, MatrixCoefficients, TransferCharacteristics,
    };

    fn sdr(format: DxgiFormat) -> MediaColorContract {
        MediaColorContract {
            storage: format.storage(),
            colorimetry: Colorimetry {
                primaries: ColorPrimaries::Bt709,
                transfer: TransferCharacteristics::Srgb,
                matrix: MatrixCoefficients::Identity,
                range: ColorRange::Full,
            },
            hdr_static: None,
            source_preserved: true,
        }
    }

    #[test]
    fn dxgi_storage_must_match_the_common_color_contract() {
        let source_color = sdr(DxgiFormat::B8G8R8A8Unorm);
        let mut encoded_color = sdr(DxgiFormat::Nv12);
        encoded_color.colorimetry.matrix = MatrixCoefficients::Bt709;
        encoded_color.colorimetry.range = ColorRange::Limited;
        let config = ProducerConfig {
            codec: Codec::H265,
            width: NonZeroU32::new(3840).unwrap(),
            height: NonZeroU32::new(2160).unwrap(),
            fps_numerator: NonZeroU32::new(120).unwrap(),
            fps_denominator: NonZeroU32::new(1).unwrap(),
            bitrate: NonZeroU32::new(80_000_000).unwrap(),
            source_color,
            encoded_color,
        };
        let mut cap = EncoderCapabilities {
            codec: Codec::H265,
            adapter_luid: AdapterLuid(1),
            input_format: DxgiFormat::P010,
            input_color: source_color,
            output_color: encoded_color,
            supported_modes: vec![EncoderMode {
                width: NonZeroU32::new(3840).unwrap(),
                height: NonZeroU32::new(2160).unwrap(),
                fps_numerator: NonZeroU32::new(120).unwrap(),
                fps_denominator: NonZeroU32::new(1).unwrap(),
            }],
            accepts_direct_d3d11_resource: true,
            supports_main10: false,
            supports_hdr_static_metadata: false,
        };
        assert_eq!(
            validate_capability(config, &cap),
            Err(ProducerError::ColorContractChanged)
        );

        cap.input_format = DxgiFormat::B8G8R8A8Unorm;
        assert_eq!(validate_capability(config, &cap), Ok(()));

        cap.supported_modes = vec![EncoderMode {
            width: NonZeroU32::new(1920).unwrap(),
            height: NonZeroU32::new(1080).unwrap(),
            fps_numerator: NonZeroU32::new(120).unwrap(),
            fps_denominator: NonZeroU32::new(1).unwrap(),
        }];
        assert_eq!(
            validate_capability(config, &cap),
            Err(ProducerError::CapabilityExceeded)
        );
        cap.supported_modes = vec![EncoderMode {
            width: config.width,
            height: config.height,
            fps_numerator: NonZeroU32::new(240).unwrap(),
            fps_denominator: NonZeroU32::new(2).unwrap(),
        }];
        assert_eq!(
            validate_capability(config, &cap),
            Err(ProducerError::CapabilityExceeded)
        );

        cap.supported_modes = vec![EncoderMode {
            width: config.width,
            height: config.height,
            fps_numerator: config.fps_numerator,
            fps_denominator: config.fps_denominator,
        }];
        cap.output_color = source_color;
        assert_eq!(
            validate_capability(config, &cap),
            Err(ProducerError::ColorContractChanged)
        );

        let rgb10_color = sdr(DxgiFormat::R10G10B10A2Unorm);
        let mut yuv10_color = sdr(DxgiFormat::P010);
        yuv10_color.colorimetry.matrix = MatrixCoefficients::Bt709;
        yuv10_color.colorimetry.range = ColorRange::Limited;
        let rgb10_config = ProducerConfig {
            source_color: rgb10_color,
            encoded_color: yuv10_color,
            ..config
        };
        cap.input_format = DxgiFormat::R10G10B10A2Unorm;
        cap.input_color = rgb10_color;
        cap.output_color = yuv10_color;
        cap.supports_main10 = false;
        assert_eq!(
            validate_capability(rgb10_config, &cap),
            Err(ProducerError::UnsupportedBitDepth)
        );

        let float16_color = sdr(DxgiFormat::R16G16B16A16Float);
        let float16_config = ProducerConfig {
            source_color: float16_color,
            encoded_color: source_color,
            ..config
        };
        cap.input_format = DxgiFormat::R16G16B16A16Float;
        cap.input_color = float16_color;
        assert_eq!(
            validate_capability(float16_config, &cap),
            Err(ProducerError::UnsupportedBitDepth)
        );
    }

    struct Capture {
        close_attempts: u8,
    }
    impl TextureCapture for Capture {
        type Frame = Frame;
        fn api(&self) -> CaptureApi {
            CaptureApi::DesktopDuplication
        }
        fn next_texture(&mut self) -> Result<Option<Self::Frame>, ProducerError> {
            Ok(None)
        }
        fn close(&mut self) -> Result<(), ProducerError> {
            self.close_attempts += 1;
            if self.close_attempts == 1 {
                Err(ProducerError::CaptureFailed)
            } else {
                Ok(())
            }
        }
    }
    struct Frame;
    unsafe impl D3d11TextureLease for Frame {
        fn texture_ptr(&self) -> *mut c_void {
            std::ptr::null_mut()
        }
        fn device_ptr(&self) -> *mut c_void {
            std::ptr::null_mut()
        }
        fn descriptor(&self) -> TextureDescriptor {
            unreachable!()
        }
        fn capture_pts_us(&self) -> i64 {
            0
        }
    }
    struct Encoder {
        capability: EncoderCapabilities,
        close_attempts: u8,
    }
    impl DirectNvenc<Frame> for Encoder {
        fn capability(&self) -> &EncoderCapabilities {
            &self.capability
        }
        fn encode_texture(&mut self, _: Frame, _: bool) -> Result<EncodedUnit, ProducerError> {
            unreachable!()
        }
        fn close(&mut self) -> Result<(), ProducerError> {
            self.close_attempts += 1;
            if self.close_attempts == 1 {
                Err(ProducerError::EncoderFailed)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn failed_close_retains_complete_publisher_for_retry() {
        let source_color = sdr(DxgiFormat::B8G8R8A8Unorm);
        let mut encoded_color = sdr(DxgiFormat::Nv12);
        encoded_color.colorimetry.matrix = MatrixCoefficients::Bt709;
        encoded_color.colorimetry.range = ColorRange::Limited;
        let config = ProducerConfig {
            codec: Codec::H264,
            width: NonZeroU32::new(1920).unwrap(),
            height: NonZeroU32::new(1080).unwrap(),
            fps_numerator: NonZeroU32::new(120).unwrap(),
            fps_denominator: NonZeroU32::new(1).unwrap(),
            bitrate: NonZeroU32::new(20_000_000).unwrap(),
            source_color,
            encoded_color,
        };
        let capability = EncoderCapabilities {
            codec: Codec::H264,
            adapter_luid: AdapterLuid(1),
            input_format: DxgiFormat::B8G8R8A8Unorm,
            input_color: source_color,
            output_color: encoded_color,
            supported_modes: vec![EncoderMode {
                width: NonZeroU32::new(1920).unwrap(),
                height: NonZeroU32::new(1080).unwrap(),
                fps_numerator: NonZeroU32::new(120).unwrap(),
                fps_denominator: NonZeroU32::new(1).unwrap(),
            }],
            accepts_direct_d3d11_resource: true,
            supports_main10: false,
            supports_hdr_static_metadata: false,
        };
        let publisher = WindowsPublisher::open(
            Capture { close_attempts: 0 },
            Encoder {
                capability,
                close_attempts: 0,
            },
            config,
        )
        .unwrap();
        let quarantine = publisher.close().unwrap_err();
        assert_eq!(quarantine.last_error(), ProducerError::EncoderFailed);
        assert!(!quarantine.encoder_closed());
        assert!(!quarantine.capture_closed());
        assert_eq!(quarantine.capture.close_attempts, 0);
        assert_eq!(quarantine.encoder.close_attempts, 1);

        let quarantine = quarantine.retry().unwrap_err();
        assert_eq!(quarantine.last_error(), ProducerError::CaptureFailed);
        assert!(quarantine.encoder_closed());
        assert!(!quarantine.capture_closed());
        assert_eq!(quarantine.capture.close_attempts, 1);
        assert_eq!(quarantine.encoder.close_attempts, 2);

        assert!(quarantine.retry().is_ok());
    }
}
