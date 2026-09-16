#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
flutter_root="$repo_root/flutter"
profile="$flutter_root/ohos/build-profile.json5"
build_mode="${OHOS_BUILD_MODE:-debug}"
package_kind="${OHOS_PACKAGE_KIND:-hap}"
product="${OHOS_PRODUCT:-default}"

if [[ "$package_kind" != "hap" && "$package_kind" != "app" ]]; then
  echo "Unsupported OHOS_PACKAGE_KIND: $package_kind" >&2
  exit 2
fi
if [[ "$product" != "default" && "$product" != "publish" ]]; then
  echo "Unsupported OHOS_PRODUCT: $product" >&2
  exit 2
fi
if [[ "$package_kind" == "app" && ( "$build_mode" != "release" || "$product" != "publish" || -z "${RUSTDESK_SIGNING_DIR:-}" ) ]]; then
  echo "APP packaging requires release mode, publish product, and RUSTDESK_SIGNING_DIR" >&2
  exit 2
fi

if [[ "$build_mode" != "debug" && "$build_mode" != "profile" && "$build_mode" != "release" ]]; then
  echo "Unsupported OHOS_BUILD_MODE: $build_mode" >&2
  exit 2
fi
if ! command -v flutter >/dev/null 2>&1; then
  echo "Flutter-OH is not available on PATH" >&2
  exit 2
fi
if ! command -v hvigorw >/dev/null 2>&1; then
  echo "hvigorw is not available on PATH" >&2
  exit 2
fi

backup=""
dependency_backup=""
dependency_backup_ready=false
cleanup() {
  if [[ -n "$backup" && -f "$backup" ]]; then
    cp "$backup" "$profile"
    rm -f "$backup"
  fi
  if [[ -n "$dependency_backup" && -d "$dependency_backup" ]]; then
    if [[ "$dependency_backup_ready" != true ]]; then
      rm -f "$dependency_backup/pubspec.yaml" "$dependency_backup/pubspec.lock"
      rmdir "$dependency_backup"
      return
    fi
    cp "$dependency_backup/pubspec.yaml" "$flutter_root/pubspec.yaml"
    if [[ -f "$dependency_backup/pubspec.lock" ]]; then
      cp "$dependency_backup/pubspec.lock" "$flutter_root/pubspec.lock"
    else
      rm -f "$flutter_root/pubspec.lock"
    fi
    rm -f "$dependency_backup/pubspec.yaml" "$dependency_backup/pubspec.lock"
    rmdir "$dependency_backup"
  fi
}
trap cleanup EXIT

if [[ -n "${RUSTDESK_SIGNING_DIR:-}" ]]; then
  signing_json="$(cd "$RUSTDESK_SIGNING_DIR" && pwd)/signingConfigs.json"
  if [[ ! -r "$signing_json" ]]; then
    echo "RUSTDESK_SIGNING_DIR must contain signingConfigs.json" >&2
    exit 2
  fi
  backup="$(mktemp /tmp/rustdesk-flutter-build-profile.XXXXXX)"
  cp "$profile" "$backup"
  PROFILE="$profile" SIGNING_JSON="$signing_json" PRODUCT="$product" python3 - <<'PY'
import json
import os
from pathlib import Path

profile_path = Path(os.environ["PROFILE"])
signing_path = Path(os.environ["SIGNING_JSON"])
profile = json.loads(profile_path.read_text(encoding="utf-8"))
configs = json.loads(signing_path.read_text(encoding="utf-8"))
if not isinstance(configs, list):
    raise SystemExit("signingConfigs.json must contain an array")
product_name = os.environ["PRODUCT"]
config = next((item for item in configs if item.get("name") == product_name), None)
if config is None:
    raise SystemExit(f"signingConfigs.json has no {product_name} configuration")
material = config.get("material", {})
for field in ("storeFile", "profile", "certpath"):
    value = material.get(field)
    if not isinstance(value, str) or not Path(value).is_file():
        raise SystemExit(f"{product_name} signing material {field} is missing")
profile["app"]["signingConfigs"] = [config]
for product in profile["app"].get("products", []):
    if product.get("name") == product_name:
        product["signingConfig"] = product_name
profile_path.write_text(json.dumps(profile, indent=2) + "\n", encoding="utf-8")
PY
fi

dependency_backup="$(mktemp -d /tmp/rustdesk-flutter-dependencies.XXXXXX)"
cp "$flutter_root/pubspec.yaml" "$dependency_backup/pubspec.yaml"
if [[ -f "$flutter_root/pubspec.lock" ]]; then
  cp "$flutter_root/pubspec.lock" "$dependency_backup/pubspec.lock"
fi
dependency_backup_ready=true
python3 "$repo_root/scripts/prepare-ohos-flutter.py"

# AppGallery rejects an App Pack whose versionName+versionCode pair is already in use, so a build
# that carries a CI run identity derives a fresh, monotonic versionCode for it:
#
#   versionCode = 100000000 + UTC-days-since-2020-01-01 * 100000 + run-number * 100 + run-attempt
#
# The Flutter tool is the only layer that reaches the packaged App: it stamps this number into
# local.properties and hvigor's flutter plugin writes it into the App manifest, overwriting both
# AppScope/app.json5 and anything hvigorfile hooks set themselves. Local builds keep the committed
# baseline because GITHUB_RUN_NUMBER is absent.
build_number_arg=()
if [[ -n "${GITHUB_RUN_NUMBER:-}" ]]; then
  derived_version_code="$(python3 - <<'PY'
import datetime
import os

day = (datetime.datetime.now(datetime.timezone.utc)
       - datetime.datetime(2020, 1, 1, tzinfo=datetime.timezone.utc)).days
run_number = int(os.environ['GITHUB_RUN_NUMBER'])
run_attempt = int(os.environ.get('GITHUB_RUN_ATTEMPT', '1'))
if not 1 <= run_number <= 999:
    raise SystemExit(f'GitHub run number {run_number} overflows the per-day sequence')
if not 1 <= run_attempt <= 99:
    raise SystemExit(f'GitHub run attempt must be 1..99, got {run_attempt}')
print(100_000_000 + day * 100_000 + run_number * 100 + run_attempt)
PY
)"
  echo "OHOS App versionCode=$derived_version_code"
  build_number_arg=(--build-number "$derived_version_code")
fi

cd "$flutter_root"
env -u RUSTDESK_SIGNING_DIR flutter build "$package_kind" "--$build_mode" --flavor "$product" \
  ${build_number_arg[@]+"${build_number_arg[@]}"} "$@"
