//! OHOS native_avcapability.h binding. No pixel/codec-instance/surface APIs.
//! Capability and returned arrays are platform-owned; copy metadata before
//! querying again, never free platform pointers or expose them across threads.
//! ABI checked with installed API26 headers, using only APIs introduced <=22
//! for target/compatible API23. No API24/26 additions are bound. Original SDK23
//! package verification and execution on the API24 device remain outstanding.

use super::{CapabilityError, MediaCapabilityReport, ScreenCaptureRateEvidence, Unmeasured};

#[cfg(target_env = "ohos")]
static QUERY_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(super) fn query_h264_decoder() -> Result<super::AdvertisedCodec, CapabilityError> {
    #[cfg(target_env = "ohos")]
    {
        let _guard = QUERY_LOCK
            .lock()
            .map_err(|_| CapabilityError::QueryLockPoisoned)?;
        native::query_codec(super::CodecDirection::Decode, false)
    }
    #[cfg(not(target_env = "ohos"))]
    {
        Err(CapabilityError::UnsupportedPlatform)
    }
}

pub(super) fn query_h264_encoder() -> Result<super::AdvertisedCodec, CapabilityError> {
    #[cfg(target_env = "ohos")]
    {
        let _guard = QUERY_LOCK
            .lock()
            .map_err(|_| CapabilityError::QueryLockPoisoned)?;
        native::query_codec(super::CodecDirection::Encode, false)
    }
    #[cfg(not(target_env = "ohos"))]
    {
        Err(CapabilityError::UnsupportedPlatform)
    }
}

pub(super) fn query() -> MediaCapabilityReport {
    #[cfg(target_env = "ohos")]
    let (encoder, decoder, screen_capture_rate) = {
        // Serialize our use of platform-owned query storage, including copying.
        let (encoder, decoder) = match QUERY_LOCK.lock() {
            Ok(_guard) => (
                native::query_codec(super::CodecDirection::Encode, true),
                native::query_codec(super::CodecDirection::Decode, true),
            ),
            Err(_) => (
                Err(CapabilityError::QueryLockPoisoned),
                Err(CapabilityError::QueryLockPoisoned),
            ),
        };
        (
            encoder,
            decoder,
            ScreenCaptureRateEvidence::OhosDocumentedMaximum {
                fps: 60,
                source: super::CAPTURE_REFERENCE,
            },
        )
    };
    #[cfg(not(target_env = "ohos"))]
    let (encoder, decoder, screen_capture_rate) = (
        Err(CapabilityError::UnsupportedPlatform),
        Err(CapabilityError::UnsupportedPlatform),
        ScreenCaptureRateEvidence::NotApplicableToPlatform,
    );
    MediaCapabilityReport {
        encoder,
        decoder,
        screen_capture_rate,
        codec_throughput: Unmeasured::NotMeasured,
        hdr_display: Unmeasured::NotMeasured,
        screen_capture_hdr: Unmeasured::NotMeasured,
    }
}

#[cfg(target_env = "ohos")]
mod native {
    use super::super::{
        AdvertisedCodec, AdvertisedRange, AdvertisedSizeRate, CapabilityError, CodecDirection,
        TARGETS_120,
    };
    use std::{
        ffi::{CStr, c_char, c_int},
        ptr, slice,
    };

    // native_avcapability.h: HARDWARE=0; native_avcodec_base.h: MAIN_10=1.
    // C enums below are ABI-sized integers (checked against target SDK headers).
    const HARDWARE: c_int = 0;
    const HEVC_MAIN10: i32 = 1;
    const AV_ERR_OK: c_int = 0;
    const MAX_METADATA_ITEMS: u32 = 4096;
    #[repr(C)]
    struct Capability {
        _opaque: [u8; 0],
    }
    #[repr(C)]
    #[derive(Default)]
    struct Range {
        min: i32,
        max: i32,
    }

    #[link(name = "native_media_codecbase")]
    unsafe extern "C" {
        fn OH_AVCodec_GetCapabilityByCategory(
            mime: *const c_char,
            is_encoder: bool,
            category: c_int,
        ) -> *mut Capability;
        fn OH_AVCapability_IsHardware(cap: *mut Capability) -> bool;
        fn OH_AVCapability_GetName(cap: *mut Capability) -> *const c_char;
        fn OH_AVCapability_GetSupportedProfiles(
            cap: *mut Capability,
            values: *mut *const i32,
            count: *mut u32,
        ) -> c_int;
        fn OH_AVCapability_GetSupportedLevelsForProfile(
            cap: *mut Capability,
            profile: i32,
            values: *mut *const i32,
            count: *mut u32,
        ) -> c_int;
        fn OH_AVCapability_GetVideoSupportedNativeBufferFormats(
            cap: *mut Capability,
            values: *mut *const c_int,
            count: *mut u32,
        ) -> c_int;
        fn OH_AVCapability_GetVideoSupportedPixelFormats(
            cap: *mut Capability,
            values: *mut *const i32,
            count: *mut u32,
        ) -> c_int;
        fn OH_AVCapability_GetVideoWidthAlignment(
            cap: *mut Capability,
            alignment: *mut i32,
        ) -> c_int;
        fn OH_AVCapability_GetVideoHeightAlignment(
            cap: *mut Capability,
            alignment: *mut i32,
        ) -> c_int;
        fn OH_AVCapability_IsVideoSizeSupported(
            cap: *mut Capability,
            width: i32,
            height: i32,
        ) -> bool;
        fn OH_AVCapability_GetVideoFrameRateRangeForSize(
            cap: *mut Capability,
            width: i32,
            height: i32,
            range: *mut Range,
        ) -> c_int;
        fn OH_AVCapability_AreVideoSizeAndFrameRateSupported(
            cap: *mut Capability,
            width: i32,
            height: i32,
            fps: i32,
        ) -> bool;
    }

    fn check(api: &'static str, code: c_int) -> Result<(), CapabilityError> {
        if code == AV_ERR_OK {
            Ok(())
        } else {
            Err(CapabilityError::Native { api, code })
        }
    }

    // Closure performs one native query. Platform memory is copied immediately;
    // zero-length lists never construct a Rust slice from a null pointer.
    fn list(
        api: &'static str,
        call: impl FnOnce(*mut *const i32, *mut u32) -> c_int,
    ) -> Result<Vec<i32>, CapabilityError> {
        let mut values = ptr::null();
        let mut count = 0;
        check(api, call(&mut values, &mut count))?;
        if count == 0 {
            return Ok(Vec::new());
        }
        if values.is_null() || count > MAX_METADATA_ITEMS || !values.is_aligned() {
            return Err(CapabilityError::InvalidNativeResult { api });
        }
        // SAFETY: successful native API returns count live int32/enum entries;
        // the serialized caller copies before the next native query.
        Ok(unsafe { slice::from_raw_parts(values, count as usize) }.to_vec())
    }

    fn alignment(
        api: &'static str,
        call: impl FnOnce(*mut i32) -> c_int,
    ) -> Result<i32, CapabilityError> {
        let mut value = 0;
        check(api, call(&mut value))?;
        if value <= 0 {
            return Err(CapabilityError::InvalidNativeResult { api });
        }
        Ok(value)
    }

    pub(super) fn query_codec(
        direction: CodecDirection,
        hevc: bool,
    ) -> Result<AdvertisedCodec, CapabilityError> {
        // SAFETY: fixed NUL-terminated HEVC MIME, SDK category enum; no raw
        // pointer escapes this function. APIs below require a video capability.
        let cap = unsafe {
            OH_AVCodec_GetCapabilityByCategory(
                if hevc {
                    c"video/hevc".as_ptr()
                } else {
                    c"video/avc".as_ptr()
                },
                direction == CodecDirection::Encode,
                HARDWARE,
            )
        };
        if cap.is_null() {
            return Err(CapabilityError::NoHardwareCapability { direction });
        }
        if !unsafe { OH_AVCapability_IsHardware(cap) } {
            return Err(CapabilityError::UnexpectedSoftwareCodec);
        }
        let name = unsafe { OH_AVCapability_GetName(cap) };
        if name.is_null() {
            return Err(CapabilityError::InvalidNativeResult {
                api: "OH_AVCapability_GetName",
            });
        }
        // Native contract returns a NUL-terminated name, valid during query.
        let codec_name = unsafe { CStr::from_ptr(name) }
            .to_str()
            .map_err(|_| CapabilityError::InvalidNativeResult {
                api: "OH_AVCapability_GetName",
            })?
            .to_owned();
        if codec_name.is_empty() {
            return Err(CapabilityError::InvalidNativeResult {
                api: "OH_AVCapability_GetName",
            });
        }
        let profiles = list("OH_AVCapability_GetSupportedProfiles", |p, n| unsafe {
            OH_AVCapability_GetSupportedProfiles(cap, p, n)
        });
        let main10_advertised = profiles
            .as_ref()
            .map(|p| hevc && p.contains(&HEVC_MAIN10))
            .map_err(Clone::clone);
        let main10_levels = match &main10_advertised {
            Ok(true) => list(
                "OH_AVCapability_GetSupportedLevelsForProfile",
                |p, n| unsafe {
                    OH_AVCapability_GetSupportedLevelsForProfile(cap, HEVC_MAIN10, p, n)
                },
            )
            .map(Some),
            Ok(false) => Ok(None),
            Err(error) => Err(error.clone()),
        };
        let native_buffer_formats = list(
            "OH_AVCapability_GetVideoSupportedNativeBufferFormats",
            |p, n| unsafe { OH_AVCapability_GetVideoSupportedNativeBufferFormats(cap, p, n) },
        );
        let pixel_formats = list(
            "OH_AVCapability_GetVideoSupportedPixelFormats",
            |p, n| unsafe { OH_AVCapability_GetVideoSupportedPixelFormats(cap, p, n) },
        );
        let width_alignment = alignment("OH_AVCapability_GetVideoWidthAlignment", |p| unsafe {
            OH_AVCapability_GetVideoWidthAlignment(cap, p)
        });
        let height_alignment = alignment("OH_AVCapability_GetVideoHeightAlignment", |p| unsafe {
            OH_AVCapability_GetVideoHeightAlignment(cap, p)
        });
        let sizes = TARGETS_120.map(|target| {
            let size_supported =
                unsafe { OH_AVCapability_IsVideoSizeSupported(cap, target.width, target.height) };
            let api = "OH_AVCapability_GetVideoFrameRateRangeForSize";
            let mut range = Range::default();
            let code = unsafe {
                OH_AVCapability_GetVideoFrameRateRangeForSize(
                    cap,
                    target.width,
                    target.height,
                    &mut range,
                )
            };
            let frame_rate_range = check(api, code).and_then(|()| {
                if range.min < 0 || range.max < range.min {
                    Err(CapabilityError::InvalidNativeResult { api })
                } else {
                    Ok(AdvertisedRange {
                        min: range.min,
                        max: range.max,
                    })
                }
            });
            let size_and_rate_supported = unsafe {
                OH_AVCapability_AreVideoSizeAndFrameRateSupported(
                    cap,
                    target.width,
                    target.height,
                    target.fps,
                )
            };
            AdvertisedSizeRate {
                target,
                size_supported,
                frame_rate_range,
                size_and_rate_supported,
            }
        });
        Ok(AdvertisedCodec {
            direction,
            mime: if hevc { "video/hevc" } else { "video/avc" },
            codec_name,
            hardware: true,
            profiles,
            main10_advertised,
            main10_levels,
            native_buffer_formats,
            pixel_formats,
            width_alignment,
            height_alignment,
            sizes,
        })
    }
}
