# RustDesk Guide

## Project Layout

### Directory Structure
* `src/` Rust app
* `src/server/` audio / clipboard / input / video / network
* `src/platform/` platform-specific code
* `src/ui/` legacy Sciter UI (deprecated)
* `flutter/` current UI
* `libs/hbb_common/` shared with the server: rendezvous proto, sockets, `Config` core
* `libs/base/` (crate `base`) client-only: option keys, message proto, file transfer, platform code
* `libs/scrap/` screen capture
* `libs/enigo/` input control
* `libs/clipboard/` clipboard
* `libs/base/src/config/keys.rs` the single import path for all options

### Key Components
- **Remote Desktop Protocol**: Custom protocol implemented in `src/rendezvous_mediator.rs` for communicating with rustdesk-server
- **Screen Capture**: Platform-specific screen capture in `libs/scrap/`
- **Input Handling**: Cross-platform input simulation in `libs/enigo/`
- **Audio/Video Services**: Real-time audio/video streaming in `src/server/`
- **File Transfer**: Secure file transfer implementation in `libs/base/src/fs.rs`

`hbb_common` is a git submodule shared with the server, so changing it costs a
round-trip. Put client-only code in `libs/base` instead; it is a normal
workspace member. `base::config::keys` re-exports the handful of keys
`hbb_common` still reads, so callers get the whole set from that one path.

### UI Architecture
- **Legacy UI**: Sciter-based (deprecated) - files in `src/ui/`
- **Modern UI**: Flutter-based - files in `flutter/`
  - Desktop: `flutter/lib/desktop/`
  - Mobile: `flutter/lib/mobile/`
  - Shared: `flutter/lib/common/` and `flutter/lib/models/`

## OpenHarmony (RustDesk Unofficial) port

This checkout is upstream RustDesk plus the HarmonyOS port and its release chain: `flutter/ohos/`
(ArkTS shell, `AppScope/app.json5`), `.github/workflows/flutter-*.yml`, `scripts/build-ohos-*.sh`,
`scripts/prepare-ohos-flutter.py`, `store-listing/`. The rules below are invariants learned the hard
way; `HANDOVER.md` is the longer narrative and its §6/§8 are superseded by this section.

### Dependencies (the classic red-nightly trap)

* `flutter/pubspec.yaml` is the single dependency source for every leg: stock Flutter 3.24.5 (linux,
  windows x64, android, ios, darwin), 3.44 (windows arm64) and Flutter-OH 3.41 (OHOS). Committed
  values must resolve under Dart 3.5.4 with no rewriting.
* `settings_ui`, `flex_color_picker` and `xterm` point at forks. Keep the **standard** refs
  committed; `scripts/prepare-ohos-flutter.py` swaps them to the OHOS refs inside the OHOS leg.
  Committing the swapped output makes every other leg compile HarmonyOS-only `TargetPlatform.ohos`
  and die in `kernel_snapshot_program`. `--check`, run by the `verify-dependency-state` job in
  `flutter-build.yml` (which also runs the preparer's unit test), catches exactly that.
* `extended_text: ^14.2.0` and `google_fonts: ^6.2.1` are the only values the 3.24.5 legs and the
  OHOS preparer both accept.

### CI

* `flutter-nightly.yml` → `flutter-build.yml` → `{bridge.yml, flutter-ohos.yml}`. The OHOS leg
  (`flutter-ohos.yml`, macos-15) builds the signed App Pack, uploads the unsigned HAP and submits
  the AppGallery Connect test version.
* AppGallery keeps one invitation-test version under review at a time. A second submission inside
  that window fails with `the versionName and versionCode of the pkg is same with other pkg in use`
  (or `the count of harmony test in audit is up to the limit`). That is a server-side window, not a
  build fault: serialise submissions rather than chasing it in the workflow.
* Every submission still needs a fresh `versionCode`, and exactly one layer reaches the packaged
  App: the Flutter tool. `flutter_tools` copies the pubspec's `version:` into `AppScope/app.json5`,
  and hvigor's flutter plugin then writes `local.properties`' `flutter.versionCode` into the
  manifest, after any `hvigorfile.ts` hook has run - rewriting `app.json5` from CI and overriding it
  from a hvigorfile hook were both measured and both lost (`pack.info ... versionCode=68`).
  `scripts/build-ohos-flutter-hap.sh` derives
  `versionCode = 100000000 + UTC-days-since-2020-01-01 * 100000 + run-number * 100 + run-attempt`
  and passes it as `flutter build --build-number`; without `GITHUB_RUN_NUMBER` a local build keeps
  the committed baseline. `pack.info ... versionCode=` in the leg log is the proof.
* The Flutter-OH SDK (gitcode clone + Huawei OBS dart/engine zips) fails intermittently from GitHub
  runners with `curl: (6) Could not resolve host: flutter-ohos.obs.cn-south-1.myhuaweicloud.com`.
  Keep the `Restore/Save the Flutter-OH SDK` cache steps and the provisioning/packaging retries.

### HarmonyOS client behaviour

* Rotation: `auto_rotation` follows the sensor regardless of the control centre lock; only the
  `*_restricted` values honour it, so both abilities use `auto_rotation_restricted`.
* Secure keyboard: HarmonyOS raises it for `TextInputType.visiblePassword` inputs only, while
  Android and iOS raise it from `obscureText` and treat the visible-password type as the plain,
  suggestion-free one. Use `plainKeyboardType` / `secureKeyboardType` from `flutter/lib/common.dart`;
  only the relay Key field and the account password ask for it. On a device, a focused Key field
  makes the system refuse screen capture (the frame comes back black) while a plain field captures
  normally - that difference is the check.
* Text chat is off on HarmonyOS: `isTextChatSupported` gates every entry point and
  `chat_client_mode` / `chat_server_mode` events are dropped, so Android keeps the feature and
  HarmonyOS has no tab, button or unread counter.
* `isOhosDesktop` (deviceType `2in1` or a freeform window) switches to the desktop UI; a tablet runs
  the mobile UI. The DevEco emulator at `127.0.0.1:5555` is `2in1`, so tablet-shaped screens need a
  real device.

### Local OHOS build and device work

* Toolchain: `~/flutter-ohos/bin` and `~/command-line-tools/{bin,tool/node/bin}` on `PATH`, plus
  `JAVA_HOME` (zulu-17) and `DEVECO_SDK_HOME`. Then `OHOS_BUILD_MODE=debug OHOS_PACKAGE_KIND=hap
  OHOS_PRODUCT=default bash scripts/build-ohos-flutter-hap.sh` yields
  `flutter/ohos/entry/build/default/outputs/default/entry-default-signed.hap`.
  `OHOS_PACKAGE_KIND=app` also needs release mode, the `publish` product and `RUSTDESK_SIGNING_DIR`.
* Install with `hdc -t <target> install -r <hap>`; record `hdc shell bm dump -n <bundle>` before and
  after to confirm bundleName / versionCode / versionName / debug, and uninstall first when the
  signing identity differs - `hdc uninstall` deletes the app's data, so ask before using it on a
  user's device.
* `hdc shell uitest dumpLayout` and `uitest uiInput click` drive the app without a mouse. The OHOS
  secure keyboard collapses when a hardware keyboard is attached; its toolbar button (bottom right)
  re-opens it.
* Keep the screen awake (harmonyos skill script) while driving on-screen UI, and stop that job when
  the session ends.

### Store listing

* `store-listing/zh-CN/artwork/index.html` renders the promo posters at exactly 1080x1920, 1920x1280
  and 1920x1080 (`?scene=quality|keyboard|pointer&format=phone|tablet|landscape`); `overview.html`
  renders the 1600x1880 contact sheet. After editing, re-render the nine exports, refresh
  `sources.json` (bytes/sha256) and rebuild `rustdesk-store-assets.zip`.
* The exported PNGs are ignored by `.gitignore` (`*png`); the ZIP is the tracked deliverable, so a
  fresh clone has no poster files on disk.

## Rust Rules

* Avoid `unwrap()` / `expect()` in production code.
* Exceptions:

  * tests;
  * lock acquisition where failure means poisoning, not normal control flow.
* Otherwise prefer `Result` + `?` or explicit handling.
* Do not ignore errors silently.
* Avoid unnecessary `.clone()`.
* Prefer borrowing when practical.
* Do not add dependencies unless needed.
* Keep code simple and idiomatic.

## Tokio Rules

* Assume a Tokio runtime already exists.
* Never create nested runtimes.
* Never call `Runtime::block_on()` inside Tokio / async code.
* Do not hide runtime creation inside helpers or libraries.
* Do not hold locks across `.await`.
* Prefer `.await`, `tokio::spawn`, channels.
* Use `spawn_blocking` or dedicated threads for blocking work.
* Do not use `std::thread::sleep()` in async code.

## Editing Hygiene

* Change only what is required.
* Prefer the smallest valid diff.
* Do not refactor unrelated code.
* Do not make formatting-only changes.
* Keep naming/style consistent with nearby code.

### Imports

* One `use` per crate. Everything a file takes from the same crate goes in a
  single braced block, not one statement per item:

  ```rust
  // no
  use base::fs;
  use base::message_proto::*;

  // yes
  use base::{fs, message_proto::*};
  ```

* The only reason to split is a `#[cfg(...)]` that does not apply to the whole
  block -- an attribute binds to one item, so a differently-gated import has to
  stand on its own. A `pub use` re-export likewise cannot join a plain `use`.

  ```rust
  #[cfg(not(feature = "flutter"))]
  use base::fs;
  use base::message_proto::*;
  ```

* When splitting an existing `use` because some of its items moved to another
  crate, fold each side into that crate's existing block rather than leaving a
  second statement behind.

### Comments

* Avoid comments unless they explain a non-obvious reason, constraint, or workaround.
* Never restate what the code does; prefer clearer code instead.
* If the code is self-explanatory, add no comment.

### Be minimally invasive

* Prefer purely additive changes: layer new (`#[cfg]`-gated) blocks or new functions around existing code instead of restructuring it. The ideal diff for a fix adds lines and modifies/deletes none.
* Do not extract or reshape existing code just to enable your new code; look for a mechanism that leaves existing lines untouched (e.g. hide/show an existing object instead of refactoring its construction into a helper for rebuilding).
* Accept a little duplication over a restructure. A new function that repeats a few lines of an existing one is a better diff than reshaping the original so both can share it.
* Put new logic in self-contained functions in the module it belongs to (platform-specific logic in `src/platform/`, with `use` inside the function body to avoid churning shared import blocks). Call sites in shared files (`src/tray.rs`, `src/core_main.rs`, `src/server/connection.rs`, …) should be thin one-line hooks.

### Scope check before touching shared code

* Before changing a shared trait, a shared struct, or the signature of a widely used function, check whether the bug or feature is specific to one path. If it is, keep the change inside that path unless that is impossible, and say in the PR why it was.
* If an unrelated caller needs `Default::default()`, `None`, or another placeholder solely to satisfy a signature you changed, the diff is too broad: stop and redesign.
* The expected shape of a fix is a new function in the feature's own module, plus at most a new field or a thin hook in the shared code it needs. Feature-specific state belongs beside the feature's existing state, not in a new abstraction every caller has to learn.

### Mandatory regression-surface check

Before considering any implementation complete, perform a minimization pass over the final diff.

* Inspect every modified existing file and every modified existing code path. Each must be strictly necessary for the requested change. Revert changes that are merely cleanup, refactoring, consistency improvements, or fixes for pre-existing issues.
* For new features, preserve the existing implementation path when the feature is disabled or unsupported whenever practical. `feature off` should run the old code, not a rewritten equivalent.
* Do not route existing behavior through a new abstraction merely to share code with the new feature. Prefer a parallel new function or a small amount of duplication over changing a proven existing path.
* Keep new implementation logic in new or feature-specific modules. Changes to shared/core files should normally be thin hooks, capability checks, or protocol plumbing.
* Do not fix unrelated pre-existing bugs in the same PR. Put them in a separate change unless they directly block correctness or security of the requested work.
* For submodule bumps, inspect the exact commit range and ensure unrelated changes are not being pulled into the parent PR.
* Before finalizing, explicitly report the regression surface: list the existing files and existing runtime paths whose behavior changed, and explain why each change is unavoidable.
* During review, treat an unnecessarily modified legacy path as a review finding even if tests pass and the rewritten behavior appears equivalent.

## Reviewing a PR

* Review only what the diff introduces. Verify ownership with `gh pr diff` before reporting a finding — if the offending lines are untouched context, it is a pre-existing problem, not this PR's.
* List pre-existing problems in a separate section at the end, or leave out the ones that are not fatal. Never mix them into the findings the author has to fix.
* Before re-reviewing, read the author's reply comments. Do not re-raise items they declined on scope grounds.
* State a finding's consequence exactly: distinguish "the value is lost" from "the shortcut is inert but the value still saves".

## Localization (`src/lang/*.rs`)

Each file is a `HashMap<key, translation>`. Layout:

* `template.rs` is the master list of every key. **Never edit it** as part of translation work.
* `en.rs` holds only the keys whose English display text differs from the key itself.
* Every other file (`de.rs`, `fr.rs`, …) carries the full key set; an untranslated entry has an empty value: `("key", "")`.
* `it.rs` is maintained by hand by its translator. Never fill or change its entries; when adding new keys, append them to it with `""` and leave the translation to the maintainer.

### Finding the English source for a key

When filling an empty entry, determine the source English text with this rule:

* If `key` exists in `en.rs` **with a non-empty value**, that value is the source text (look it up in `en.rs`).
* Otherwise the **key string itself is the source text** (the key is already plain English).

Then translate that source into the file's target language (infer the language from the file's existing non-empty entries / filename).

### Translation hygiene

* Only fill empty values. Never change keys, and never touch existing non-empty translations.
* Preserve placeholders (`{}`) and escape sequences (`\n`, `\"`) exactly as in the source.
* Do not translate brand or technical tokens: `RustDesk`, `Socks5`, `TLS`, `UAC`, `Wayland`, `X11`, `TCP`, `UDP`, `2FA`, `RDP`, `D3D`, etc.
* Copy URL values (e.g. `doc_*` keys) verbatim from `en.rs`.

### Adding new keys (feature work)

* New English-text keys use sentence case, not Title Case: `Use ID whitelisting`, **not** `Use ID Whitelisting`. Acronyms (ID, IP, 2FA…) stay uppercase. Legacy Title-Case keys (e.g. `Use IP Whitelisting`) stay as-is — do not rename them.
* Since the key itself is the English display text, a sentence-case key usually needs **no** `en.rs` entry; add one only when the display text must differ from the key (e.g. `*_tip` keys).
* Append each new key to `template.rs` (with `""`) and to every `src/lang/*.rs` file (translated, or `""` if unsure; always `""` for `it.rs`), at the end of the list.
