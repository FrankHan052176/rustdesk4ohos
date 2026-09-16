#!/usr/bin/env python3
"""Regression coverage for the derived OpenHarmony App version."""

import datetime as dt
import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    'derive_ohos_app_version', ROOT / 'scripts/derive-ohos-app-version.py')
DERIVE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DERIVE)

PUBSPEC = '''name: flutter_hbb
description: Your Remote Desktop Software

version: 1.5.0+68

environment:
  sdk: '^3.1.0'
'''
MANIFEST = '''{
  "app": {
    "bundleName": "top.frankhan.resk.flutter",
    "vendor": "FrankHan",
    "versionCode": 67,
    "versionName": "1.4.9",
    "icon": "$media:app_icon",
    "label": "$string:app_name"
  }
}
'''
DAY = dt.datetime(2026, 9, 16, 12, 0, tzinfo=dt.timezone.utc)


def pubspec_version(pubspec: str) -> str:
    return DERIVE.PUBSPEC_VERSION.search(pubspec).group(2)


def manifest_code(manifest: str) -> int:
    return int(DERIVE.VERSION_CODE.search(manifest).group(2))


def manifest_name(manifest: str) -> str:
    return DERIVE.VERSION_NAME.search(manifest).group(2)


class DerivationTest(unittest.TestCase):
    def test_derives_the_code_and_keeps_the_semver_name(self):
        pubspec, manifest, code = DERIVE.derive(PUBSPEC, MANIFEST, 12, 1, DAY)
        self.assertEqual(pubspec_version(pubspec), f'1.5.0+{code}')
        self.assertEqual(manifest_name(manifest), '1.5.0')
        self.assertEqual(manifest_code(manifest), code)
        self.assertGreater(code, DERIVE.CODE_OFFSET)

    def test_leaves_every_other_line_untouched(self):
        pubspec, manifest, _ = DERIVE.derive(PUBSPEC, MANIFEST, 12, 1, DAY)
        for line in PUBSPEC.splitlines():
            if not line.startswith('version:'):
                self.assertIn(line, pubspec.splitlines())
        for line in MANIFEST.splitlines():
            if 'versionName' in line or 'versionCode' in line:
                continue
            self.assertIn(line, manifest.splitlines())

    def test_is_monotonic_across_runs_days_and_attempts(self):
        _, _, first = DERIVE.derive(PUBSPEC, MANIFEST, 12, 1, DAY)
        _, _, later_run = DERIVE.derive(PUBSPEC, MANIFEST, 13, 1, DAY)
        _, _, later_attempt = DERIVE.derive(PUBSPEC, MANIFEST, 12, 2, DAY)
        _, _, later_day = DERIVE.derive(PUBSPEC, MANIFEST, 12, 1, DAY + dt.timedelta(days=1))
        self.assertLess(first, later_run)
        self.assertLess(first, later_attempt)
        self.assertLess(later_run, later_day)

    def test_rerunning_the_same_build_keeps_the_base_version(self):
        once, once_manifest, code = DERIVE.derive(PUBSPEC, MANIFEST, 12, 1, DAY)
        twice, twice_manifest, again = DERIVE.derive(once, once_manifest, 12, 1, DAY)
        self.assertEqual(once, twice)
        self.assertEqual(once_manifest, twice_manifest)
        self.assertEqual(code, again)
        self.assertEqual(manifest_name(once_manifest), '1.5.0')

    def test_rejects_versions_outside_the_run_identity_range(self):
        for run_number, run_attempt in ((0, 1), (1, 0), (1, 100), (1000, 1)):
            with self.subTest(run_number=run_number, run_attempt=run_attempt):
                with self.assertRaises(ValueError):
                    DERIVE.derive(PUBSPEC, MANIFEST, run_number, run_attempt, DAY)

    def test_rejects_an_unsupported_pubspec_version(self):
        for base in ('1.5', 'v1.5.0', 'beta'):
            with self.subTest(base=base):
                with self.assertRaisesRegex(ValueError, 'base version'):
                    DERIVE.derive(PUBSPEC.replace('version: 1.5.0+68', f'version: {base}'),
                                  MANIFEST, 1, 1, DAY)

    def test_rejects_missing_or_duplicated_version_lines(self):
        broken_pubspecs = (PUBSPEC.replace('version: 1.5.0+68\n', ''),
                           PUBSPEC.replace('version: 1.5.0+68\n', 'version: 1.5.0+68\nversion: 1.5.0+68\n'))
        for broken in broken_pubspecs:
            with self.subTest(kind='pubspec'):
                with self.assertRaisesRegex(ValueError, 'version line'):
                    DERIVE.derive(broken, MANIFEST, 1, 1, DAY)
        broken_manifests = (MANIFEST.replace('    "versionName": "1.4.9",\n', ''),
                            MANIFEST.replace('    "versionCode": 67,\n', ''))
        for broken in broken_manifests:
            with self.subTest(kind='manifest'):
                with self.assertRaisesRegex(ValueError, 'versionName|versionCode'):
                    DERIVE.derive(PUBSPEC, broken, 1, 1, DAY)

    def test_rejects_a_code_past_the_harmonyos_limit(self):
        far_future = dt.datetime(2200, 1, 1, tzinfo=dt.timezone.utc)
        with self.assertRaisesRegex(ValueError, 'HarmonyOS limit'):
            DERIVE.derive(PUBSPEC, MANIFEST, 1, 1, far_future)

    def test_cli_check_does_not_write_and_applies_to_both_files(self):
        with tempfile.TemporaryDirectory() as directory:
            pubspec = Path(directory) / 'pubspec.yaml'
            manifest = Path(directory) / 'app.json5'
            pubspec.write_text(PUBSPEC, encoding='utf-8')
            manifest.write_text(MANIFEST, encoding='utf-8')
            command = [sys.executable, str(ROOT / 'scripts/derive-ohos-app-version.py'),
                       '--pubspec', str(pubspec), '--app-json', str(manifest),
                       '--run-number', '12', '--run-attempt', '1']
            check = subprocess.run(command + ['--check'], capture_output=True, text=True, timeout=10)
            self.assertEqual(check.returncode, 0, check.stderr)
            self.assertIn('would be 1.5.0 (', check.stdout)
            self.assertEqual(pubspec.read_text(encoding='utf-8'), PUBSPEC)
            self.assertEqual(manifest.read_text(encoding='utf-8'), MANIFEST)

            applied = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertEqual(applied.returncode, 0, applied.stderr)
            self.assertIn('version: 1.5.0+34', pubspec.read_text(encoding='utf-8'))
            self.assertNotEqual(manifest.read_text(encoding='utf-8'), MANIFEST)


if __name__ == '__main__':
    unittest.main()
