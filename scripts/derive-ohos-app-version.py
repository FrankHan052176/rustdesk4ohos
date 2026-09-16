#!/usr/bin/env python3
"""Derive the OpenHarmony App version for one CI build.

AppGallery rejects a submission whose `versionName` and `versionCode` are already
in use, so a nightly that re-submits the same App Pack always fails with
`the versionName and versionCode of the pkg is same with other pkg in use`.
The version therefore has to move with the build, not with the committed file.

Scheme, mirroring R_RustDesk-ArkTS and the EasyTier app so all three projects
share one convention:

    versionCode = 100000000 + UTC-days-since-2020-01-01 * 100000
                              + run-number * 100 + run-attempt
    versionName = <base major.minor.patch>.<that same suffix>

The epoch is fixed so the code stays monotonic across year boundaries, the run
identity keeps two builds of the same day apart, and the 100000000 offset stays
above every previously published code. Only the runner's checkout is rewritten;
the committed AppScope/app.json5 keeps the local baseline.
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
BASE_VERSION = re.compile(r'^(\d+\.\d+\.\d+)(?:\.\d+)?$')


def derive(source: str, run_number: int, run_attempt: int, now: dt.datetime) -> tuple[str, str, int]:
    """Return the rewritten manifest and the derived (versionName, versionCode)."""
    if run_number < 1:
        raise ValueError(f'GitHub run number must be positive, got {run_number}')
    if not 1 <= run_attempt <= 99:
        raise ValueError(f'GitHub run attempt must be 1..99, got {run_attempt}')
    if run_number > 999:
        raise ValueError(
            f'GitHub run number {run_number} overflows the per-day sequence; '
            'widen the scheme before the 1000th run of a workflow'
        )

    match = VERSION_NAME.search(source)
    if match is None or len(VERSION_NAME.findall(source)) != 1:
        raise ValueError('Expected exactly one versionName in the App manifest')
    base = BASE_VERSION.match(match.group(2))
    if base is None:
        raise ValueError(f'Unsupported base versionName: {match.group(2)}')

    day = (now.astimezone(dt.timezone.utc) - EPOCH).days
    suffix = day * DAY_SCALE + run_number * RUN_SCALE + run_attempt
    version_name = f'{base.group(1)}.{suffix}'
    version_code = CODE_OFFSET + suffix
    if version_code > MAX_CODE:
        raise ValueError(f'App versionCode exceeds the HarmonyOS limit: {version_code}')

    patched, count = VERSION_NAME.subn(rf'\g<1>{version_name}\g<3>', source, count=1)
    if count != 1:
        raise ValueError('Expected exactly one versionName to rewrite')
    patched, count = VERSION_CODE.subn(rf'\g<1>{version_code}\g<3>', patched, count=1)
    if count != 1:
        raise ValueError('Expected exactly one versionCode to rewrite')
    return patched, version_name, version_code


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app-json', type=Path,
                        default=Path(__file__).resolve().parents[1] / 'flutter/ohos/AppScope/app.json5')
    parser.add_argument('--run-number', type=int, required=True)
    parser.add_argument('--run-attempt', type=int, required=True)
    parser.add_argument('--check', action='store_true',
                        help='print the derived version without writing the manifest')
    args = parser.parse_args()

    source = args.app_json.read_text(encoding='utf-8')
    patched, version_name, version_code = derive(
        source, args.run_number, args.run_attempt, dt.datetime.now(dt.timezone.utc))
    if args.check:
        print(f'App version would be {version_name} ({version_code})')
        return
    if patched != source:
        args.app_json.write_text(patched, encoding='utf-8')
    print(f'App version is {version_name} ({version_code})')


if __name__ == '__main__':
    main()
