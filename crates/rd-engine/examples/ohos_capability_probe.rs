//! Standalone device check of the exact production metadata provider.
//! No N-API, codec instances, capture, pixel access or display changes.
#![allow(dead_code)]

#[path = "../src/media_capability.rs"]
mod media_capability;

use media_capability::{AdvertisedCodec, CapabilityError};

fn print_codec(direction: &str, codec: Result<AdvertisedCodec, CapabilityError>) {
    match codec {
        Err(error) => println!("direction={direction} error={error:?}"),
        Ok(codec) => {
            println!(
                "direction={direction} name={:?} hardware={} mime={}",
                codec.codec_name, codec.hardware, codec.mime
            );
            println!(
                "direction={direction} profiles={:?} main10={:?} main10_levels={:?}",
                codec.profiles, codec.main10_advertised, codec.main10_levels
            );
            println!(
                "direction={direction} native_buffer_formats={:?} pixel_formats={:?} alignment={:?}/{:?}",
                codec.native_buffer_formats,
                codec.pixel_formats,
                codec.width_alignment,
                codec.height_alignment
            );
            for size in codec.sizes {
                println!(
                    "direction={direction} width={} height={} fps={} size_supported={} range={:?} size_rate_supported={}",
                    size.target.width,
                    size.target.height,
                    size.target.fps,
                    size.size_supported,
                    size.frame_rate_range,
                    size.size_and_rate_supported
                );
            }
        }
    }
}

fn main() {
    println!("RD_ENGINE_CAPABILITY_PROBE_BEGIN evidence=advertised_only");
    let report = media_capability::query_hevc_hardware_capabilities();
    print_codec("encode", report.encoder);
    print_codec("decode", report.decoder);
    println!("capture={:?}", report.screen_capture_rate);
    println!(
        "codec_throughput={:?} hdr_display={:?} screen_capture_hdr={:?}",
        report.codec_throughput, report.hdr_display, report.screen_capture_hdr
    );
    println!("RD_ENGINE_CAPABILITY_PROBE_COMPLETED");
}
