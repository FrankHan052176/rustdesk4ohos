# Windows Desktop Duplication shim origin and boundary

This directory is a new, standalone Windows-only implementation written for the
modern runtime. It is not linked to, compiled with, or copied from RustDesk's
existing legacy `scrap` capture code. That legacy code may be consulted only as
behavioral reference for repository context; it is not an implementation source
for these files.

The implementation is based on Microsoft's documented Windows APIs:

- [Desktop Duplication API](https://learn.microsoft.com/windows/win32/direct3ddxgi/desktop-dup-api)
- [`IDXGIOutput1::DuplicateOutput`](https://learn.microsoft.com/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutput1-duplicateoutput)
- [`IDXGIOutput5::DuplicateOutput1`](https://learn.microsoft.com/windows/win32/api/dxgi1_5/nf-dxgi1_5-idxgioutput5-duplicateoutput1)
- [`IDXGIOutput6::GetDesc1`](https://learn.microsoft.com/windows/win32/api/dxgi1_6/nf-dxgi1_6-idxgioutput6-getdesc1)
- [`DXGI_OUTPUT_DESC1`](https://learn.microsoft.com/windows/win32/api/dxgi1_6/ns-dxgi1_6-dxgi_output_desc1)
- [`IDXGIOutputDuplication::AcquireNextFrame`](https://learn.microsoft.com/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutputduplication-acquirenextframe)
- [`IDXGIOutputDuplication::ReleaseFrame`](https://learn.microsoft.com/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutputduplication-releaseframe)
- [`D3D11CreateDevice`](https://learn.microsoft.com/windows/win32/api/d3d11/nf-d3d11-d3d11createdevice)
- [`DXGI_OUTDUPL_FRAME_INFO`](https://learn.microsoft.com/windows/win32/api/dxgi1_2/ns-dxgi1_2-dxgi_outdupl_frame_info)

## Deliberate boundaries

- Hardware D3D11 device creation uses the selected DXGI adapter and
  `D3D_DRIVER_TYPE_UNKNOWN`. There is no WARP, reference, GDI, or CPU fallback.
- `DuplicateOutput1` is preferred. The sole capture fallback is
  `DuplicateOutput`, only for supported SDR when the newer interface/operation
  is unavailable.
- Requested formats are selected from the open-time `GetDesc1` color space.
  `RGB_FULL_G2084_NONE_P2020` requires `BitsPerColor >= 10` and requests only
  `R10G10B10A2_UNORM`; the actual duplication mode must also be R10. There is no
  legacy `DuplicateOutput` fallback for this PQ HDR path because legacy
  duplication cannot guarantee an R10 surface, and silently opening BGRA8 would
  clip the HDR contract. `RGB_FULL_G22_NONE_P709` requests BGRA8 first and R10
  second and is the only color space allowed to use legacy `DuplicateOutput`.
  All other, including non-RGB, color spaces fail with `RD_DD_UNSUPPORTED`
  before duplication is attempted.
- The returned `ID3D11Texture2D*` is the exact resource obtained from the
  acquired duplication frame. It is borrowed by the caller for the lifetime of
  the returned lease; the lease owns the COM references.
- The device pointer in the frame is likewise borrowed for the lease lifetime.
- `rd_dd_get_capture_info` exposes the selected adapter LUID, actual duplication
  `ModeDesc`, QPC frequency, open-time display color snapshot, and a borrowed
  `ID3D11Device*` before any frame acquisition. This permits encoder creation
  without acquiring and discarding a bootstrap frame. The borrowed device
  remains valid until `rd_dd_close` succeeds; a failed close retains it.
- Both `DuplicateOutput1` and legacy `DuplicateOutput` paths validate the actual
  duplication `ModeDesc.Format` at open. Formats outside BGRA8 and
  R10G10B10A2 fail immediately with `RD_DD_UNSUPPORTED_FORMAT`.
- Exactly one lease may be outstanding. `rd_dd_release` consumes the lease and
  returns `RD_DD_OK` only when `ReleaseFrame` returns `S_OK`, a documented
  terminal whole-duplication/device invalidation (`DXGI_ERROR_ACCESS_LOST`,
  `DXGI_ERROR_SESSION_DISCONNECTED`, `DXGI_ERROR_DEVICE_REMOVED`, or
  `DXGI_ERROR_DEVICE_RESET`), or `DXGI_ERROR_INVALID_CALL` indicating a safe
  no-frame state. Terminal/no-frame results latch access lost so subsequent
  acquire/capture-info calls request close and reopen.
- Any other public `ReleaseFrame` result is ambiguous: the exact lease and all
  local COM owners remain retained, `RD_DD_RECLAMATION_UNCERTAIN` is returned,
  and the caller retries `rd_dd_release` with that same lease pointer. There is
  no forced-free or abandon API.
- Internal discard paths retain the exact acquired owner on an ambiguous result.
  The next acquire and close each retry that internal `ReleaseFrame` once. `S_OK`
  clears quarantine; terminal invalidation or `DXGI_ERROR_INVALID_CALL` safely
  releases the owner and latches access lost; another unknown result keeps the
  quarantine. Close proceeds only after safe reclamation and otherwise returns
  `RD_DD_RECLAMATION_UNCERTAIN`.
- A non-identity display rotation is rejected because presenting an upright
  image would require a copy/rotation stage. Geometry changes are not adapted.
- Raw `ColorSpace`, `BitsPerColor`, `MinLuminance`, `MaxLuminance`, and
  `MaxFullFrameLuminance` are snapshotted from `IDXGIOutput6::GetDesc1` at open.
  A change is reported as `RD_DD_COLOR_CHANGED` before another frame is
  acquired. The luminance values describe display capability only; they are not
  HDR10 content mastering metadata, MaxCLL, or MaxFALL and must not be treated
  as such.
- There are no staging textures, `CopyResource`, `Map`, shaders, video
  processors, scaling, cursor composition, CPU conversion, frame repetition, or
  silent frame discard. A frame rejected for an explicitly reported format or
  geometry error is released once solely to satisfy the DXGI acquisition
  contract.
- Emitted `qpc_timestamp` is always DXGI's actual `LastPresentTime`; no synthetic
  presentation timestamp is generated. Acquisitions with `AccumulatedFrames ==
  0` or a zero `LastPresentTime` are metadata-only, are released exactly once,
  and return `RD_DD_WOULD_BLOCK` rather than repeating pixels. Emitted frames
  expose `AccumulatedFrames` so source-side coalescing remains observable.

No macOS build or runtime claim is made. These sources require Windows SDK
D3D11/DXGI headers and libraries and intentionally fail preprocessing elsewhere.
