#!/usr/bin/env python3
"""Derive the OpenHarmony App version for one CI build.

AppGallery rejects a submission whose `versionName` and `versionCode` are already
in use, so a nightly that re-submits the same App Pack always fails with
`the versionName and versionCode of the pkg is same with other pkg in use`. The
version therefore has to move with the build, not with the committed files.

Scheme, mirroring R_RustDesk-ArkTS and the EasyTier app so all three projects
share one convention:

    versionCode = 100000000 + UTC-days-since-2020-01-01 * 100000
                              + run-number * 100 + run-attempt
    versionName = the base major.minor.patch, unchanged

The code alone makes the submitted pair unique, and it keeps the pubspec's
`version:` semver-valid (a fourth dotted number is not accepted there, and the
App Store would reject it on iOS). The base version comes from the Flutter
manifest because the OHOS build copies `flutter/pubspec.yaml`'s `version:` into
the App's versionName/versionCode (see flutter_tools' hvigor_utils); writing only
AppScope/app.json5 gets overwritten, so both files are rewritten together.

The epoch is fixed so the code stays monotonic across year boundaries, the run
identity keeps two builds of one day apart, and the 100000000 offset stays above
every previously published code. Only the runner's checkout is rewritten; the
committed files keep the local baseline.
"""

import argparse
import datetime as dt
import re
from pathlib import Path

EPOCH = dt.datetime(2020, 1, 1, tzinfo=dt.timezone.utc)
CODE_OFFSET = 100_000_000
DAY_SCALE = 100_000
RUN_SCALE = 100
MAX_CODE = 2_147_483_647

VERSION_NAME = re.compile(r'(?m)^(\s*"versionName":\s*")([^"]+)(".*)$')
VERSION_CODE = re.compile(r'(?m)^(\s*"versionCode":\s*)(\d+)(,.*)$')
PUBSPEC_VERSION = re.compile(r'(?m)^(version:\s*)(\S+)(\s*)$')
BASE_VERSION = re.compile(r'^(\d+\.\d+\.\d+)(?:[.+-].*)?$')


def base_version(pubspec: str) -> str:
    match = PUBSPEC_VERSION.search(pubspec)
    if match is None or len(PUBSPEC_VERSION.findall(pubspec)) != 1:
        raise ValueError('Expected exactly one version line in the Flutter pubspec')
    base = BASE_VERSION.match(match.group(2))
    if base is None:
        raise ValueError(f'Unsupported base version: {match.group(2)}')
    return base.group(1)


def derive(
    pubspec: str,
    manifest: str,
    run_number: int,
    run_attempt: int,
    now: dt.datetime,
) -> tuple[str, str, int]:
    """Return (pubspec, manifest, versionCode) for this build."""
    if run_number < 1:
        raise ValueError(f'GitHub run number must be positive, got {run_number}')
    if not 1 <= run_attempt <= 99:
        raise ValueError(f'GitHub run attempt must be 1..99, got {run_attempt}')
    if run_number > 999:
        raise ValueError(
            f'GitHub run number {run_number} overflows the per-day sequence; '
            'widen the scheme before the 1000th run of a workflow'
        )

    version_name = base_version(pubspec)
    day = (now.astimezone(dt.timezone.utc) - EPOCH).days
    version_code = CODE_OFFSET + day * DAY_SCALE + run_number * RUN_SCALE + run_attempt
    if version_code > MAX_CODE:
        raise ValueError(f'App versionCode exceeds the HarmonyOS limit: {version_code}')

    patched_pubspec, count = PUBSPEC_VERSION.subn(
        rf'\g<1>{version_name}+{version_code}\g<3>', pubspec, count=1)
    if count != 1:
        raise ValueError('Expected exactly one version line to rewrite')

    if len(VERSION_NAME.findall(manifest)) != 1:
        raise ValueError('Expected exactly one versionName in the App manifest')
    if len(VERSION_CODE.findall(manifest)) != 1:
        raise ValueError('Expected exactly one versionCode in the App manifest')
    patched_manifest = VERSION_NAME.sub(rf'\g<1>{version_name}\g<3>', manifest, count=1)
    patched_manifest = VERSION_CODE.sub(rf'\g<1>{version_code}\g<3>', patched_manifest, count=1)
    return patched_pubspec, patched_manifest, version_code


def main() -> None:
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pubspec', type=Path, default=root / 'flutter/pubspec.yaml')
    parser.add_argument('--app-json', type=Path, default=root / 'flutter/ohos/AppScope/app.json5')
    parser.add_argument('--run-number', type=int, required=True)
    parser.add_argument('--run-attempt', type=int, required=True)
    parser.add_argument('--check', action='store_true',
                        help='print the derived version without writing anything')
    args = parser.parse_args()

    pubspec = args.pubspec.read_text(encoding='utf-8')
    manifest = args.app_json.read_text(encoding='utf-8')
    new_pubspec, new_manifest, version_code = derive(
        pubspec, manifest, args.run_number, args.run_attempt, dt.datetime.now(dt.timezone.utc))
    if args.check:
        print(f'App version would be {VERSION_NAME.search(new_manifest).group(2)} ({version_code})')
        return
    if new_pubspec != pubspec:
        args.pubspec.write_text(new_pubspec, encoding='utf-8')
    if new_manifest != manifest:
        args.app_json.write_text(new_manifest, encoding='utf-8')
    print(f'App version is {VERSION_NAME.search(new_manifest).group(2)} ({version_code})')


if __name__ == '__main__':
    main()
