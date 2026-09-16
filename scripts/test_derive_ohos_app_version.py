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


def version_code(manifest: str) -> int:
    return int(DERIVE.VERSION_CODE.search(manifest).group(2))


def version_name(manifest: str) -> str:
    return DERIVE.VERSION_NAME.search(manifest).group(2)


class DerivationTest(unittest.TestCase):
    def test_derives_suffix_from_base_version(self):
        manifest, name, code = DERIVE.derive(MANIFEST, 12, 1, DAY)
        self.assertTrue(name.startswith('1.4.9.'))
        self.assertEqual(name, f'1.4.9.{code - DERIVE.CODE_OFFSET}')
        self.assertEqual(version_name(manifest), name)
        self.assertEqual(version_code(manifest), code)

    def test_leaves_every_other_field_untouched(self):
        manifest, _, _ = DERIVE.derive(MANIFEST, 12, 1, DAY)
        for line in MANIFEST.splitlines():
            if 'versionName' in line or 'versionCode' in line:
                continue
            self.assertIn(line, manifest.splitlines())

    def test_is_monotonic_across_runs_days_and_attempts(self):
        _, _, first = DERIVE.derive(MANIFEST, 12, 1, DAY)
        _, _, later_run = DERIVE.derive(MANIFEST, 13, 1, DAY)
        _, _, later_attempt = DERIVE.derive(MANIFEST, 12, 2, DAY)
        _, _, later_day = DERIVE.derive(MANIFEST, 12, 1, DAY + dt.timedelta(days=1))
        self.assertLess(first, later_run)
        self.assertLess(first, later_attempt)
        self.assertLess(later_run, later_day)

    def test_rerunning_the_same_day_keeps_the_base_version(self):
        once, _, _ = DERIVE.derive(MANIFEST, 12, 1, DAY)
        twice, name, code = DERIVE.derive(once, 12, 1, DAY)
        self.assertEqual(once, twice)
        self.assertTrue(name.startswith('1.4.9.'))
        self.assertEqual(code, version_code(once))

    def test_rejects_versions_outside_the_run_identity_range(self):
        for run_number, run_attempt in ((0, 1), (1, 0), (1, 100), (1000, 1)):
            with self.subTest(run_number=run_number, run_attempt=run_attempt):
                with self.assertRaises(ValueError):
                    DERIVE.derive(MANIFEST, run_number, run_attempt, DAY)

    def test_rejects_an_unsupported_base_version(self):
        for base in ('1.4', 'v1.4.9', '1.4.9-beta'):
            with self.subTest(base=base):
                with self.assertRaisesRegex(ValueError, 'base versionName'):
                    DERIVE.derive(MANIFEST.replace('"1.4.9"', f'"{base}"'), 1, 1, DAY)
    def test_rejects_a_missing_or_duplicated_version_field(self):
        for broken in (MANIFEST.replace('    "versionName": "1.4.9",\n', ''),
                       MANIFEST.replace('    "versionName": "1.4.9",\n',
                                        '    "versionName": "1.4.9",\n    "versionName": "1.4.9",\n')):
            with self.subTest(broken=broken[:40]):
                with self.assertRaisesRegex(ValueError, 'versionName|versionCode'):
                    DERIVE.derive(broken, 1, 1, DAY)

    def test_rejects_a_code_past_the_harmonyos_limit(self):
        far_future = dt.datetime(2200, 1, 1, tzinfo=dt.timezone.utc)
        with self.assertRaisesRegex(ValueError, 'HarmonyOS limit'):
            DERIVE.derive(MANIFEST, 1, 1, far_future)

    def test_cli_check_does_not_write(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / 'app.json5'
            manifest.write_text(MANIFEST, encoding='utf-8')
            check = subprocess.run(
                [sys.executable, str(ROOT / 'scripts/derive-ohos-app-version.py'),
                 '--app-json', str(manifest), '--run-number', '12', '--run-attempt', '1', '--check'],
                capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(check.returncode, 0, check.stderr)
            self.assertIn('would be 1.4.9.', check.stdout)
            self.assertEqual(manifest.read_text(encoding='utf-8'), MANIFEST)

            applied = subprocess.run(
                [sys.executable, str(ROOT / 'scripts/derive-ohos-app-version.py'),
                 '--app-json', str(manifest), '--run-number', '12', '--run-attempt', '1'],
                capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(applied.returncode, 0, applied.stderr)
            self.assertNotEqual(manifest.read_text(encoding='utf-8'), MANIFEST)


if __name__ == '__main__':
    unittest.main()
