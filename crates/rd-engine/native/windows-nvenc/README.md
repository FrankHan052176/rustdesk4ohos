# Windows direct NVENC shim

A Windows-only C++17 C ABI around NVENC 12.1 for an existing `ID3D11Device` and existing same-device `ID3D11Texture2D`.

The frame path is exactly register, map, encode, blocking bitstream lock, loan release, unlock, unmap, unregister. It does not call `CopyResource`, D3D `Map`, `UpdateSubresource`, create staging textures, scale, or provide a software fallback. The output loan points directly into NVIDIA's locked bitstream memory; there is no intermediate encoded-byte copy and no async buffering.

A successful encode deliberately keeps the NVIDIA bitstream locked and the input texture mapped and registered. The Rust caller must retain its owned capture lease for the exact submitted `ID3D11Texture2D` and must not mutate, release, recycle, or reuse the texture until `rd_nvenc_release_output` succeeds. The shim never AddRefs or Releases a submitted texture.

`rd_nvenc_release_output` validates the complete loan identity and performs cleanup in strict retryable stages: unlock, then unmap, then unregister. State is cleared only after each stage succeeds. `RD_NVENC_CLEANUP_FAILED` retains the loan identity and unfinished stages; the caller must retry with the same loan. Already successful stages are not repeated. Once the unlock stage succeeds, the bitstream pointer is no longer readable even if unmap or unregister subsequently fails.

`rd_nvenc_shutdown` refuses an outstanding or partially released loan. After release succeeds, shutdown remains retryable and retains the opaque handle if synchronous EOS flush, bitstream-buffer destruction, or session destruction fails. The supplied D3D11 device is AddRef'd until successful shutdown.

HDR static metadata is emitted as standardized mastering-display (SEI payload type 137) and content-light-level (type 144) payloads on the first picture and requested IDRs. Color VUI fields are applied to H.264/HEVC configuration. Field values use the bitstream standards' integer units and enumerations; validation of their semantic ranges remains the caller's responsibility.
