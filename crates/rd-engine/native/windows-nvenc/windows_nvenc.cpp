#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#define RD_WINDOWS_NVENC_BUILD
#include "windows_nvenc.h"
#ifdef _WIN32
#include <windows.h>
#include <d3d11.h>
#include <dxgi.h>
#include <new>
#include <vector>
#include <cstring>
#include "include/nvEncodeAPI.h"

static thread_local char g_error[256] = {};
static rd_nvenc_status fail(rd_nvenc_status s, const char *msg) noexcept {
    strncpy_s(g_error, msg ? msg : "", _TRUNCATE);
    return s;
}
static rd_nvenc_status nvfail(const char *op, NVENCSTATUS n) noexcept {
    _snprintf_s(g_error, _TRUNCATE, "%s failed (NVENC status %d)", op, (int)n);
    return RD_NVENC_NVENC_ERROR;
}
static bool guid_eq(const GUID &a, const GUID &b) noexcept {
    return InlineIsEqualGUID(a, b) != 0;
}

struct rd_nvenc_encoder {
    HMODULE dll = nullptr;
    ID3D11Device *device = nullptr;
    NV_ENCODE_API_FUNCTION_LIST api{};
    void *session = nullptr;
    NV_ENC_OUTPUT_PTR bitstream = nullptr;
    GUID codec{};
    NV_ENC_BUFFER_FORMAT format = NV_ENC_BUFFER_FORMAT_UNDEFINED;
    DXGI_FORMAT dxgi_format = DXGI_FORMAT_UNKNOWN;
    uint32_t width = 0, height = 0, frame_index = 0;
    bool hdr = false, loaned = false, bitstream_locked = false;
    bool initialized = false, flushed = false;
    NV_ENC_INPUT_PTR pending_mapped = nullptr;
    NV_ENC_REGISTERED_PTR pending_registered = nullptr;
    const uint8_t *loan_data = nullptr;
    uint64_t loan_size = 0;
    uint64_t loan_timestamp = 0;
    uint32_t loan_picture_type = 0;
    std::vector<uint8_t> mastering;
    std::vector<uint8_t> light;
};

static HMODULE load_nvenc() noexcept {
    HMODULE k32 = GetModuleHandleW(L"kernel32.dll");
    using LoadEx = HMODULE(WINAPI *)(LPCWSTR, HANDLE, DWORD);
    auto loadex = reinterpret_cast<LoadEx>(GetProcAddress(k32, "LoadLibraryExW"));
    if (loadex) {
        HMODULE m =
            loadex(L"nvEncodeAPI64.dll", nullptr, 0x00000800 /* LOAD_LIBRARY_SEARCH_SYSTEM32 */);
        if (m) {
            return m;
        }
    }
    wchar_t dir[MAX_PATH + 1]{};
    UINT n = GetSystemDirectoryW(dir, MAX_PATH);
    if (!n || n >= MAX_PATH - 20) {
        return nullptr;
    }
    if (dir[n - 1] != L'\\') {
        dir[n++] = L'\\';
    }
    const wchar_t name[] = L"nvEncodeAPI64.dll";
    memcpy(dir + n, name, sizeof(name));
    return LoadLibraryExW(dir, nullptr, LOAD_WITH_ALTERED_SEARCH_PATH);
}

static bool map_format(uint32_t f, NV_ENC_BUFFER_FORMAT &nv, DXGI_FORMAT &dx) noexcept {
    switch (f) {
    case RD_NVENC_INPUT_NV12:
        nv = NV_ENC_BUFFER_FORMAT_NV12;
        dx = DXGI_FORMAT_NV12;
        return true;
    case RD_NVENC_INPUT_P010:
        nv = NV_ENC_BUFFER_FORMAT_YUV420_10BIT;
        dx = DXGI_FORMAT_P010;
        return true;
    case RD_NVENC_INPUT_BGRA8:
        nv = NV_ENC_BUFFER_FORMAT_ARGB;
        dx = DXGI_FORMAT_B8G8R8A8_UNORM;
        return true;
    case RD_NVENC_INPUT_RGBA8:
        nv = NV_ENC_BUFFER_FORMAT_ABGR;
        dx = DXGI_FORMAT_R8G8B8A8_UNORM;
        return true;
    case RD_NVENC_INPUT_RGB10A2:
        nv = NV_ENC_BUFFER_FORMAT_ABGR10;
        dx = DXGI_FORMAT_R10G10B10A2_UNORM;
        return true;
    default:
        return false;
    }
}

static int cap(rd_nvenc_encoder *e, NV_ENC_CAPS c, int *out) noexcept {
    NV_ENC_CAPS_PARAM p{};
    p.version = NV_ENC_CAPS_PARAM_VER;
    p.capsToQuery = c;
    return e->api.nvEncGetEncodeCaps(e->session, e->codec, &p, out) == NV_ENC_SUCCESS;
}
static void be16(std::vector<uint8_t> &v, uint16_t x) {
    v.push_back(uint8_t(x >> 8));
    v.push_back(uint8_t(x));
}
static void be32(std::vector<uint8_t> &v, uint32_t x) {
    v.push_back(uint8_t(x >> 24));
    v.push_back(uint8_t(x >> 16));
    v.push_back(uint8_t(x >> 8));
    v.push_back(uint8_t(x));
}
static void make_hdr(rd_nvenc_encoder *e, const rd_nvenc_hdr_static_metadata &h) {
    e->mastering.clear();
    e->mastering.reserve(24);
    for (int i = 0; i < 3; i++) {
        be16(e->mastering, h.display_primaries_x_gbr[i]);
        be16(e->mastering, h.display_primaries_y_gbr[i]);
    }
    be16(e->mastering, h.white_point_x);
    be16(e->mastering, h.white_point_y);
    be32(e->mastering, h.max_display_mastering_luminance);
    be32(e->mastering, h.min_display_mastering_luminance);
    e->light.clear();
    e->light.reserve(4);
    be16(e->light, h.max_content_light_level);
    be16(e->light, h.max_frame_average_light_level);
}
static void set_vui(NV_ENC_CONFIG_H264_VUI_PARAMETERS &v, const rd_nvenc_color_config &c) noexcept {
    if (!c.enabled) {
        return;
    }
    v.videoSignalTypePresentFlag = 1;
    v.videoFormat = (NV_ENC_VUI_VIDEO_FORMAT)c.video_format;
    v.videoFullRangeFlag = c.video_full_range ? 1u : 0u;
    v.colourDescriptionPresentFlag = 1;
    v.colourPrimaries = (NV_ENC_VUI_COLOR_PRIMARIES)c.color_primaries;
    v.transferCharacteristics = (NV_ENC_VUI_TRANSFER_CHARACTERISTIC)c.transfer_characteristics;
    v.colourMatrix = (NV_ENC_VUI_MATRIX_COEFFS)c.matrix_coefficients;
}

static rd_nvenc_status cleanup_partial(rd_nvenc_encoder *e) noexcept {
    if (e->pending_mapped && e->session) {
        if (e->api.nvEncUnmapInputResource(e->session, e->pending_mapped) != NV_ENC_SUCCESS) {
            return fail(RD_NVENC_CLEANUP_FAILED, "pending input unmap failed; handle retained");
        }
        e->pending_mapped = nullptr;
    }
    if (e->pending_registered && e->session) {
        if (e->api.nvEncUnregisterResource(e->session, e->pending_registered) != NV_ENC_SUCCESS) {
            return fail(RD_NVENC_CLEANUP_FAILED,
                        "pending input unregister failed; handle retained");
        }
        e->pending_registered = nullptr;
    }
    if (e->initialized && !e->flushed && e->session) {
        NV_ENC_PIC_PARAMS eos{};
        eos.version = NV_ENC_PIC_PARAMS_VER;
        eos.encodePicFlags = NV_ENC_PIC_FLAG_EOS;
        NVENCSTATUS n = e->api.nvEncEncodePicture(e->session, &eos);
        if (n != NV_ENC_SUCCESS) {
            return fail(RD_NVENC_CLEANUP_FAILED, "synchronous NVENC flush failed; handle retained");
        }
        e->flushed = true;
    }
    if (e->bitstream && e->session) {
        if (e->api.nvEncDestroyBitstreamBuffer(e->session, e->bitstream) != NV_ENC_SUCCESS) {
            return fail(RD_NVENC_CLEANUP_FAILED, "bitstream destruction failed; handle retained");
        }
        e->bitstream = nullptr;
    }
    if (e->session) {
        if (e->api.nvEncDestroyEncoder(e->session) != NV_ENC_SUCCESS) {
            return fail(RD_NVENC_CLEANUP_FAILED, "session destruction failed; handle retained");
        }
        e->session = nullptr;
    }
    if (e->device) {
        e->device->Release();
        e->device = nullptr;
    }
    if (e->dll) {
        FreeLibrary(e->dll);
        e->dll = nullptr;
    }
    return RD_NVENC_OK;
}

static rd_nvenc_status failed_create_cleanup(rd_nvenc_encoder *e,
                                             rd_nvenc_encoder **out,
                                             rd_nvenc_status original_status) noexcept {
    rd_nvenc_status cleanup_status = cleanup_partial(e);
    if (cleanup_status != RD_NVENC_OK) {
        *out = e;
        return RD_NVENC_CLEANUP_FAILED;
    }
    delete e;
    return original_status;
}

extern "C" RD_NVENC_API rd_nvenc_status RD_NVENC_CALL rd_nvenc_create(void *devp,
                                                                      const rd_nvenc_create_desc *d,
                                                                      rd_nvenc_encoder **out) {
    g_error[0] = 0;
    if (out) {
        *out = nullptr;
    }
    rd_nvenc_encoder *e = nullptr;
    try {
        if (!devp || !d || !out || d->struct_size != sizeof(*d) || !d->width || !d->height ||
            !d->fps_num || !d->fps_den || !d->bitrate_bps || !d->gop_length) {
            return fail(RD_NVENC_INVALID_ARGUMENT, "invalid create arguments");
        }
        const bool ten_bit =
            d->input_format == RD_NVENC_INPUT_P010 || d->input_format == RD_NVENC_INPUT_RGB10A2;
        if (d->hdr_static.enabled && (d->codec != RD_NVENC_CODEC_H265 || !ten_bit)) {
            return fail(RD_NVENC_INVALID_ARGUMENT,
                        "HDR static metadata requires H.265 with a 10-bit input format");
        }
        e = new (std::nothrow) rd_nvenc_encoder;
        if (!e) {
            return fail(RD_NVENC_OUT_OF_MEMORY, "allocation failed");
        }
        e->width = d->width;
        e->height = d->height;
        if (!map_format(d->input_format, e->format, e->dxgi_format)) {
            delete e;
            return fail(RD_NVENC_UNSUPPORTED, "unsupported input format enum");
        }
        e->codec = d->codec == RD_NVENC_CODEC_H264   ? NV_ENC_CODEC_H264_GUID
                   : d->codec == RD_NVENC_CODEC_H265 ? NV_ENC_CODEC_HEVC_GUID
                                                     : GUID{};
        if (d->codec != RD_NVENC_CODEC_H264 && d->codec != RD_NVENC_CODEC_H265) {
            delete e;
            return fail(RD_NVENC_UNSUPPORTED, "unsupported codec enum");
        }
        if (d->codec == RD_NVENC_CODEC_H264 &&
            (d->input_format == RD_NVENC_INPUT_P010 || d->input_format == RD_NVENC_INPUT_RGB10A2)) {
            delete e;
            return fail(RD_NVENC_UNSUPPORTED, "10-bit H.264 input is unsupported");
        }
        e->dll = load_nvenc();
        if (!e->dll) {
            delete e;
            return fail(RD_NVENC_DLL_NOT_FOUND, "nvEncodeAPI64.dll not found in System32");
        }
        using GetMaxFn = NVENCSTATUS(NVENCAPI *)(uint32_t *);
        using CreateFn = NVENCSTATUS(NVENCAPI *)(NV_ENCODE_API_FUNCTION_LIST *);
        auto maxver =
            reinterpret_cast<GetMaxFn>(GetProcAddress(e->dll, "NvEncodeAPIGetMaxSupportedVersion"));
        auto create =
            reinterpret_cast<CreateFn>(GetProcAddress(e->dll, "NvEncodeAPICreateInstance"));
        if (!maxver || !create) {
            FreeLibrary(e->dll);
            delete e;
            return fail(RD_NVENC_DLL_NOT_FOUND, "required NVENC exports missing");
        }
        uint32_t driver = 0;
        NVENCSTATUS ns = maxver(&driver);
        if (ns != NV_ENC_SUCCESS) {
            FreeLibrary(e->dll);
            delete e;
            return nvfail("NvEncodeAPIGetMaxSupportedVersion", ns);
        }
        const uint32_t need = (NVENCAPI_MAJOR_VERSION << 4) | NVENCAPI_MINOR_VERSION;
        if (driver < need) {
            FreeLibrary(e->dll);
            delete e;
            return fail(RD_NVENC_DRIVER_TOO_OLD, "NVIDIA driver NVENC API is older than 12.1");
        }
        e->api.version = NV_ENCODE_API_FUNCTION_LIST_VER;
        ns = create(&e->api);
        if (ns != NV_ENC_SUCCESS) {
            FreeLibrary(e->dll);
            delete e;
            return nvfail("NvEncodeAPICreateInstance", ns);
        }
        e->device = (ID3D11Device *)devp;
        e->device->AddRef();
        NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS op{};
        op.version = NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
        op.deviceType = NV_ENC_DEVICE_TYPE_DIRECTX;
        op.device = e->device;
        op.apiVersion = NVENCAPI_VERSION;
        ns = e->api.nvEncOpenEncodeSessionEx(&op, &e->session);
        if (ns != NV_ENC_SUCCESS) {
            rd_nvenc_status original = nvfail("nvEncOpenEncodeSessionEx", ns);
            return failed_create_cleanup(e, out, original);
        }
        uint32_t ng = 0;
        ns = e->api.nvEncGetEncodeGUIDCount(e->session, &ng);
        std::vector<GUID> gs(ng);
        if (ns == NV_ENC_SUCCESS) {
            ns = e->api.nvEncGetEncodeGUIDs(e->session, gs.data(), ng, &ng);
        }
        bool found = false;
        for (uint32_t i = 0; i < ng; i++) {
            if (guid_eq(gs[i], e->codec)) {
                found = true;
            }
        }
        if (ns != NV_ENC_SUCCESS || !found) {
            rd_nvenc_status original =
                fail(RD_NVENC_UNSUPPORTED, "requested codec GUID is not supported");
            return failed_create_cleanup(e, out, original);
        }
        uint32_t nf = 0;
        ns = e->api.nvEncGetInputFormatCount(e->session, e->codec, &nf);
        std::vector<NV_ENC_BUFFER_FORMAT> fs(nf);
        if (ns == NV_ENC_SUCCESS) {
            ns = e->api.nvEncGetInputFormats(e->session, e->codec, fs.data(), nf, &nf);
        }
        found = false;
        for (uint32_t i = 0; i < nf; i++) {
            if (fs[i] == e->format) {
                found = true;
            }
        }
        if (ns != NV_ENC_SUCCESS || !found) {
            rd_nvenc_status original =
                fail(RD_NVENC_UNSUPPORTED, "requested NVENC input format is not supported");
            return failed_create_cleanup(e, out, original);
        }
        int mx = 0, my = 0, ten = 0;
        if (!cap(e, NV_ENC_CAPS_WIDTH_MAX, &mx) || !cap(e, NV_ENC_CAPS_HEIGHT_MAX, &my) ||
            d->width > (uint32_t)mx || d->height > (uint32_t)my) {
            rd_nvenc_status original =
                fail(RD_NVENC_UNSUPPORTED, "requested dimensions exceed NVENC caps");
            return failed_create_cleanup(e, out, original);
        }
        if ((d->input_format == RD_NVENC_INPUT_P010 || d->input_format == RD_NVENC_INPUT_RGB10A2) &&
            (!cap(e, NV_ENC_CAPS_SUPPORT_10BIT_ENCODE, &ten) || !ten)) {
            rd_nvenc_status original = fail(RD_NVENC_UNSUPPORTED, "10-bit encode is not supported");
            return failed_create_cleanup(e, out, original);
        }
        NV_ENC_PRESET_CONFIG pc{};
        pc.version = NV_ENC_PRESET_CONFIG_VER;
        pc.presetCfg.version = NV_ENC_CONFIG_VER;
        ns = e->api.nvEncGetEncodePresetConfigEx(
            e->session, e->codec, NV_ENC_PRESET_P1_GUID, NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY, &pc);
        if (ns != NV_ENC_SUCCESS) {
            rd_nvenc_status original = nvfail("nvEncGetEncodePresetConfigEx", ns);
            return failed_create_cleanup(e, out, original);
        }
        NV_ENC_CONFIG cfg = pc.presetCfg;
        cfg.version = NV_ENC_CONFIG_VER;
        cfg.gopLength = d->gop_length;
        cfg.frameIntervalP = 1;
        cfg.rcParams.rateControlMode = NV_ENC_PARAMS_RC_CBR;
        cfg.rcParams.enableLookahead = 0;
        cfg.rcParams.lookaheadDepth = 0;
        cfg.rcParams.enableTemporalAQ = 0;
        cfg.rcParams.multiPass = NV_ENC_MULTI_PASS_DISABLED;
        cfg.rcParams.zeroReorderDelay = 1;
        cfg.rcParams.averageBitRate = d->bitrate_bps;
        cfg.rcParams.maxBitRate = d->bitrate_bps;
        cfg.rcParams.vbvBufferSize = (uint32_t)((uint64_t)d->bitrate_bps * d->fps_den / d->fps_num);
        if (!cfg.rcParams.vbvBufferSize) {
            cfg.rcParams.vbvBufferSize = 1;
        }
        cfg.rcParams.vbvInitialDelay = cfg.rcParams.vbvBufferSize;
        if (d->codec == RD_NVENC_CODEC_H264) {
            cfg.profileGUID = NV_ENC_H264_PROFILE_HIGH_GUID;
            auto &h = cfg.encodeCodecConfig.h264Config;
            h.idrPeriod = d->gop_length;
            h.repeatSPSPPS = 1;
            h.hierarchicalPFrames = 0;
            h.hierarchicalBFrames = 0;
            h.enableLTR = 0;
            h.maxNumRefFrames = 1;
            h.useBFramesAsRef = NV_ENC_BFRAME_REF_MODE_DISABLED;
            h.numRefL0 = NV_ENC_NUM_REF_FRAMES_1;
            h.numRefL1 = NV_ENC_NUM_REF_FRAMES_1;
            set_vui(h.h264VUIParameters, d->color);
        } else {
            cfg.profileGUID =
                ten_bit ? NV_ENC_HEVC_PROFILE_MAIN10_GUID : NV_ENC_HEVC_PROFILE_MAIN_GUID;
            auto &h = cfg.encodeCodecConfig.hevcConfig;
            h.idrPeriod = d->gop_length;
            h.repeatSPSPPS = 1;
            h.chromaFormatIDC = 1;
            h.enableLTR = 0;
            h.maxNumRefFramesInDPB = 1;
            h.maxTemporalLayersMinus1 = 0;
            h.useBFramesAsRef = NV_ENC_BFRAME_REF_MODE_DISABLED;
            h.numRefL0 = NV_ENC_NUM_REF_FRAMES_1;
            h.numRefL1 = NV_ENC_NUM_REF_FRAMES_1;
            h.pixelBitDepthMinus8 = (d->input_format == RD_NVENC_INPUT_P010 ||
                                     d->input_format == RD_NVENC_INPUT_RGB10A2)
                                        ? 2
                                        : 0;
            set_vui(h.hevcVUIParameters, d->color);
        }
        NV_ENC_INITIALIZE_PARAMS ip{};
        ip.version = NV_ENC_INITIALIZE_PARAMS_VER;
        ip.encodeGUID = e->codec;
        ip.presetGUID = NV_ENC_PRESET_P1_GUID;
        ip.encodeWidth = d->width;
        ip.encodeHeight = d->height;
        ip.darWidth = d->width;
        ip.darHeight = d->height;
        ip.frameRateNum = d->fps_num;
        ip.frameRateDen = d->fps_den;
        ip.enableEncodeAsync = 0;
        ip.enablePTD = 1;
        ip.maxEncodeWidth = d->width;
        ip.maxEncodeHeight = d->height;
        ip.tuningInfo = NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY;
        ip.encodeConfig = &cfg;
        ns = e->api.nvEncInitializeEncoder(e->session, &ip);
        if (ns == NV_ENC_SUCCESS) {
            e->initialized = true;
        }
        if (ns != NV_ENC_SUCCESS) {
            rd_nvenc_status original = nvfail("nvEncInitializeEncoder", ns);
            return failed_create_cleanup(e, out, original);
        }
        NV_ENC_CREATE_BITSTREAM_BUFFER bb{};
        bb.version = NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
        ns = e->api.nvEncCreateBitstreamBuffer(e->session, &bb);
        if (ns != NV_ENC_SUCCESS) {
            rd_nvenc_status original = nvfail("nvEncCreateBitstreamBuffer", ns);
            return failed_create_cleanup(e, out, original);
        }
        e->bitstream = bb.bitstreamBuffer;
        if (d->hdr_static.enabled) {
            make_hdr(e, d->hdr_static);
            e->hdr = true;
        }
        *out = e;
        return RD_NVENC_OK;
    } catch (const std::bad_alloc &) {
        rd_nvenc_status original = fail(RD_NVENC_OUT_OF_MEMORY, "allocation failed");
        return e ? failed_create_cleanup(e, out, original) : original;
    } catch (...) {
        rd_nvenc_status original = fail(RD_NVENC_INTERNAL_ERROR, "unexpected C++ exception");
        return e ? failed_create_cleanup(e, out, original) : original;
    }
}

extern "C" RD_NVENC_API rd_nvenc_status RD_NVENC_CALL rd_nvenc_encode_texture(
    rd_nvenc_encoder *e, void *texp, uint64_t ts, uint32_t force, rd_nvenc_output_loan *out) {
    g_error[0] = 0;
    if (!e || !texp || !out || out->struct_size != sizeof(*out) || out->reserved != 0 ||
        force > 1) {
        return fail(RD_NVENC_INVALID_ARGUMENT,
                    "invalid encode arguments, force flag, or output reserved field");
    }
    if (e->loaned) {
        return fail(RD_NVENC_BUSY, "release the outstanding output loan first");
    }
    if (e->pending_mapped || e->pending_registered) {
        return fail(RD_NVENC_CLEANUP_FAILED, "prior input cleanup is incomplete");
    }
    ID3D11Texture2D *tex = (ID3D11Texture2D *)texp;
    D3D11_TEXTURE2D_DESC td{};
    tex->GetDesc(&td);
    ID3D11Device *owner = nullptr;
    tex->GetDevice(&owner);
    bool same = owner == e->device;
    if (owner) {
        owner->Release();
    }
    if (!same || td.Width != e->width || td.Height != e->height || td.Format != e->dxgi_format ||
        td.SampleDesc.Count != 1) {
        return fail(RD_NVENC_D3D11_ERROR, "texture device, size, format, or sample count mismatch");
    }
    NV_ENC_REGISTER_RESOURCE rr{};
    rr.version = NV_ENC_REGISTER_RESOURCE_VER;
    rr.resourceType = NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
    rr.width = e->width;
    rr.height = e->height;
    rr.pitch = 0;
    rr.subResourceIndex = 0;
    rr.resourceToRegister = tex;
    rr.bufferFormat = e->format;
    rr.bufferUsage = NV_ENC_INPUT_IMAGE;
    NVENCSTATUS ns = e->api.nvEncRegisterResource(e->session, &rr);
    if (ns != NV_ENC_SUCCESS) {
        return nvfail("nvEncRegisterResource", ns);
    }
    e->pending_registered = rr.registeredResource;
    NV_ENC_MAP_INPUT_RESOURCE mr{};
    mr.version = NV_ENC_MAP_INPUT_RESOURCE_VER;
    mr.registeredResource = rr.registeredResource;
    ns = e->api.nvEncMapInputResource(e->session, &mr);
    if (ns != NV_ENC_SUCCESS) {
        NVENCSTATUS u = e->api.nvEncUnregisterResource(e->session, rr.registeredResource);
        if (u == NV_ENC_SUCCESS) {
            e->pending_registered = nullptr;
        }
        return u == NV_ENC_SUCCESS
                   ? nvfail("nvEncMapInputResource", ns)
                   : fail(RD_NVENC_CLEANUP_FAILED, "map and unregister both failed");
    }
    e->pending_mapped = mr.mappedResource;
    NV_ENC_SEI_PAYLOAD sei[2]{};
    if (e->hdr && (force || e->frame_index == 0)) {
        sei[0].payloadType = 137;
        sei[0].payloadSize = (uint32_t)e->mastering.size();
        sei[0].payload = e->mastering.data();
        sei[1].payloadType = 144;
        sei[1].payloadSize = (uint32_t)e->light.size();
        sei[1].payload = e->light.data();
    }
    NV_ENC_PIC_PARAMS pp{};
    pp.version = NV_ENC_PIC_PARAMS_VER;
    pp.inputWidth = e->width;
    pp.inputHeight = e->height;
    pp.inputPitch = 0;
    pp.frameIdx = e->frame_index;
    pp.inputTimeStamp = ts;
    pp.inputBuffer = mr.mappedResource;
    pp.outputBitstream = e->bitstream;
    pp.bufferFmt = mr.mappedBufferFmt;
    pp.pictureStruct = NV_ENC_PIC_STRUCT_FRAME;
    if (force) {
        pp.encodePicFlags = NV_ENC_PIC_FLAG_FORCEIDR | NV_ENC_PIC_FLAG_OUTPUT_SPSPPS;
    } else if (e->frame_index == 0) {
        pp.encodePicFlags = NV_ENC_PIC_FLAG_OUTPUT_SPSPPS;
    }
    if (e->hdr && (force || e->frame_index == 0)) {
        if (guid_eq(e->codec, NV_ENC_CODEC_H264_GUID)) {
            pp.codecPicParams.h264PicParams.seiPayloadArrayCnt = 2;
            pp.codecPicParams.h264PicParams.seiPayloadArray = sei;
        } else {
            pp.codecPicParams.hevcPicParams.seiPayloadArrayCnt = 2;
            pp.codecPicParams.hevcPicParams.seiPayloadArray = sei;
        }
    }
    ns = e->api.nvEncEncodePicture(e->session, &pp);
    if (ns == NV_ENC_SUCCESS) {
        NV_ENC_LOCK_BITSTREAM lb{};
        lb.version = NV_ENC_LOCK_BITSTREAM_VER;
        lb.outputBitstream = e->bitstream;
        lb.doNotWait = 0;
        ns = e->api.nvEncLockBitstream(e->session, &lb);
        if (ns == NV_ENC_SUCCESS) {
            e->bitstream_locked = true;
            e->loan_data = static_cast<const uint8_t *>(lb.bitstreamBufferPtr);
            e->loan_size = lb.bitstreamSizeInBytes;
            e->loan_timestamp = lb.outputTimeStamp;
            e->loan_picture_type = static_cast<uint32_t>(lb.pictureType);
            e->loaned = true;

            out->data = e->loan_data;
            out->size = e->loan_size;
            out->timestamp = e->loan_timestamp;
            out->picture_type = e->loan_picture_type;
            out->reserved = 0;
            ++e->frame_index;
            return RD_NVENC_OK;
        }
    }
    NVENCSTATUS um = e->api.nvEncUnmapInputResource(e->session, mr.mappedResource);
    if (um != NV_ENC_SUCCESS) {
        return fail(RD_NVENC_CLEANUP_FAILED, "encode failed and input unmap also failed");
    }
    e->pending_mapped = nullptr;

    NVENCSTATUS ur = e->api.nvEncUnregisterResource(e->session, rr.registeredResource);
    if (ur != NV_ENC_SUCCESS) {
        return fail(RD_NVENC_CLEANUP_FAILED, "encode failed and input unregister also failed");
    }
    e->pending_registered = nullptr;
    return nvfail(ns == NV_ENC_SUCCESS ? "nvEncLockBitstream" : "nvEncEncodePicture", ns);
}

extern "C" RD_NVENC_API rd_nvenc_status RD_NVENC_CALL
rd_nvenc_release_output(rd_nvenc_encoder *e, const rd_nvenc_output_loan *l) {
    g_error[0] = 0;
    if (!e || !l || l->struct_size != sizeof(*l) || l->reserved != 0 || !e->loaned ||
        l->data != e->loan_data || l->size != e->loan_size || l->timestamp != e->loan_timestamp ||
        l->picture_type != e->loan_picture_type) {
        return fail(RD_NVENC_INVALID_ARGUMENT, "invalid or stale output loan");
    }

    if (e->bitstream_locked) {
        NVENCSTATUS ns = e->api.nvEncUnlockBitstream(e->session, e->bitstream);
        if (ns != NV_ENC_SUCCESS) {
            return fail(RD_NVENC_CLEANUP_FAILED, "bitstream unlock failed; loan retained");
        }
        e->bitstream_locked = false;
    }

    if (e->pending_mapped) {
        NVENCSTATUS ns = e->api.nvEncUnmapInputResource(e->session, e->pending_mapped);
        if (ns != NV_ENC_SUCCESS) {
            return fail(RD_NVENC_CLEANUP_FAILED, "input unmap failed; loan retained");
        }
        e->pending_mapped = nullptr;
    }

    if (e->pending_registered) {
        NVENCSTATUS ns = e->api.nvEncUnregisterResource(e->session, e->pending_registered);
        if (ns != NV_ENC_SUCCESS) {
            return fail(RD_NVENC_CLEANUP_FAILED, "input unregister failed; loan retained");
        }
        e->pending_registered = nullptr;
    }

    e->loaned = false;
    e->loan_data = nullptr;
    e->loan_size = 0;
    e->loan_timestamp = 0;
    e->loan_picture_type = 0;
    return RD_NVENC_OK;
}
extern "C" RD_NVENC_API rd_nvenc_status RD_NVENC_CALL rd_nvenc_shutdown(rd_nvenc_encoder *e) {
    g_error[0] = 0;
    if (!e) {
        return fail(RD_NVENC_INVALID_ARGUMENT, "null encoder");
    }
    if (e->loaned) {
        return fail(RD_NVENC_BUSY, "output loan is still outstanding");
    }
    rd_nvenc_status s = cleanup_partial(e);
    if (s != RD_NVENC_OK) {
        return s;
    }
    delete e;
    return RD_NVENC_OK;
}
extern "C" RD_NVENC_API const char *RD_NVENC_CALL rd_nvenc_last_error(void) {
    return g_error;
}
#else
extern "C" RD_NVENC_API rd_nvenc_status RD_NVENC_CALL rd_nvenc_create(void *,
                                                                      const rd_nvenc_create_desc *,
                                                                      rd_nvenc_encoder **) {
    return RD_NVENC_UNSUPPORTED;
}
extern "C" RD_NVENC_API rd_nvenc_status RD_NVENC_CALL
rd_nvenc_encode_texture(rd_nvenc_encoder *, void *, uint64_t, uint32_t, rd_nvenc_output_loan *) {
    return RD_NVENC_UNSUPPORTED;
}
extern "C" RD_NVENC_API rd_nvenc_status RD_NVENC_CALL
rd_nvenc_release_output(rd_nvenc_encoder *, const rd_nvenc_output_loan *) {
    return RD_NVENC_UNSUPPORTED;
}
extern "C" RD_NVENC_API rd_nvenc_status RD_NVENC_CALL rd_nvenc_shutdown(rd_nvenc_encoder *) {
    return RD_NVENC_UNSUPPORTED;
}
extern "C" RD_NVENC_API const char *RD_NVENC_CALL rd_nvenc_last_error(void) {
    return "Windows only";
}
#endif
