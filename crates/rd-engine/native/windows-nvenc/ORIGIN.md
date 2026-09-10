# Origin and provenance

This directory contains a minimal direct NVENC shim. No AMD, Intel MFX, FFmpeg, or NVIDIA sample-wrapper source is included.

## Local upstream checkout

- Repository checkout: `/Users/frankhan/.cargo/git/checkouts/hwcodec-3f3da9ff8e484625/778df1f`
- Exact upstream commit: `778df1f99597722473b29443bac22ae6c23946fe`

## Copied file

- Local source: `externals/nv-codec-headers_n12.1.14.0/include/ffnvcodec/nvEncodeAPI.h`
- Destination: `include/nvEncodeAPI.h`
- Upstream package/version: `nv-codec-headers_n12.1.14.0` / NVENC API 12.1

## Consulted but not copied

- `externals/Video_Codec_SDK_12.1.14/Samples/NvCodec/NvEncoder/NvEncoder.cpp`
- `externals/Video_Codec_SDK_12.1.14/Samples/NvCodec/NvEncoder/NvEncoderD3D11.cpp`

The shim implementation (`windows_nvenc.cpp` and `windows_nvenc.h`) is new project code. The copied NVIDIA header retains its upstream copyright and license text.
