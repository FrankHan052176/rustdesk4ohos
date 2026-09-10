#ifndef RD_DESKTOP_DUPLICATION_H_
#define RD_DESKTOP_DUPLICATION_H_

#include <stddef.h>
#include <stdint.h>

#if defined(_WIN32)
#define RD_DD_CALL __cdecl
#if defined(RD_DD_BUILD)
#define RD_DD_API __declspec(dllexport)
#else
#define RD_DD_API __declspec(dllimport)
#endif
#else
#define RD_DD_CALL
#define RD_DD_API
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef struct rd_dd_capture rd_dd_capture;
typedef struct rd_dd_lease rd_dd_lease;

typedef enum rd_dd_status {
  RD_DD_OK = 0,
  RD_DD_WOULD_BLOCK = 1,
  RD_DD_ACCESS_LOST = 2,
  RD_DD_GEOMETRY_CHANGED = 3,
  RD_DD_UNSUPPORTED_FORMAT = 4,
  RD_DD_LEASE_OUTSTANDING = 5,
  RD_DD_RECLAMATION_UNCERTAIN = 6,
  RD_DD_NOT_FOUND = 7,
  RD_DD_INVALID_ARGUMENT = 8,
  RD_DD_UNSUPPORTED = 9,
  RD_DD_D3D_FAILURE = 10,
  RD_DD_COLOR_CHANGED = 11
} rd_dd_status;

typedef enum rd_dd_pixel_format {
  RD_DD_FORMAT_UNKNOWN = 0,
  RD_DD_FORMAT_BGRA8_UNORM = 1,
  RD_DD_FORMAT_R10G10B10A2_UNORM = 2
} rd_dd_pixel_format;

typedef struct rd_dd_luid {
  uint32_t low_part;
  int32_t high_part;
} rd_dd_luid;

typedef struct rd_dd_output_id {
  rd_dd_luid adapter_luid;
  uint32_t output_index;
  uint32_t reserved;
} rd_dd_output_id;

typedef struct rd_dd_output_info {
  rd_dd_output_id id;
  uint32_t attached_to_desktop;
  uint32_t rotation;
  int32_t desktop_left;
  int32_t desktop_top;
  int32_t desktop_right;
  int32_t desktop_bottom;
  uint16_t device_name[32];
  /* Raw IDXGIOutput6::GetDesc1 display capability values. These luminance
   * values are not HDR10 content mastering metadata or MaxCLL/MaxFALL. */
  uint32_t dxgi_color_space_type;
  uint32_t bits_per_color;
  float display_min_luminance_nits;
  float display_max_luminance_nits;
  float display_max_full_frame_luminance_nits;
  uint32_t reserved_color;
} rd_dd_output_info;

typedef struct rd_dd_open_options {
  uint32_t struct_size;
  uint32_t flags;
  rd_dd_output_id output;
} rd_dd_open_options;

typedef struct rd_dd_frame {
  uint32_t struct_size;
  uint32_t pixel_format;
  uint32_t width;
  uint32_t height;
  rd_dd_luid adapter_luid;
  /* Actual DXGI LastPresentTime only; metadata-only acquisitions are not
   * emitted. accumulated_frames is DXGI's source-side coalescing count. */
  uint64_t qpc_timestamp;
  uint64_t qpc_frequency;
  uint32_t qpc_is_dxgi_present_time;
  uint32_t accumulated_frames;
  /* Open-time display capability snapshot; not content metadata. */
  uint32_t dxgi_color_space_type;
  uint32_t bits_per_color;
  float display_min_luminance_nits;
  float display_max_luminance_nits;
  float display_max_full_frame_luminance_nits;
  uint32_t reserved;
  void* d3d11_texture2d;
  void* d3d11_device;
  rd_dd_lease* lease;
} rd_dd_frame;

typedef struct rd_dd_capture_info {
  uint32_t struct_size;
  uint32_t pixel_format;
  uint32_t width;
  uint32_t height;
  rd_dd_luid adapter_luid;
  uint64_t qpc_frequency;
  uint32_t dxgi_color_space_type;
  uint32_t bits_per_color;
  float display_min_luminance_nits;
  float display_max_luminance_nits;
  float display_max_full_frame_luminance_nits;
  uint32_t reserved;
  /* Borrowed ID3D11Device. Valid until rd_dd_close succeeds. */
  void* d3d11_device;
} rd_dd_capture_info;

/* Enumerates attached physical outputs. Pass outputs=NULL/capacity=0 to query.
 * RD_DD_OK is returned and count always receives the required total. */
RD_DD_API rd_dd_status RD_DD_CALL rd_dd_enumerate_outputs(
    rd_dd_output_info* outputs, uint32_t capacity, uint32_t* count);
/* Supports RGB full-range G22/P709 SDR and RGB full-range PQ/P2020 HDR only.
 * PQ requires BitsPerColor >= 10 and an actual R10 duplication surface. */
RD_DD_API rd_dd_status RD_DD_CALL rd_dd_open(
    const rd_dd_open_options* options, rd_dd_capture** capture);
RD_DD_API rd_dd_status RD_DD_CALL rd_dd_get_capture_info(
    rd_dd_capture* capture, rd_dd_capture_info* info);
RD_DD_API rd_dd_status RD_DD_CALL rd_dd_acquire(
    rd_dd_capture* capture, uint32_t timeout_ms, rd_dd_frame* frame);
/* Terminal DXGI duplication/device-loss failures invalidate the whole graph and
 * consume/release the lease, so this function returns OK: the lease pointer is
 * invalid after OK and must not be retried. Subsequent capture operations
 * report ACCESS_LOST. DXGI_ERROR_INVALID_CALL is also treated as a safe
 * no-frame state: it consumes the lease, latches ACCESS_LOST, and returns OK.
 * Other failures return RECLAMATION_UNCERTAIN without consuming the lease;
 * retry rd_dd_release with that exact same lease pointer. */
RD_DD_API rd_dd_status RD_DD_CALL rd_dd_release(rd_dd_lease* lease);
/* Close never consumes a public outstanding lease. For an internally retained
 * frame it retries ReleaseFrame once and closes if reclamation is then safe. */
RD_DD_API rd_dd_status RD_DD_CALL rd_dd_close(rd_dd_capture* capture);
RD_DD_API int32_t RD_DD_CALL rd_dd_last_hresult(const rd_dd_capture* capture);

#ifdef __cplusplus
}  /* extern "C" */

#include <type_traits>
static_assert(sizeof(rd_dd_luid) == 8, "rd_dd_luid ABI");
static_assert(sizeof(rd_dd_output_id) == 16, "rd_dd_output_id ABI");
static_assert(sizeof(rd_dd_output_info) == 128, "rd_dd_output_info ABI");
static_assert(offsetof(rd_dd_output_info, dxgi_color_space_type) == 104,
              "rd_dd_output_info ABI");
static_assert(offsetof(rd_dd_output_info,
                       display_max_full_frame_luminance_nits) == 120,
              "rd_dd_output_info ABI");
static_assert(sizeof(rd_dd_open_options) == 24, "rd_dd_open_options ABI");
static_assert(std::is_standard_layout<rd_dd_frame>::value, "rd_dd_frame layout");
static_assert(offsetof(rd_dd_frame, adapter_luid) == 16, "rd_dd_frame ABI");
static_assert(offsetof(rd_dd_frame, qpc_timestamp) == 24, "rd_dd_frame ABI");
static_assert(offsetof(rd_dd_frame, accumulated_frames) == 44,
              "rd_dd_frame ABI");
static_assert(offsetof(rd_dd_frame, dxgi_color_space_type) == 48,
              "rd_dd_frame ABI");
static_assert(offsetof(rd_dd_frame, display_min_luminance_nits) == 56,
              "rd_dd_frame ABI");
static_assert(offsetof(rd_dd_frame, d3d11_texture2d) == 72, "rd_dd_frame ABI");
#if INTPTR_MAX == INT64_MAX
static_assert(sizeof(rd_dd_frame) == 96, "rd_dd_frame x64 ABI");
#endif
static_assert(std::is_standard_layout<rd_dd_capture_info>::value,
              "rd_dd_capture_info layout");
static_assert(offsetof(rd_dd_capture_info, adapter_luid) == 16,
              "rd_dd_capture_info ABI");
static_assert(offsetof(rd_dd_capture_info, qpc_frequency) == 24,
              "rd_dd_capture_info ABI");
static_assert(offsetof(rd_dd_capture_info, dxgi_color_space_type) == 32,
              "rd_dd_capture_info ABI");
static_assert(offsetof(rd_dd_capture_info, d3d11_device) == 56,
              "rd_dd_capture_info ABI");
#if INTPTR_MAX == INT64_MAX
static_assert(sizeof(rd_dd_capture_info) == 64,
              "rd_dd_capture_info x64 ABI");
#endif
#endif

#endif  /* RD_DESKTOP_DUPLICATION_H_ */
