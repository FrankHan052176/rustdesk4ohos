#ifndef RD_WINDOWS_NVENC_H
#define RD_WINDOWS_NVENC_H
#include <stdint.h>
#if defined(_WIN32)
#if defined(RD_WINDOWS_NVENC_BUILD)
#define RD_NVENC_API __declspec(dllexport)
#else
#define RD_NVENC_API __declspec(dllimport)
#endif
#define RD_NVENC_CALL __cdecl
#else
#define RD_NVENC_API
#define RD_NVENC_CALL
#endif
#ifdef __cplusplus
extern "C" {
#endif
typedef struct rd_nvenc_encoder rd_nvenc_encoder;
typedef enum rd_nvenc_status {
    RD_NVENC_OK = 0,
    RD_NVENC_INVALID_ARGUMENT = 1,
    RD_NVENC_UNSUPPORTED = 2,
    RD_NVENC_DLL_NOT_FOUND = 3,
    RD_NVENC_DRIVER_TOO_OLD = 4,
    RD_NVENC_NVENC_ERROR = 5,
    RD_NVENC_D3D11_ERROR = 6,
    RD_NVENC_OUT_OF_MEMORY = 7,
    RD_NVENC_BUSY = 8,
    RD_NVENC_CLEANUP_FAILED = 9,
    RD_NVENC_INTERNAL_ERROR = 10
} rd_nvenc_status;
typedef enum rd_nvenc_codec {
    RD_NVENC_CODEC_H264 = 1,
    RD_NVENC_CODEC_H265 = 2
} rd_nvenc_codec;
typedef enum rd_nvenc_input_format {
    RD_NVENC_INPUT_NV12 = 1,
    RD_NVENC_INPUT_P010 = 2,
    RD_NVENC_INPUT_BGRA8 = 3,
    RD_NVENC_INPUT_RGBA8 = 4,
    RD_NVENC_INPUT_RGB10A2 = 5
} rd_nvenc_input_format;
typedef struct rd_nvenc_color_config {
    uint32_t enabled, video_full_range, color_primaries, transfer_characteristics,
        matrix_coefficients, video_format;
} rd_nvenc_color_config;
#define RD_NVENC_HDR_PRIMARY_GREEN_INDEX 0u
#define RD_NVENC_HDR_PRIMARY_BLUE_INDEX 1u
#define RD_NVENC_HDR_PRIMARY_RED_INDEX 2u

typedef struct rd_nvenc_hdr_static_metadata {
    uint32_t enabled;
    /* HEVC mastering_display_colour_volume wire order: [0]=green, [1]=blue, [2]=red. */
    uint16_t display_primaries_x_gbr[3];
    uint16_t display_primaries_y_gbr[3];
    uint16_t white_point_x, white_point_y;
    uint32_t max_display_mastering_luminance, min_display_mastering_luminance;
    uint16_t max_content_light_level, max_frame_average_light_level;
} rd_nvenc_hdr_static_metadata;
typedef struct rd_nvenc_create_desc {
    uint32_t struct_size, codec, input_format, width, height, fps_num, fps_den, bitrate_bps,
        gop_length;
    rd_nvenc_color_config color;
    rd_nvenc_hdr_static_metadata hdr_static;
} rd_nvenc_create_desc;
typedef struct rd_nvenc_output_loan {
    uint32_t struct_size;
    /* Direct pointer into NVIDIA's locked bitstream buffer; never owned by the caller. */
    const uint8_t *data;
    uint64_t size, timestamp;
    uint32_t picture_type, reserved;
} rd_nvenc_output_loan;
/* CLEANUP_FAILED may return a non-null handle; call rd_nvenc_shutdown to retry cleanup. */
RD_NVENC_API rd_nvenc_status RD_NVENC_CALL rd_nvenc_create(void *d3d11_device,
                                                           const rd_nvenc_create_desc *desc,
                                                           rd_nvenc_encoder **out_encoder);
/*
 * On success, the NVIDIA bitstream remains locked and the input texture remains mapped and
 * registered. The caller must keep its ID3D11Texture2D capture lease alive and must not mutate,
 * release, or reuse that texture until rd_nvenc_release_output succeeds.
 */
RD_NVENC_API rd_nvenc_status RD_NVENC_CALL rd_nvenc_encode_texture(rd_nvenc_encoder *encoder,
                                                                   void *d3d11_texture,
                                                                   uint64_t timestamp,
                                                                   uint32_t force_idr_and_headers,
                                                                   rd_nvenc_output_loan *out_loan);
/*
 * Staged and retryable: unlock, then unmap, then unregister. CLEANUP_FAILED retains the loan
 * identity and all unfinished stages; retry with the same loan until the call succeeds. Once
 * the unlock stage succeeds, loan.data is no longer readable even if a later stage fails.
 */
RD_NVENC_API rd_nvenc_status RD_NVENC_CALL
rd_nvenc_release_output(rd_nvenc_encoder *encoder, const rd_nvenc_output_loan *loan);
/* Refuses an outstanding or partially released loan; finish release_output first. */
RD_NVENC_API rd_nvenc_status RD_NVENC_CALL rd_nvenc_shutdown(rd_nvenc_encoder *encoder);
RD_NVENC_API const char *RD_NVENC_CALL rd_nvenc_last_error(void);
#ifdef __cplusplus
}
static_assert(sizeof(rd_nvenc_color_config) == 24, "rd_nvenc_color_config ABI");
static_assert(sizeof(rd_nvenc_hdr_static_metadata) == 32, "rd_nvenc_hdr_static_metadata ABI");
static_assert(sizeof(rd_nvenc_create_desc) == 92, "rd_nvenc_create_desc ABI");
#if INTPTR_MAX == INT64_MAX
static_assert(sizeof(rd_nvenc_output_loan) == 40, "rd_nvenc_output_loan ABI");
#else
static_assert(sizeof(rd_nvenc_output_loan) == 32, "rd_nvenc_output_loan ABI");
#endif
#endif
#endif
