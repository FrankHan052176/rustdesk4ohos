#define RD_DD_BUILD
#include "rd_desktop_duplication.h"

#if !defined(_WIN32)
#error "rd_desktop_duplication.cpp is Windows-only"
#endif

#include <d3d11.h>
#include <dxgi1_6.h>
#include <windows.h>

#include <cstring>
#include <mutex>
#include <new>

namespace {

template <typename T>
void release_com(T*& value) {
  if (value != nullptr) {
    value->Release();
    value = nullptr;
  }
}

bool same_luid(const LUID& a, const rd_dd_luid& b) {
  return a.LowPart == b.low_part && a.HighPart == b.high_part;
}

rd_dd_luid abi_luid(const LUID& value) {
  return {value.LowPart, value.HighPart};
}

bool rotation_needs_copy(DXGI_MODE_ROTATION rotation) {
  return rotation == DXGI_MODE_ROTATION_ROTATE90 ||
         rotation == DXGI_MODE_ROTATION_ROTATE180 ||
         rotation == DXGI_MODE_ROTATION_ROTATE270;
}

bool same_display_color_capability(const DXGI_OUTPUT_DESC1& a,
                                   const DXGI_OUTPUT_DESC1& b) {
  return a.ColorSpace == b.ColorSpace && a.BitsPerColor == b.BitsPerColor &&
         a.MinLuminance == b.MinLuminance &&
         a.MaxLuminance == b.MaxLuminance &&
         a.MaxFullFrameLuminance == b.MaxFullFrameLuminance;
}

rd_dd_pixel_format abi_format(DXGI_FORMAT format) {
  switch (format) {
    case DXGI_FORMAT_B8G8R8A8_UNORM:
      return RD_DD_FORMAT_BGRA8_UNORM;
    case DXGI_FORMAT_R10G10B10A2_UNORM:
      return RD_DD_FORMAT_R10G10B10A2_UNORM;
    default:
      return RD_DD_FORMAT_UNKNOWN;
  }
}

rd_dd_status status_from_hresult(HRESULT hr) {
  if (hr == DXGI_ERROR_WAIT_TIMEOUT) return RD_DD_WOULD_BLOCK;
  if (hr == DXGI_ERROR_ACCESS_LOST || hr == DXGI_ERROR_SESSION_DISCONNECTED ||
      hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET) {
    return RD_DD_ACCESS_LOST;
  }
  if (hr == DXGI_ERROR_NOT_FOUND) return RD_DD_NOT_FOUND;
  if (hr == E_INVALIDARG || hr == E_POINTER) return RD_DD_INVALID_ARGUMENT;
  if (hr == E_NOINTERFACE || hr == E_NOTIMPL || hr == DXGI_ERROR_UNSUPPORTED) {
    return RD_DD_UNSUPPORTED;
  }
  return RD_DD_D3D_FAILURE;
}

bool is_terminal_duplication_invalidation(HRESULT hr) {
  return hr == DXGI_ERROR_ACCESS_LOST ||
         hr == DXGI_ERROR_SESSION_DISCONNECTED ||
         hr == DXGI_ERROR_DEVICE_REMOVED || hr == DXGI_ERROR_DEVICE_RESET;
}

struct LocatedOutput {
  IDXGIAdapter1* adapter = nullptr;
  IDXGIOutput* output = nullptr;
  DXGI_ADAPTER_DESC1 adapter_desc{};
  DXGI_OUTPUT_DESC output_desc{};
};

void release_located(LocatedOutput& located) {
  release_com(located.output);
  release_com(located.adapter);
}

HRESULT locate_output(const rd_dd_output_id& wanted, LocatedOutput* found) {
  IDXGIFactory1* factory = nullptr;
  HRESULT hr = CreateDXGIFactory1(__uuidof(IDXGIFactory1),
                                  reinterpret_cast<void**>(&factory));
  if (FAILED(hr)) return hr;

  for (UINT adapter_index = 0;; ++adapter_index) {
    IDXGIAdapter1* adapter = nullptr;
    hr = factory->EnumAdapters1(adapter_index, &adapter);
    if (hr == DXGI_ERROR_NOT_FOUND) break;
    if (FAILED(hr)) {
      factory->Release();
      return hr;
    }

    DXGI_ADAPTER_DESC1 adapter_desc{};
    hr = adapter->GetDesc1(&adapter_desc);
    if (FAILED(hr)) {
      adapter->Release();
      factory->Release();
      return hr;
    }
    if (!same_luid(adapter_desc.AdapterLuid, wanted.adapter_luid)) {
      adapter->Release();
      continue;
    }

    IDXGIOutput* output = nullptr;
    hr = adapter->EnumOutputs(wanted.output_index, &output);
    if (SUCCEEDED(hr)) {
      DXGI_OUTPUT_DESC output_desc{};
      hr = output->GetDesc(&output_desc);
      if (SUCCEEDED(hr) && output_desc.AttachedToDesktop) {
        found->adapter = adapter;
        found->output = output;
        found->adapter_desc = adapter_desc;
        found->output_desc = output_desc;
        factory->Release();
        return S_OK;
      }
      output->Release();
    }
    adapter->Release();
    factory->Release();
    return FAILED(hr) ? hr : DXGI_ERROR_NOT_FOUND;
  }

  factory->Release();
  return DXGI_ERROR_NOT_FOUND;
}

}  // namespace

struct rd_dd_capture {
  std::mutex mutex;
  IDXGIAdapter1* adapter = nullptr;
  IDXGIOutput* output = nullptr;
  IDXGIOutput6* output6 = nullptr;
  ID3D11Device* device = nullptr;
  ID3D11DeviceContext* context = nullptr;
  IDXGIOutputDuplication* duplication = nullptr;
  DXGI_OUTDUPL_DESC duplication_desc{};
  rd_dd_output_id output_id{};
  RECT desktop_rect{};
  DXGI_MODE_ROTATION rotation = DXGI_MODE_ROTATION_UNSPECIFIED;
  DXGI_OUTPUT_DESC1 color_desc{};
  uint64_t qpc_frequency = 0;
  HRESULT last_hr = S_OK;
  rd_dd_lease* outstanding = nullptr;
  IUnknown* uncertain_resource = nullptr;
  bool access_lost = false;
  bool reclamation_uncertain = false;
};

struct rd_dd_lease {
  rd_dd_capture* owner = nullptr;
  IDXGIOutputDuplication* duplication = nullptr;
  IDXGIAdapter1* adapter = nullptr;
  ID3D11Device* device = nullptr;
  ID3D11Texture2D* texture = nullptr;
};

void release_lease_com_refs(rd_dd_lease* lease) {
  release_com(lease->texture);
  release_com(lease->device);
  release_com(lease->adapter);
  release_com(lease->duplication);
}

bool is_safe_no_frame_state(HRESULT hr) {
  return hr == DXGI_ERROR_INVALID_CALL;
}

rd_dd_status retry_internal_reclamation(rd_dd_capture* capture) {
  if (!capture->reclamation_uncertain || capture->outstanding != nullptr ||
      capture->uncertain_resource == nullptr) {
    return RD_DD_OK;
  }
  const HRESULT hr = capture->duplication->ReleaseFrame();
  capture->last_hr = hr;
  if (hr == S_OK) {
    release_com(capture->uncertain_resource);
    capture->reclamation_uncertain = false;
    return RD_DD_OK;
  }
  if (is_terminal_duplication_invalidation(hr) ||
      is_safe_no_frame_state(hr)) {
    release_com(capture->uncertain_resource);
    capture->reclamation_uncertain = false;
    capture->access_lost = true;
    return RD_DD_ACCESS_LOST;
  }
  return RD_DD_RECLAMATION_UNCERTAIN;
}

extern "C" rd_dd_status RD_DD_CALL rd_dd_enumerate_outputs(
    rd_dd_output_info* outputs, uint32_t capacity, uint32_t* count) {
  if (count == nullptr || (capacity != 0 && outputs == nullptr)) {
    return RD_DD_INVALID_ARGUMENT;
  }
  *count = 0;
  IDXGIFactory1* factory = nullptr;
  HRESULT hr = CreateDXGIFactory1(__uuidof(IDXGIFactory1),
                                  reinterpret_cast<void**>(&factory));
  if (FAILED(hr)) return status_from_hresult(hr);

  uint32_t total = 0;
  for (UINT adapter_index = 0;; ++adapter_index) {
    IDXGIAdapter1* adapter = nullptr;
    hr = factory->EnumAdapters1(adapter_index, &adapter);
    if (hr == DXGI_ERROR_NOT_FOUND) break;
    if (FAILED(hr)) {
      factory->Release();
      return status_from_hresult(hr);
    }
    DXGI_ADAPTER_DESC1 adapter_desc{};
    hr = adapter->GetDesc1(&adapter_desc);
    if (FAILED(hr)) {
      adapter->Release();
      factory->Release();
      return status_from_hresult(hr);
    }
    for (UINT output_index = 0;; ++output_index) {
      IDXGIOutput* output = nullptr;
      hr = adapter->EnumOutputs(output_index, &output);
      if (hr == DXGI_ERROR_NOT_FOUND) break;
      if (FAILED(hr)) {
        adapter->Release();
        factory->Release();
        return status_from_hresult(hr);
      }
      DXGI_OUTPUT_DESC desc{};
      hr = output->GetDesc(&desc);
      if (FAILED(hr)) {
        output->Release();
        adapter->Release();
        factory->Release();
        return status_from_hresult(hr);
      }
      if (!desc.AttachedToDesktop) {
        output->Release();
        continue;
      }
      IDXGIOutput6* output6 = nullptr;
      hr = output->QueryInterface(__uuidof(IDXGIOutput6),
                                  reinterpret_cast<void**>(&output6));
      output->Release();
      if (FAILED(hr)) {
        adapter->Release();
        factory->Release();
        return status_from_hresult(hr);
      }
      DXGI_OUTPUT_DESC1 desc1{};
      hr = output6->GetDesc1(&desc1);
      output6->Release();
      if (FAILED(hr)) {
        adapter->Release();
        factory->Release();
        return status_from_hresult(hr);
      }
      if (outputs != nullptr && total < capacity) {
        rd_dd_output_info& info = outputs[total];
        std::memset(&info, 0, sizeof(info));
        info.id.adapter_luid = abi_luid(adapter_desc.AdapterLuid);
        info.id.output_index = output_index;
        info.attached_to_desktop = 1;
        info.rotation = static_cast<uint32_t>(desc.Rotation);
        info.desktop_left = desc.DesktopCoordinates.left;
        info.desktop_top = desc.DesktopCoordinates.top;
        info.desktop_right = desc.DesktopCoordinates.right;
        info.desktop_bottom = desc.DesktopCoordinates.bottom;
        std::memcpy(info.device_name, desc.DeviceName,
                    sizeof(info.device_name));
        info.dxgi_color_space_type = static_cast<uint32_t>(desc1.ColorSpace);
        info.bits_per_color = desc1.BitsPerColor;
        info.display_min_luminance_nits = desc1.MinLuminance;
        info.display_max_luminance_nits = desc1.MaxLuminance;
        info.display_max_full_frame_luminance_nits =
            desc1.MaxFullFrameLuminance;
      }
      ++total;
    }
    adapter->Release();
  }
  factory->Release();
  *count = total;
  return RD_DD_OK;
}

extern "C" rd_dd_status RD_DD_CALL rd_dd_open(
    const rd_dd_open_options* options, rd_dd_capture** capture_out) {
  if (options == nullptr || capture_out == nullptr ||
      options->struct_size != sizeof(rd_dd_open_options) ||
      options->flags != 0 || options->output.reserved != 0) {
    return RD_DD_INVALID_ARGUMENT;
  }
  *capture_out = nullptr;
  LocatedOutput located;
  HRESULT hr = locate_output(options->output, &located);
  if (FAILED(hr)) return status_from_hresult(hr);
  if ((located.adapter_desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE) != 0) {
    release_located(located);
    return RD_DD_UNSUPPORTED;
  }
  if (rotation_needs_copy(located.output_desc.Rotation)) {
    release_located(located);
    return RD_DD_GEOMETRY_CHANGED;
  }

  auto* capture = new (std::nothrow) rd_dd_capture();
  if (capture == nullptr) {
    release_located(located);
    return RD_DD_D3D_FAILURE;
  }
  capture->adapter = located.adapter;
  capture->output = located.output;
  capture->output_id = options->output;
  capture->desktop_rect = located.output_desc.DesktopCoordinates;
  capture->rotation = located.output_desc.Rotation;
  hr = capture->output->QueryInterface(__uuidof(IDXGIOutput6),
                                       reinterpret_cast<void**>(&capture->output6));
  if (FAILED(hr)) {
    capture->last_hr = hr;
    rd_dd_close(capture);
    return status_from_hresult(hr);
  }
  hr = capture->output6->GetDesc1(&capture->color_desc);
  if (FAILED(hr)) {
    capture->last_hr = hr;
    rd_dd_close(capture);
    return status_from_hresult(hr);
  }
  const bool is_pq_hdr =
      capture->color_desc.ColorSpace ==
      DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
  const bool is_sdr =
      capture->color_desc.ColorSpace ==
      DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709;
  if (!is_pq_hdr && !is_sdr) {
    rd_dd_close(capture);
    return RD_DD_UNSUPPORTED;
  }
  if (is_pq_hdr && capture->color_desc.BitsPerColor < 10) {
    rd_dd_close(capture);
    return RD_DD_UNSUPPORTED;
  }

  D3D_FEATURE_LEVEL chosen{};
  const D3D_FEATURE_LEVEL levels[] = {D3D_FEATURE_LEVEL_11_1,
                                     D3D_FEATURE_LEVEL_11_0};
  hr = D3D11CreateDevice(capture->adapter, D3D_DRIVER_TYPE_UNKNOWN, nullptr,
                         D3D11_CREATE_DEVICE_BGRA_SUPPORT, levels,
                         ARRAYSIZE(levels), D3D11_SDK_VERSION, &capture->device,
                         &chosen, &capture->context);
  if (hr == E_INVALIDARG) {
    hr = D3D11CreateDevice(capture->adapter, D3D_DRIVER_TYPE_UNKNOWN, nullptr,
                           D3D11_CREATE_DEVICE_BGRA_SUPPORT, levels + 1, 1,
                           D3D11_SDK_VERSION, &capture->device, &chosen,
                           &capture->context);
  }
  if (FAILED(hr)) {
    capture->last_hr = hr;
    rd_dd_close(capture);
    return status_from_hresult(hr);
  }

  IDXGIOutput5* output5 = nullptr;
  hr = capture->output->QueryInterface(__uuidof(IDXGIOutput5),
                                       reinterpret_cast<void**>(&output5));
  bool try_legacy = is_sdr && hr == E_NOINTERFACE;
  if (SUCCEEDED(hr)) {
    if (is_pq_hdr) {
      const DXGI_FORMAT formats[] = {DXGI_FORMAT_R10G10B10A2_UNORM};
      hr = output5->DuplicateOutput1(capture->device, 0, ARRAYSIZE(formats),
                                     formats, &capture->duplication);
    } else {
      const DXGI_FORMAT formats[] = {DXGI_FORMAT_B8G8R8A8_UNORM,
                                     DXGI_FORMAT_R10G10B10A2_UNORM};
      hr = output5->DuplicateOutput1(capture->device, 0, ARRAYSIZE(formats),
                                     formats, &capture->duplication);
    }
    output5->Release();
    try_legacy = is_sdr &&
                 (hr == E_NOINTERFACE || hr == E_NOTIMPL ||
                  hr == DXGI_ERROR_UNSUPPORTED);
  }
  if (try_legacy) {
    IDXGIOutput1* output1 = nullptr;
    hr = capture->output->QueryInterface(__uuidof(IDXGIOutput1),
                                         reinterpret_cast<void**>(&output1));
    if (SUCCEEDED(hr)) {
      hr = output1->DuplicateOutput(capture->device, &capture->duplication);
      output1->Release();
    }
  }
  if (FAILED(hr)) {
    capture->last_hr = hr;
    rd_dd_close(capture);
    return status_from_hresult(hr);
  }

  capture->duplication->GetDesc(&capture->duplication_desc);
  const rd_dd_pixel_format actual_format =
      abi_format(capture->duplication_desc.ModeDesc.Format);
  if (actual_format == RD_DD_FORMAT_UNKNOWN ||
      (is_pq_hdr && actual_format != RD_DD_FORMAT_R10G10B10A2_UNORM)) {
    rd_dd_close(capture);
    return RD_DD_UNSUPPORTED_FORMAT;
  }
  const uint32_t desktop_width = static_cast<uint32_t>(
      capture->desktop_rect.right - capture->desktop_rect.left);
  const uint32_t desktop_height = static_cast<uint32_t>(
      capture->desktop_rect.bottom - capture->desktop_rect.top);
  if (capture->duplication_desc.ModeDesc.Width != desktop_width ||
      capture->duplication_desc.ModeDesc.Height != desktop_height) {
    rd_dd_close(capture);
    return RD_DD_GEOMETRY_CHANGED;
  }
  LARGE_INTEGER qpc_frequency{};
  if (!QueryPerformanceFrequency(&qpc_frequency) ||
      qpc_frequency.QuadPart <= 0) {
    const DWORD error = GetLastError();
    capture->last_hr = error == ERROR_SUCCESS ? E_FAIL
                                               : HRESULT_FROM_WIN32(error);
    rd_dd_close(capture);
    return RD_DD_D3D_FAILURE;
  }
  capture->qpc_frequency = static_cast<uint64_t>(qpc_frequency.QuadPart);

  capture->last_hr = S_OK;
  *capture_out = capture;
  return RD_DD_OK;
}

extern "C" rd_dd_status RD_DD_CALL rd_dd_get_capture_info(
    rd_dd_capture* capture, rd_dd_capture_info* info) {
  if (capture == nullptr || info == nullptr ||
      info->struct_size != sizeof(rd_dd_capture_info)) {
    return RD_DD_INVALID_ARGUMENT;
  }
  std::lock_guard<std::mutex> lock(capture->mutex);
  if (capture->access_lost) return RD_DD_ACCESS_LOST;
  const uint32_t caller_size = info->struct_size;
  std::memset(info, 0, sizeof(*info));
  info->struct_size = caller_size;
  info->pixel_format = static_cast<uint32_t>(
      abi_format(capture->duplication_desc.ModeDesc.Format));
  info->width = capture->duplication_desc.ModeDesc.Width;
  info->height = capture->duplication_desc.ModeDesc.Height;
  info->adapter_luid = capture->output_id.adapter_luid;
  info->qpc_frequency = capture->qpc_frequency;
  info->dxgi_color_space_type =
      static_cast<uint32_t>(capture->color_desc.ColorSpace);
  info->bits_per_color = capture->color_desc.BitsPerColor;
  info->display_min_luminance_nits = capture->color_desc.MinLuminance;
  info->display_max_luminance_nits = capture->color_desc.MaxLuminance;
  info->display_max_full_frame_luminance_nits =
      capture->color_desc.MaxFullFrameLuminance;
  info->d3d11_device = capture->device;
  return RD_DD_OK;
}

extern "C" rd_dd_status RD_DD_CALL rd_dd_acquire(
    rd_dd_capture* capture, uint32_t timeout_ms, rd_dd_frame* frame) {
  if (capture == nullptr || frame == nullptr ||
      frame->struct_size != sizeof(rd_dd_frame)) {
    return RD_DD_INVALID_ARGUMENT;
  }
  std::lock_guard<std::mutex> lock(capture->mutex);
  if (capture->access_lost) return RD_DD_ACCESS_LOST;
  if (capture->outstanding != nullptr) {
    return capture->reclamation_uncertain ? RD_DD_RECLAMATION_UNCERTAIN
                                          : RD_DD_LEASE_OUTSTANDING;
  }
  if (capture->reclamation_uncertain) {
    const rd_dd_status retry = retry_internal_reclamation(capture);
    if (retry != RD_DD_OK) return retry;
  }

  DXGI_OUTPUT_DESC1 current{};
  HRESULT hr = capture->output6->GetDesc1(&current);
  if (FAILED(hr)) {
    capture->last_hr = hr;
    if (is_terminal_duplication_invalidation(hr)) capture->access_lost = true;
    return status_from_hresult(hr);
  }
  if (!current.AttachedToDesktop || rotation_needs_copy(current.Rotation) ||
      current.Rotation != capture->rotation ||
      !EqualRect(&current.DesktopCoordinates, &capture->desktop_rect)) {
    return RD_DD_GEOMETRY_CHANGED;
  }
  if (!same_display_color_capability(current, capture->color_desc)) {
    return RD_DD_COLOR_CHANGED;
  }

  DXGI_OUTDUPL_FRAME_INFO info{};
  IDXGIResource* resource = nullptr;
  hr = capture->duplication->AcquireNextFrame(timeout_ms, &info, &resource);
  if (FAILED(hr)) {
    capture->last_hr = hr;
    if (is_terminal_duplication_invalidation(hr)) capture->access_lost = true;
    return status_from_hresult(hr);
  }

  /* Pointer/metadata-only acquisitions do not represent new desktop pixels.
   * Releasing them and reporting WOULD_BLOCK prevents duplicate-pixel emission
   * and prevents inventing a presentation timestamp. */
  if (info.AccumulatedFrames == 0 || info.LastPresentTime.QuadPart == 0) {
    HRESULT release_hr = capture->duplication->ReleaseFrame();
    capture->last_hr = release_hr;
    if (release_hr == S_OK) {
      resource->Release();
      capture->last_hr = S_OK;
      return RD_DD_WOULD_BLOCK;
    }
    if (is_terminal_duplication_invalidation(release_hr) ||
        is_safe_no_frame_state(release_hr)) {
      resource->Release();
      capture->access_lost = true;
      return RD_DD_ACCESS_LOST;
    }
    capture->reclamation_uncertain = true;
    capture->uncertain_resource = resource;
    return RD_DD_RECLAMATION_UNCERTAIN;
  }

  ID3D11Texture2D* texture = nullptr;
  hr = resource->QueryInterface(__uuidof(ID3D11Texture2D),
                                reinterpret_cast<void**>(&texture));
  D3D11_TEXTURE2D_DESC desc{};
  if (SUCCEEDED(hr)) texture->GetDesc(&desc);
  const uint32_t expected_width = capture->duplication_desc.ModeDesc.Width;
  const uint32_t expected_height = capture->duplication_desc.ModeDesc.Height;
  rd_dd_status validation = RD_DD_OK;
  if (FAILED(hr)) validation = status_from_hresult(hr);
  else if (abi_format(desc.Format) == RD_DD_FORMAT_UNKNOWN)
    validation = RD_DD_UNSUPPORTED_FORMAT;
  else if (desc.Width != expected_width || desc.Height != expected_height)
    validation = RD_DD_GEOMETRY_CHANGED;

  if (validation != RD_DD_OK) {
    HRESULT release_hr = capture->duplication->ReleaseFrame();
    capture->last_hr = release_hr;
    if (release_hr == S_OK) {
      release_com(texture);
      resource->Release();
      capture->last_hr = FAILED(hr) ? hr : S_OK;
      return validation;
    }
    if (is_terminal_duplication_invalidation(release_hr) ||
        is_safe_no_frame_state(release_hr)) {
      release_com(texture);
      resource->Release();
      capture->access_lost = true;
      return RD_DD_ACCESS_LOST;
    }
    capture->reclamation_uncertain = true;
    release_com(texture);
    capture->uncertain_resource = resource;
    return RD_DD_RECLAMATION_UNCERTAIN;
  }

  resource->Release();

  auto* lease = new (std::nothrow) rd_dd_lease();
  if (lease == nullptr) {
    HRESULT release_hr = capture->duplication->ReleaseFrame();
    capture->last_hr = release_hr;
    if (release_hr == S_OK) {
      texture->Release();
      return RD_DD_D3D_FAILURE;
    }
    if (is_terminal_duplication_invalidation(release_hr) ||
        is_safe_no_frame_state(release_hr)) {
      texture->Release();
      capture->access_lost = true;
      return RD_DD_ACCESS_LOST;
    }
    capture->reclamation_uncertain = true;
    capture->uncertain_resource = texture;
    return RD_DD_RECLAMATION_UNCERTAIN;
  }
  lease->owner = capture;
  lease->duplication = capture->duplication;
  lease->adapter = capture->adapter;
  lease->device = capture->device;
  lease->texture = texture;
  lease->duplication->AddRef();
  lease->adapter->AddRef();
  lease->device->AddRef();
  capture->outstanding = lease;

  const uint32_t caller_size = frame->struct_size;
  std::memset(frame, 0, sizeof(*frame));
  frame->struct_size = caller_size;
  frame->pixel_format = static_cast<uint32_t>(abi_format(desc.Format));
  frame->width = desc.Width;
  frame->height = desc.Height;
  frame->adapter_luid = capture->output_id.adapter_luid;
  frame->qpc_timestamp =
      static_cast<uint64_t>(info.LastPresentTime.QuadPart);
  frame->qpc_frequency = capture->qpc_frequency;
  frame->qpc_is_dxgi_present_time = 1;
  frame->accumulated_frames = info.AccumulatedFrames;
  frame->dxgi_color_space_type =
      static_cast<uint32_t>(capture->color_desc.ColorSpace);
  frame->bits_per_color = capture->color_desc.BitsPerColor;
  frame->display_min_luminance_nits = capture->color_desc.MinLuminance;
  frame->display_max_luminance_nits = capture->color_desc.MaxLuminance;
  frame->display_max_full_frame_luminance_nits =
      capture->color_desc.MaxFullFrameLuminance;
  frame->d3d11_texture2d = texture;
  frame->d3d11_device = capture->device;
  frame->lease = lease;
  capture->last_hr = S_OK;
  return RD_DD_OK;
}

extern "C" rd_dd_status RD_DD_CALL rd_dd_release(rd_dd_lease* lease) {
  if (lease == nullptr || lease->owner == nullptr) return RD_DD_INVALID_ARGUMENT;
  rd_dd_capture* capture = lease->owner;
  std::lock_guard<std::mutex> lock(capture->mutex);
  if (capture->outstanding != lease) return RD_DD_INVALID_ARGUMENT;
  HRESULT hr = lease->duplication->ReleaseFrame();
  capture->last_hr = hr;
  if (hr != S_OK && !is_terminal_duplication_invalidation(hr) &&
      !is_safe_no_frame_state(hr)) {
    capture->reclamation_uncertain = true;
    return RD_DD_RECLAMATION_UNCERTAIN;
  }
  capture->outstanding = nullptr;
  capture->reclamation_uncertain = false;
  if (hr != S_OK) capture->access_lost = true;
  release_lease_com_refs(lease);
  lease->owner = nullptr;
  delete lease;
  /* S_OK, documented terminal invalidation, and INVALID_CALL/no-frame all
   * consume the public lease. Returning OK is the caller's clear-owner signal;
   * terminal/no-frame state is observed on the next capture operation. */
  return RD_DD_OK;
}

extern "C" rd_dd_status RD_DD_CALL rd_dd_close(rd_dd_capture* capture) {
  if (capture == nullptr) return RD_DD_INVALID_ARGUMENT;
  {
    std::lock_guard<std::mutex> lock(capture->mutex);
    if (capture->outstanding != nullptr) {
      return capture->reclamation_uncertain ? RD_DD_RECLAMATION_UNCERTAIN
                                            : RD_DD_LEASE_OUTSTANDING;
    }
    if (capture->reclamation_uncertain) {
      const rd_dd_status retry = retry_internal_reclamation(capture);
      if (retry == RD_DD_RECLAMATION_UNCERTAIN) return retry;
    }
  }
  release_com(capture->duplication);
  release_com(capture->context);
  release_com(capture->device);
  release_com(capture->output6);
  release_com(capture->output);
  release_com(capture->adapter);
  delete capture;
  return RD_DD_OK;
}

extern "C" int32_t RD_DD_CALL rd_dd_last_hresult(
    const rd_dd_capture* capture) {
  return capture == nullptr ? E_INVALIDARG : capture->last_hr;
}

static_assert(sizeof(DXGI_FORMAT) == sizeof(uint32_t), "DXGI format ABI");
static_assert(sizeof(LUID) == sizeof(rd_dd_luid), "LUID ABI");
static_assert(offsetof(rd_dd_output_info, device_name) == 40,
              "output info ABI");
