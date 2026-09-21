# RustDesk Unofficial（rustdesk4ohos）交接文档

- 写入时间：2026-09-16
- 适用工作区：`/Users/frankhan/HarmonyOS/P_RustDesk/R_RustDesk-Flutter`
- 事实基准：本地 `main` = `abcb8e203`（本文档之前最后一条代码提交）
- 证据来源：GitHub Actions run 35039790140（上游 rustdesk/rustdesk，作为对照）、35040623329、35044623413、35045057983；pub.dev API；本地脚本 dry-run
- 本文只覆盖本工作区（= `FrankHan052176/rustdesk4ohos` 的本地检出）。其余三个工程的关系见 §2。

## 0. 给接手人的三句话

1. 本工作区 = **RustDesk 上游仓库的 fork**（Rust 内核 + Flutter 前端 + 全部 CI），在它上面叠了 HarmonyOS/AGC 的发布链；`main` 是"RustDesk Unofficial"主线。
2. **最容易踩的坑是依赖约束**：`flutter/pubspec.yaml` 的同一份值要同时被 Flutter 3.24.5、3.44 和 Flutter-OH 3.41 三条腿接受，后两者靠脚本在构建时现场改写（§6）。
3. 改 CI 后**必须跑一次完整 nightly** 才算验证过（§7），不要只看单条腿。

## 1. 工作区与远端

```
origin    https://github.com/FrankHan052176/rustdesk4ohos.git      # 我方 fork（推送目标）
upstream  https://github.com/rustdesk/rustdesk.git                 # 上游，只用于 fetch/merge
zc        /Volumes/RustDeskBuildCache/native-workspace/R_RustDesk-Core
```

- 本地路径即工作区根目录，`.git` 就在根上。
- 默认分支 `main`，也就是 UO 主线；推送用 `main`。

## 2. 四个工程的关系

| 工程 | 角色 |
| --- | --- |
| `R_RustDesk-Flutter`（本工作区） | RustDesk 全量检出：`src/` Rust 内核、`flutter/` 前端、`.github/workflows/` CI，以及 HarmonyOS/AGC 发布链 |
| `R_RustDesk-Core` | ArkTS 侧使用的 Rust 内核工作副本（`zc` remote 指向它），与 HAR/ArkTS 协作 |
| `R_RustDesk-Har` | 把 Rust 内核封装成 HarmonyOS HAR 的 NAPI 桥接工程 |
| `R_RustDesk-ArkTS` | HarmonyOS 客户端（ArkTS/ArkUI），消费 HAR |

跨层改动方向是 `Core → HAR → ArkTS`；本工作区负责的是**内核 + Flutter 客户端 + 发布链**。Flutter 前端同时是 ArkTS 端功能迁移的参考实现。

## 3. 分支现状

| 分支 | HEAD | 状态 | 用途 |
| --- | --- | --- | --- |
| `main` | `abcb8e203` | 与 origin 同步 | **UO 主线**，OHOS/AGC 发布链都在这里 |
| `flutter-ohos` | `a4ecde85d` | 已推送 | 早期 OHOS 移植分支（历史，勿在其上开发） |
| `master` | `f42906f55` | ahead 37 / behind 13 | 旧主线，保留备查 |
| `fix/flutter-ci-ohos-platform-compat` | `cb8b7e2e4` | ahead 3 | OHOS 平台兼容修补（历史） |
| `perf/zero-copy-high-fps` | `7ca96d3cb` | 已推送 | 零拷贝/高帧率实验 |
| `port/zero-copy-into-r4o` | `a4ecde85d` | 已推送 | 把零拷贝移植进 R4O 的实验 |

## 4. 本轮改动清单

| 提交 | 内容 | 验证 |
| --- | --- | --- |
| `e4dd257bb` | 合并上游 rustdesk/rustdesk 进 UO 主线 | 合并后内核与 App 可编译 |
| `18fa131db` | OHOS：接上签名 .app 与 AGC 发布链（含 WebRTC/STUN 启用） | OHOS 腿完整跑通 |
| `1fdf382d2` | 删除非上游的 HAR 校验 job | nightly 通过 |
| `b3ba4448d` | 新增 AGC 测试版本提交；修 3.22.3 bridge 腿依赖解析 | AGC 提交成功 |
| `1f44d943f` | AGC 开放测试窗口（提交后 1 小时开始，默认 60 天） | 线上提交成功 |
| `5af5cfae2`→`d18f4069f`→`e5aeff025` | 权限说明视频：可选 → 无视频时跳过 → **整段删除** | 该应用不声明 ACL 权限，本就不该有视频 |
| `d0445649a` | nightly Release 附件换成**未签名 HAP** | 附件名 `rustdesk-<版本>-ohos-arm64-unsigned.hap` |
| `efc1b3888` | workflow artifacts 只保留未签名 HAP | 删掉 bridge `.so` 与签名 `.app` 两个 artifact |
| `cb01472d2` | 把 committed pubspec 恢复到 3.24.5 兼容值 | 见 §6 |
| `abcb8e203` | `extended_text` 钉到 `^14.2.0`（三方腿通用值） | 两个改写脚本本地 dry-run 通过 |

## 5. CI 架构

### 触发链

```
flutter-nightly.yml       cron "0 0 * * *" + workflow_dispatch
  └─ flutter-build.yml    全平台矩阵 + generate-bridge + OHOS job
       ├─ bridge.yml                生成两个 bridge 产物
       └─ flutter-ohos.yml          OHOS 腿（ubuntu-22.04，timeout 90min）
```

- `flutter-build.yml` 的 `workflow_call` 输入：`upload-artifact`、`upload-agc`、`upload-tag`。
- `flutter-ohos.yml` 的输入：`upload-artifact`（默认 true）、`publish-release`（默认 false）、`upload-agc`（默认 false）、`upload-tag`（默认 `nightly`）、`version`（默认 `1.4.9`）。
- 手动重跑：`gh workflow run flutter-nightly.yml --repo FrankHan052176/rustdesk4ohos --ref main`。

### 版本矩阵（`flutter-build.yml` env）

| 变量 | 值 | 说明 |
| --- | --- | --- |
| `FLUTTER_VERSION` | `3.24.5` | linux/windows x64/i686/android/ios/darwin 都用它（Dart 3.5.4） |
| `ANDROID_FLUTTER_VERSION` | `3.24.5` | 同上 |
| `FLUTTER_WINDOWS_ARM_VERSION` | `3.44.9` | 仅 windows arm64；3.44 才原生支持 arm64 Dart SDK |
| `FLUTTER_ELINUX_VERSION` | `3.16.9` | linux arm64 |
| `FLUTTER_OH_VERSION` | `3.41.10-ohos-0.0.2-beta` | OHOS 腿，来自 `gitcode.com/CPF-Flutter/flutter_flutter` |
| `FLUTTER_RUST_BRIDGE_VERSION` | `1.80.1` | bridge 生成器 |
| `RUST_VERSION` / `MAC_RUST_VERSION` | `1.75` / `1.81` | 见 workflow 注释 |
| `VCPKG_COMMIT_ID` | `9e593bb18ea69cc5095e012465dcd675a822ed0d` | 改它必须同步 `vcpkg.json` baseline 以及 `ci.yml`/`playground.yml` |
| `VCPKG_CMAKE_VERSION` | `4.3.0` | vcpkg 自带的 cmake 供给 |
| `OHOS_COMMAND_LINE_TOOLS_VERSION` | `6.1.1.280` | HarmonyOS CLI tools |

### OHOS 腿步骤（`flutter-ohos.yml`）

1. Checkout（`submodules: recursive`）
2. 原生构建依赖（brew: cmake coreutils ninja pkg-config protobuf）
3. Setup Java 17 → 4. Setup HarmonyOS CLI tools → 5. Install Rust（`aarch64-unknown-linux-ohos`）→ 6. Cache Rust
7. 装 Flutter-OH 与 bridge codegen
8. **Resolve Flutter dependencies**：`python3 ../scripts/prepare-ohos-flutter.py` + `flutter pub get`
9. 交叉编译 bridge → 10. 校验 bridge
11. 准备签名材料（`OHOS_RELEASE_SIGNING_BASE64`）
12. 打包签名 App Pack → 13. 校验签名（`scripts/verify-ohos-release.py`、`scripts/test_ohos_release.py`）
14. **Stage the unsigned OpenHarmony HAP**（打包产物里必须有 `*-unsigned.hap`，否则报错退出）
15. Upload HAP artifact → 16. 提交 AGC 测试版本 → 17. 发布 Release 附件
18. 清理签名材料（`if: always()`）

### 产物与发布链

| 东西 | 位置 | 内容 |
| --- | --- | --- |
| workflow artifact | Actions artifacts | `rustdesk-flutter-ohos-hap` = `rustdesk-<VERSION>-ohos-arm64-unsigned.hap`（保留 14 天，缺失即失败） |
| bridge artifact | Actions artifacts | `bridge-artifact`（3.22.3 生成）与 `bridge-artifact-flutter-3.44` |
| Release 附件 | tag `nightly`（prerelease） | 只挂未签名 HAP；签名 `.app` 不进 Release |
| AppGallery Connect | 开放测试 | 用签名 `.app` 提交测试版本，不进生产、不进审核 |
| 其它平台 | Release `nightly` | 上游原有的 deb/rpm/apk/ipa 等附件，保持上游行为 |

要点：**签名 `.app` 只走 AGC；用户能拿到的是未签名 HAP**（自行签名）。

### 凭据与变量（只列名称与用途，值不入库）

| 名称 | 类型 | 用途 |
| --- | --- | --- |
| `OHOS_RELEASE_SIGNING_BASE64` | secret | 发布证书/Profile 的 base64 包，OHOS 腿必需 |
| `AGC_CLIENT_ID` / `AGC_CLIENT_SECRET` | secret | AppGallery Connect API 客户端凭据 |
| `SIGNING_REPOSITORY_TOKEN` | secret | 签名材料仓库访问 |
| `AGC_API_DOMAIN` | vars（可选） | 默认 `connect-api.cloud.huawei.com` |
| `AGC_TEST_DURATION_DAYS` | vars（可选） | 默认 `60` |

AGC 应用 id 硬编码在 `flutter-ohos.yml`：`6917615823381371195`；提交脚本 `.github/scripts/agc-test-release.sh`。
本轮已修：AGC 更新调用必须用 update 返回的版本 `objectId`；ACL 权限说明视频整段删除。

## 6. 依赖约束的三方机制（**本仓库最大的坑**）

`flutter/pubspec.yaml` 是**唯一**的依赖来源，但要同时喂三条腿：

| 腿 | Flutter | 谁改写 pubspec | 允许的 committed 值 |
| --- | --- | --- | --- |
| linux / windows x64 / i686 / android / ios / darwin | 3.24.5（Dart 3.5.4） | 不改写，直接消费 | 必须能被 Dart 3.5.4 解析 |
| windows arm64、`apply_flutter_3.44_web_patches.sh` 相关 | 3.44.x | `.github/patches/apply_flutter_3.44_source_patches.sh` | 任意值（脚本按包名整行替换） |
| OHOS | Flutter-OH 3.41 | `scripts/prepare-ohos-flutter.py` | `extended_text` 只接受 `^14.2.0`/`^15.0.2`；`google_fonts` 只接受 `^6.2.1`/`^8.1.0`；三个 git 依赖只接受"标准 ref"或"OHOS ref" |

当前 committed 值（**改之前先想清楚三条腿**）：

```yaml
extended_text: ^14.2.0     # 14.2.x: sdk >=3.5.0, flutter >=3.24.0
google_fonts: ^6.2.1
# settings_ui / flex_color_picker / xterm 走 git ref，OHOS 腿会把 ref 换成 OHOS 分支
```

事故记录（务必别重演）：

- `extended_text: ^15.0.2` → 需要 Dart ≥3.7，3.24.5 腿（Dart 3.5.4）**全部** `pub` 解析失败：
  `Because extended_text 15.0.2 requires SDK version >=3.7.0 <4.0.0 … version solving failed.`
  日志末尾的真实报错是 `flutter build linux --release` 失败，**不在**日志开头的 vcpkg/cmake 提示里（`cmake 4.4.0 not found` 是 vcpkg 自愈过程，紧跟其后就是 `Successfully downloaded`）。
- `extended_text: 14.0.0`（上游的硬钉法）→ OHOS 腿 preparer 断言失败：
  `Expected one supported extended_text dependency; pubspec was not changed`。

改动前的本地 dry-run（必须做，两个都要过）：

```bash
cd /Users/frankhan/HarmonyOS/P_RustDesk/R_RustDesk-Flutter
T=$(mktemp -d); mkdir -p "$T/flutter" "$T/scripts" "$T/.github/patches"
cp flutter/pubspec.yaml "$T/flutter/"; cp scripts/prepare-ohos-flutter.py "$T/scripts/"
cp .github/patches/apply_flutter_3.44_source_patches.sh "$T/.github/patches/"
(cd "$T/scripts" && python3 prepare-ohos-flutter.py)          # 期望 exit=0
(cd "$T" && bash .github/patches/apply_flutter_3.44_source_patches.sh)  # 期望 exit=0
```

另有干系文件：`scripts/test_prepare_ohos_flutter.py` 是 preparer 的单测（**CI 目前没有跑它**，改 preparer 时手动跑）。
`flutter/pubspec.lock` 目前仍记录 `extended_text 15.0.2` 与 `dart: ">=3.10.0 <4.0.0"`，与 pubspec 不一致；CI 的 `pub get` 会自动重解析，但工作树不干净，有条件时用 3.24.5 工具链 `flutter pub get` 后提交刷新。

## 7. 日常操作

```bash
# 触发整条 nightly（全平台 + OHOS）
gh workflow run flutter-nightly.yml --repo FrankHan052176/rustdesk4ohos --ref main

# 看某条 run 每条腿的结论
gh run view <RUN_ID> --repo FrankHan052176/rustdesk4ohos \
  --json jobs --jq '.jobs[] | "\(.conclusion // .status)  \(.name)"' | sort

# 看某条腿失败在哪一步
gh api "repos/FrankHan052176/rustdesk4ohos/actions/jobs/<JOB_ID>" \
  --jq '.steps[] | "\(.number)\t\(.conclusion)\t\(.name)"'

# 取某条腿的完整日志（返回的是原始文本，不是 zip）
gh api --allow-escape-sequences "repos/FrankHan052176/rustdesk4ohos/actions/jobs/<JOB_ID>/logs" > /tmp/job.log

# 对照上游是否同样绿（判断"是不是我们改坏的"）
gh run view <RUN_ID> --repo rustdesk/rustdesk --json jobs | head -5
```

判断口径：**先看上游同 workflow 是否绿**——上游绿而我们红，基本就是我们这边的改动（本轮 10 条平台腿全红就是这么定位的）。

## 8. 未完成事项与已知问题

1. **两条 nightly 在写文档时仍在跑**，接手第一件事是确认结论：
   - `35044623413` @ `cb01472d2`：OHOS 腿**失败**（`Resolve Flutter dependencies`，preparer 断言，已由 `abcb8e203` 修）；其余平台腿当时仍在跑。
   - `35045057983` @ `abcb8e203`：修复后的完整验证，排队中。
2. 平台腿（linux aarch64/sciter、ios、darwin、android 等）的**长期红腿**根因已定位为 §6 的 pubspec 约束问题；修好后需要**一轮完整 nightly 确认**，不要凭单条腿下结论。
3. `flutter/pubspec.lock` 落后于 pubspec（§6 末）。
4. `libs/hbb_common` 子模块指针是 `ded7f72a18fcc53380a2c33acfc68d44aef878a2`（内容为 `Merge rustdesk/hbb_common 29cf7cbe into ohos/core`），**有意**不同于上游指针 `29cf7cbe`；checkout 阶段的 `Git commit id not found.` 是非致命告警，不代表构建失败。
5. AGC 测试版本一旦提交即占用测试窗口（默认 60 天）；重复提交会产生新的 version，旧的可自行在 AGC 后台下架。
6. 本工作区的 `AGENTS.md` 是代码风格与 Rust/Tokio/本地化规则，改 `src/`、`libs/`、`src/lang/` 前先读它。

## 9. 回滚

| 目的 | 操作 |
| --- | --- |
| 只回退 Release 附件为签名 `.app` | `git revert d0445649a` |
| 恢复 artifacts 里同时有 `.app`/`.so` | `git revert efc1b3888` |
| 回退 pubspec 约束改动 | `git revert abcb8e203 cb01472d2` |
| 回退整个 OHOS 合并 | `git revert -m 1 18fa131db`（保留 AGC 脚本则按文件回退） |
| 上游同步 | `git fetch upstream && git merge upstream/master`，冲突集中在 `flutter/` 与 CI |

回滚后同样要跑一轮 nightly 验证。

## 10. 关键文件索引

| 路径 | 作用 |
| --- | --- |
| `.github/workflows/flutter-nightly.yml` | 夜间/手动入口 |
| `.github/workflows/flutter-build.yml` | 全平台矩阵 + OHOS job 装配 |
| `.github/workflows/flutter-ohos.yml` | OHOS 腿：签名、AGC、HAP 产物与 Release |
| `.github/workflows/bridge.yml` | 两套 bridge 产物（3.22.3 / 3.44.8） |
| `.github/patches/apply_flutter_3.44_source_patches.sh` | 3.44 腿的源码/pubspec 现场改写 |
| `.github/patches/apply_flutter_3.44_web_patches.sh` | 3.44 web 相关改写 |
| `scripts/prepare-ohos-flutter.py` | OHOS 腿的 pubspec/git-ref 改写（含断言） |
| `scripts/test_prepare_ohos_flutter.py` | 上述脚本的单测（CI 未接入） |
| `scripts/build-ohos-ffi.sh` | OHOS bridge 交叉编译 |
| `scripts/verify-ohos-release.py` / `scripts/test_ohos_release.py` | 签名 App Pack 校验 |
| `.github/scripts/agc-test-release.sh` | AGC 测试版本提交 |
| `.github/scripts/prepare-ohos-signing.py` | 从 base64 还原签名材料 |
| `flutter/pubspec.yaml` | 依赖约束的唯一来源（§6） |
| `AGENTS.md` | 本仓库代码规则（Rust/Tokio/本地化） |
