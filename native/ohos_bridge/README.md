# HarmonyOS Flutter bridge

This crate is the frontend-owned `cdylib` adapter for Flutter on HarmonyOS. It
does not contain a second RustDesk protocol or session core:

- `R_RustDesk-Core` supplies the RustDesk implementation with the
  `ohos-flutter` feature and its default `use_dasp` audio pipeline.
- this crate re-exports Core's `flutter_ffi` API to the generated Flutter Rust
  Bridge module;
- `scripts/build-ohos-ffi.sh` regenerates the FRB bindings from the sibling
  Core checkout and links this adapter.

Keep the sibling `R_RustDesk-Core` checkout pinned to the Core revision used by
the Flutter release. Flutter-specific code generation and packaging stay in
this repository.
