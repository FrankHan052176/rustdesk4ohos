#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
core_root="$(cd "$repo_root/../R_RustDesk-Core" && pwd)"
bridge_manifest="$repo_root/native/ohos_bridge/Cargo.toml"
cd "$repo_root"

source "$repo_root/scripts/ohos-env.sh"

if ! command -v flutter >/dev/null 2>&1 && [[ -x "$HOME/flutter-ohos/bin/flutter" ]]; then
  export PATH="$HOME/flutter-ohos/bin:$PATH"
fi
if ! command -v flutter >/dev/null 2>&1; then
  echo "Flutter-OH is not available on PATH" >&2
  exit 2
fi

frb_codegen="${FLUTTER_RUST_BRIDGE_CODEGEN:-$HOME/.cargo/bin/flutter_rust_bridge_codegen}"
if [[ ! -x "$frb_codegen" ]]; then
  echo "Flutter Rust bridge generator is unavailable: $frb_codegen" >&2
  exit 2
fi

"$frb_codegen" \
  --skip-deps-check \
  --llvm-path "$OHOS_NDK_HOME/native/llvm" \
  --rust-input "$core_root/src/flutter_ffi.rs" \
  --rust-crate-dir "$core_root" \
  --rust-output "$repo_root/src/bridge_generated.rs" \
  --dart-output "$repo_root/flutter/lib/generated_bridge.dart" \
  --skip-add-mod-to-lib

python3 "$repo_root/scripts/strip-ohos-frb-core-event-impl.py" \
  "$repo_root/src/bridge_generated.rs"

for dependency_script in \
  build-libvpx-ohos.sh \
  build-libaom-ohos.sh \
  build-libyuv-ohos.sh \
  build-libopus-ohos.sh; do
  bash "$repo_root/scripts/$dependency_script"
done

cargo build \
  --manifest-path "$bridge_manifest" \
  --target aarch64-unknown-linux-ohos \
  --release \
  --locked \
  --lib \
  "$@"

artifact="$repo_root/native/ohos_bridge/target/aarch64-unknown-linux-ohos/release/librustdesk_ohos_flutter_bridge.so"
if [[ ! -s "$artifact" ]]; then
  echo "OHOS FFI build did not produce $artifact" >&2
  exit 1
fi

flutter_lib_dir="$repo_root/flutter/ohos/entry/libs/arm64-v8a"
mkdir -p "$flutter_lib_dir"
cp "$artifact" "$flutter_lib_dir/liblibrustdesk.so"
libcxx="$OHOS_NDK_HOME/native/llvm/lib/aarch64-linux-ohos/libc++_shared.so"
if [[ ! -s "$libcxx" ]]; then
  echo "Missing OHOS C++ runtime: $libcxx" >&2
  exit 1
fi
cp "$libcxx" "$flutter_lib_dir/libc++_shared.so"

printf 'Done: %s\n' "$artifact"
printf 'Flutter OHOS library: %s\n' "$flutter_lib_dir/liblibrustdesk.so"
