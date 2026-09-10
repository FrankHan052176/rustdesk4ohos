//! Metadata-only HEVC hardware capability evidence, NOT codec/HDR performance.
//!
//! Integration: add `pub mod media_capability;` to the engine root. No Cargo
//! dependency is required. OHOS final linkage needs `native_media_codecbase`.
//! The backend uses only APIs introduced by API22 (SDK23-compatible surface).
//! Validation provenance: local headers are API26 (26.0.0.32); the intended
//! target/compatible API is 23. Signatures and introduction levels were checked
//! against official documentation, and local headers were used for ABI checks.
//! Neither an original SDK23 package nor an API24 device has been validated.

#[path = "platform/ohos_capability.rs"]
mod ohos_capability;

pub const CAPABILITY_REFERENCE: &str =
    "https://developer.huawei.com/consumer/cn/doc/harmonyos-references/capi-native-avcapability-h";
pub const CAPTURE_REFERENCE: &str = "https://developer.huawei.com/consumer/cn/doc/harmonyos-references/capi-native-avscreen-capture-h";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoTarget {
    pub width: i32,
    pub height: i32,
    pub fps: i32,
}

pub const TARGETS_120: [VideoTarget; 4] = [
    VideoTarget {
        width: 1280,
        height: 720,
        fps: 120,
    },
    VideoTarget {
        width: 1920,
        height: 1080,
        fps: 120,
    },
    VideoTarget {
        width: 2560,
        height: 1440,
        fps: 120,
    },
    VideoTarget {
        width: 3840,
        height: 2160,
        fps: 120,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecDirection {
    Encode,
    Decode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityError {
    UnsupportedPlatform,
    NoHardwareCapability { direction: CodecDirection },
    Native { api: &'static str, code: i32 },
    InvalidNativeResult { api: &'static str },
    UnexpectedSoftwareCodec,
    QueryLockPoisoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdvertisedRange {
    pub min: i32,
    pub max: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisedSizeRate {
    pub target: VideoTarget,
    /// Native boolean only: no error channel and no performance measurement.
    pub size_supported: bool,
    pub frame_rate_range: Result<AdvertisedRange, CapabilityError>,
    /// Applies to this size/rate, NOT a joint Main10+HDR+format guarantee.
    pub size_and_rate_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvertisedCodec {
    pub direction: CodecDirection,
    pub mime: &'static str,
    pub codec_name: String,
    pub hardware: bool,
    /// Raw OH_HEVCProfile values, copied from platform-owned metadata.
    pub profiles: Result<Vec<i32>, CapabilityError>,
    /// Exact HEVC_PROFILE_MAIN_10 membership, preserving query errors.
    pub main10_advertised: Result<bool, CapabilityError>,
    /// None means Main10 was not advertised; Err means query failed.
    pub main10_levels: Result<Option<Vec<i32>>, CapabilityError>,
    /// Raw OH_NativeBuffer_Format IDs, not OH_AVPixelFormat IDs.
    pub native_buffer_formats: Result<Vec<i32>, CapabilityError>,
    pub pixel_formats: Result<Vec<i32>, CapabilityError>,
    pub width_alignment: Result<i32, CapabilityError>,
    pub height_alignment: Result<i32, CapabilityError>,
    pub sizes: [AdvertisedSizeRate; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unmeasured {
    NotMeasured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenCaptureRateEvidence {
    /// Documentation only. No capture was created, queried or started.
    OhosDocumentedMaximum {
        fps: i32,
        source: &'static str,
    },
    NotApplicableToPlatform,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaCapabilityReport {
    pub encoder: Result<AdvertisedCodec, CapabilityError>,
    pub decoder: Result<AdvertisedCodec, CapabilityError>,
    pub screen_capture_rate: ScreenCaptureRateEvidence,
    pub codec_throughput: Unmeasured,
    pub hdr_display: Unmeasured,
    pub screen_capture_hdr: Unmeasured,
}

/// Calls actual OHOS metadata APIs, or explicitly reports unsupported elsewhere.
/// Does not create codecs, request permissions, capture pixels, or alter display.
/// Call off the UI thread: the platform may perform IPC during capability lookup.
pub fn query_hevc_hardware_capabilities() -> MediaCapabilityReport {
    ohos_capability::query()
}

/// Actual H264 hardware decoder metadata for the original-peer viewer advert.
/// Main10 fields are HEVC-specific (not applicable/false for H264); raw H264
/// profiles/native formats remain untouched. No software or assumed fallback.
pub fn query_h264_hardware_decoder() -> Result<AdvertisedCodec, CapabilityError> {
    ohos_capability::query_h264_decoder()
}

/// Actual H264 hardware encoder metadata for the Surface publisher advert.
/// This remains metadata only; Publisher::open is the real path gate.
pub fn query_h264_hardware_encoder() -> Result<AdvertisedCodec, CapabilityError> {
    ohos_capability::query_h264_encoder()
}

#[cfg(all(test, not(target_env = "ohos")))]
mod tests {
    use super::*;

    #[test]
    fn unsupported_host_never_fabricates_hardware_or_measurements() {
        let report = query_hevc_hardware_capabilities();
        assert_eq!(report.encoder, Err(CapabilityError::UnsupportedPlatform));
        assert_eq!(report.decoder, Err(CapabilityError::UnsupportedPlatform));
        assert_eq!(
            report.screen_capture_rate,
            ScreenCaptureRateEvidence::NotApplicableToPlatform
        );
        assert_eq!(report.codec_throughput, Unmeasured::NotMeasured);
        assert_eq!(report.hdr_display, Unmeasured::NotMeasured);
        assert_eq!(
            TARGETS_120[3],
            VideoTarget {
                width: 3840,
                height: 2160,
                fps: 120
            }
        );
    }
}
