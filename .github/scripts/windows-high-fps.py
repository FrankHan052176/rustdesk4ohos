"""Windows CI-only compatibility overlay and complete-runtime packaging.

Never run prepare against a developer checkout. No dependency/version overrides:
the overlay removes OHOS-only enum cases and restores Flutter 3.24 theme names.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import time
from urllib.parse import unquote, urljoin, urlparse
from urllib.request import url2pathname
import zipfile


ROOT = Path(__file__).resolve().parents[2]


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha256(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def prepare():
    changes = []

    def replace(path, old, new, count):
        before = sha256(path)
        text = path.read_text(encoding='utf-8')
        require(text.count(old) == count, f'Compatibility overlay drift: {path}')
        path.write_text(text.replace(old, new), encoding='utf-8')
        changes.append({'file': str(path.relative_to(ROOT)) if path.is_relative_to(ROOT)
                        else str(path.relative_to(Path(os.environ['PUB_CACHE']).resolve())),
                        'old': old, 'new': new, 'count': count,
                        'beforeSha256': before, 'afterSha256': sha256(path)})

    replace(ROOT / 'flutter/lib/native/common.dart', 'Platform.isOhos', 'false', 1)
    for name in ('models/input_modifier_utils.dart',
                 'desktop/widgets/material_mod_popup_menu.dart'):
        replace(ROOT / 'flutter/lib' / name, '    case TargetPlatform.ohos:\n', '', 1)
    replace(ROOT / 'flutter/lib/common.dart', 'DialogThemeData(', 'DialogTheme(', 2)
    replace(ROOT / 'flutter/lib/common.dart', 'TabBarThemeData(', 'TabBarTheme(', 2)

    config = ROOT / 'flutter/.dart_tool/package_config.json'
    packages = {p['name']: p for p in json.loads(config.read_text())['packages']}
    dependencies = {
        'settings_ui': ('lib/src/utils/platform_utils.dart', 1),
        'xterm': ('lib/src/ui/shortcut/shortcuts.dart', 1),
        'flex_color_picker': ('lib/src/functions/picker_functions.dart', 2),
    }
    pub_cache = Path(os.environ['PUB_CACHE']).resolve()
    for name, (relative, count) in dependencies.items():
        uri = urlparse(urljoin(config.as_uri(), packages[name]['rootUri']))
        require(uri.scheme == 'file', f'Non-file Dart package: {name}')
        package = Path(url2pathname(unquote(uri.path))).resolve()
        require(package.is_relative_to(pub_cache), f'Package outside isolated PUB_CACHE: {name}')
        path = package / relative
        before = sha256(path)
        text = path.read_text(encoding='utf-8')
        pattern = r'^[ \t]*case TargetPlatform\.ohos:\n'
        require(len(re.findall(pattern, text, re.MULTILINE)) == count,
                f'OHOS dependency case drift: {name}')
        path.write_text(re.sub(pattern, '', text, flags=re.MULTILINE), encoding='utf-8')
        changes.append({'package': name, 'file': relative, 'removedOhosCases': count,
                        'beforeSha256': before, 'afterSha256': sha256(path)})
    (ROOT / 'windows-compatibility-overlay.json').write_text(json.dumps(changes, indent=2))


def check_pe(path):
    with path.open('rb') as source:
        header = source.read(64)
        require(len(header) == 64 and header[:2] == b'MZ', f'Not PE: {path}')
        source.seek(struct.unpack_from('<I', header, 60)[0])
        pe = source.read(26)
    require(len(pe) == 26 and pe[:4] == b'PE\0\0', f'Invalid PE: {path}')
    require(struct.unpack_from('<H', pe, 4)[0] == 0x8664, f'Not Windows x64: {path}')
    require(struct.unpack_from('<H', pe, 24)[0] == 0x20B, f'Not PE32+: {path}')


def package():
    bundle = ROOT / 'flutter/build/windows/x64/runner/Release'
    required = ['rustdesk.exe', 'librustdesk.dll', 'flutter_windows.dll',
                'dylib_virtual_display.dll', 'WindowInjection.dll',
                'data/icudtl.dat', 'data/app.so']
    for name in required:
        path = bundle / name
        require(path.is_file() and path.stat().st_size > 0, f'Missing runtime file: {name}')
    require(any((bundle / 'data/flutter_assets').rglob('*')), 'Missing Flutter assets')
    # CMake's actual install manifest includes every plugin/bundled runtime DLL.
    manifest = ROOT / 'flutter/build/windows/x64/install_manifest.txt'
    require(manifest.is_file(), 'Missing CMake install manifest')
    installed = manifest.read_text().splitlines()
    dlls = [Path(p).name for p in installed if p.lower().endswith('.dll')]
    require(len(set(dlls)) >= 5, 'Missing plugin DLL installation entries')
    for name in dlls:
        require((bundle / name).is_file(), f'Missing installed DLL: {name}')
    for path in bundle.rglob('*'):
        if path.suffix.lower() in ('.exe', '.dll'):
            check_pe(path)
    require(b'flutter_windows.dll' in (bundle / 'rustdesk.exe').read_bytes().lower(),
            'Executable is not the Flutter runner (possible Rust-only stub)')
    require(sha256(bundle / 'librustdesk.dll') == sha256(ROOT / 'target/release/librustdesk.dll'),
            'Packaged Core DLL differs from this build')
    sha = subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip()
    require(sha == os.environ['GITHUB_SHA'], 'Unexpected source SHA')
    metadata = {
        'source': sha, 'ref': os.environ['GITHUB_REF'],
        'submodules': subprocess.check_output(['git', 'submodule', 'status', '--recursive'], text=True),
        'features': ['flutter', 'hwcodec', 'vram'], 'arch': 'x86_64-pc-windows-msvc',
        'rust': os.environ['RUST_VERSION'], 'flutter': os.environ['FLUTTER_VERSION'],
        'llvm': os.environ['LLVM_VERSION'], 'frbCodegen': '1.80.1',
        'vcpkg': os.environ['VCPKG_COMMIT_ID'],
        'workflowSha256': sha256(ROOT / '.github/workflows/windows-high-fps.yml'),
        'helperSha256': sha256(Path(__file__)),
        'compatibilityOverlaySha256': sha256(ROOT / 'windows-compatibility-overlay.json'),
        'cargoLockSha256': sha256(ROOT / 'Cargo.lock'),
        'resolvedPubLockSha256': sha256(ROOT / 'flutter/pubspec.lock'),
        'engineArchiveSha256': (ROOT / 'engine-archive-sha256.txt').read_text(encoding='utf-8-sig').strip(),
        'elapsedSeconds': int(time.time()) - int(os.environ['BUILD_STARTED']),
        'acceptance': 'Build/package checks only; GPU, H265 and high-refresh runtime not tested.',
        'fpsHintPolicy': 'Initialization only: set the requested FPS before connecting; later changes need an existing rebuild or reconnect.',
        'zeroCopyAcceptance': 'Not achieved: existing Windows GPU backends still copy or convert into additional textures. No strict-zero-copy claim.',
        'scope': 'Hardware encoder initialization hint plus opt-in sender-stage diagnostics; sender ABR, pacing, codec, quality and existing fallback policy are unchanged.',
        'senderDiagnostics': {
            'enableEnvironment': 'RUSTDESK_SENDER_TRACE=1',
            'defaultEnabled': False,
            'readmeSha256': sha256(ROOT / 'docs/WINDOWS_SENDER_DIAGNOSTICS.md'),
            'acceptance': 'Diagnostic build only. Enqueue and successful transport send are not proof of client receipt or display.',
        },
        'optionalResources': 'USB virtual-display driver and remote-printer driver not bundled; physical-display remote control is the intended use.',
        'unsigned': True,
    }
    (bundle / 'BUILD-METADATA.json').write_text(json.dumps(metadata, indent=2))
    (bundle / 'WINDOWS-COMPATIBILITY-OVERLAY.json').write_bytes(
        (ROOT / 'windows-compatibility-overlay.json').read_bytes())
    (bundle / 'WINDOWS-SENDER-DIAGNOSTICS.md').write_bytes(
        (ROOT / 'docs/WINDOWS_SENDER_DIAGNOSTICS.md').read_bytes())
    output = ROOT / 'high-fps-output'
    output.mkdir(exist_ok=False)
    archive = output / f'rustdesk-sender-diagnostic-windows-x64-{sha[:12]}-unsigned.zip'
    with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as target:
        for path in sorted(bundle.rglob('*')):
            if path.is_file():
                target.write(path, Path('rustdesk') / path.relative_to(bundle))
    with zipfile.ZipFile(archive) as target:
        require(target.testzip() is None, 'ZIP CRC validation failed')
        require(all(f'rustdesk/{name}' in target.namelist() for name in required),
                'ZIP missing required runtime entries')
        require(target.read('rustdesk/WINDOWS-SENDER-DIAGNOSTICS.md') ==
                (ROOT / 'docs/WINDOWS_SENDER_DIAGNOSTICS.md').read_bytes(),
                'ZIP sender diagnostic guide differs from this source')
    (output / 'SHA256SUMS.txt').write_text(f'{sha256(archive)}  {archive.name}\n')
    (output / 'BUILD-METADATA.json').write_text(json.dumps(metadata, indent=2))
    print(f'Complete unsigned runtime: {archive.name}')


if __name__ == '__main__':
    require(sys.platform == 'win32' and os.environ.get('GITHUB_ACTIONS') == 'true',
            'This helper runs only on an isolated Windows GitHub Actions checkout')
    require(os.environ.get('GITHUB_REF') == 'refs/heads/perf/zero-copy-high-fps',
            'Only perf/zero-copy-high-fps may use this helper')
    require(Path.cwd().resolve() == ROOT, 'Run from the checked-out repository root')
    require(len(sys.argv) == 2 and sys.argv[1] in ('prepare', 'package'),
            'Usage: windows-high-fps.py prepare|package')
    {'prepare': prepare, 'package': package}[sys.argv[1]]()
