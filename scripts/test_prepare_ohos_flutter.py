#!/usr/bin/env python3
"""Regression coverage for SDK selection and temporary build dependencies."""

import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('prepare_ohos_flutter', ROOT / 'scripts/prepare-ohos-flutter.py')
PREPARE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PREPARE)
BASE = 'name: sdk_selection_test\ndependencies:\n' + ''.join(
    f'  {name}:\n    git:\n      url: {url}\n      ref: {standard}\n'
    for name, url, standard, ohos in PREPARE.DEPENDENCIES
) + '  extended_text: ^14.2.0\n  google_fonts: ^8.2.1\ndependency_overrides:\n  intl: ^0.19.0\n'


class DependencyPreparationTest(unittest.TestCase):
    def test_selects_ohos_without_changing_other_options(self):
        selected = PREPARE.prepare(BASE)
        expected = BASE.replace('extended_text: ^14.2.0', 'extended_text: ^15.0.2')
        expected = expected.replace('google_fonts: ^8.2.1', 'google_fonts: ^8.1.0')
        for name, url, standard, ohos in PREPARE.DEPENDENCIES:
            expected = expected.replace(standard, ohos)
        self.assertEqual(selected, expected)

    def test_accepts_partial_and_already_prepared_states(self):
        name, url, standard, ohos = PREPARE.DEPENDENCIES[0]
        selected = PREPARE.prepare(BASE)
        self.assertEqual(PREPARE.prepare(BASE.replace(standard, ohos)), selected)
        for version in ('^6.2.1', '^8.1.0', '^8.2.1'):
            source = BASE.replace('google_fonts: ^8.2.1', f'google_fonts: {version}')
            self.assertEqual(PREPARE.prepare(source), selected)
        self.assertEqual(PREPARE.prepare(selected), selected)

    def test_rejects_unknown_dependency_revision(self):
        standard = PREPARE.DEPENDENCIES[-1][2]
        with self.assertRaisesRegex(ValueError, 'xterm'):
            PREPARE.prepare(BASE.replace(standard, '0' * 40))

    def test_check_rejects_a_prepared_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            pubspec = Path(directory) / 'pubspec.yaml'
            command = [sys.executable, str(ROOT / 'scripts/prepare-ohos-flutter.py'),
                       '--pubspec', str(pubspec), '--check']
            pubspec.write_text(BASE)
            standard = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertEqual(standard.returncode, 0, standard.stderr)
            self.assertEqual(pubspec.read_text(), BASE)
            pubspec.write_text(PREPARE.prepare(BASE))
            prepared = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertNotEqual(prepared.returncode, 0)
            self.assertIn('already prepared', prepared.stderr)
            self.assertEqual(pubspec.read_text(), PREPARE.prepare(BASE))

    def test_rejects_missing_or_unknown_font_dependency(self):
        for dependency in ('', '  google_fonts: ^9.0.0\n'):
            with self.subTest(dependency=dependency):
                with self.assertRaisesRegex(ValueError, 'google_fonts'):
                    PREPARE.prepare(BASE.replace('  google_fonts: ^8.2.1\n', dependency))

    def test_cli_failure_leaves_input_unchanged(self):
        invalid = BASE.replace('  extended_text: ^14.2.0\n', '')
        with tempfile.TemporaryDirectory() as directory:
            pubspec = Path(directory) / 'pubspec.yaml'
            pubspec.write_text(invalid)
            result = subprocess.run(
                [sys.executable, str(ROOT / 'scripts/prepare-ohos-flutter.py'), '--pubspec', str(pubspec)],
                capture_output=True, text=True, timeout=10,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('pubspec was not changed', result.stderr)
            self.assertEqual(pubspec.read_text(), invalid)

    @unittest.skipUnless(os.name == 'posix', 'OHOS packaging uses a POSIX build host')
    def test_build_failure_restores_manifest_and_lockfile(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scripts, flutter, tools = root / 'scripts', root / 'flutter', root / 'bin'
            for path in (scripts, flutter, tools):
                path.mkdir()
            for name in ('build-ohos-flutter-hap.sh', 'prepare-ohos-flutter.py'):
                shutil.copy2(ROOT / 'scripts' / name, scripts / name)
            (flutter / 'pubspec.yaml').write_text(BASE)
            (flutter / 'pubspec.lock').write_text('original lockfile\n')
            # The external build changes its lockfile and then fails.
            (tools / 'flutter').write_text('#!/bin/sh\nprintf "build lockfile\\n" > pubspec.lock\nexit 64\n')
            (tools / 'hvigorw').write_text('#!/bin/sh\nexit 0\n')
            for path in tools.iterdir():
                path.chmod(0o755)
            environment = os.environ.copy()
            environment.pop('RUSTDESK_SIGNING_DIR', None)
            environment.pop('OHOS_BUILD_MODE', None)
            environment['PATH'] = str(tools) + os.pathsep + str(Path(sys.executable).parent) + os.pathsep + os.environ.get('PATH', '/usr/bin:/bin')
            result = subprocess.run(
                ['bash', str(scripts / 'build-ohos-flutter-hap.sh')],
                cwd=root, env=environment, capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(result.returncode, 64, result.stdout + result.stderr)
            self.assertEqual((flutter / 'pubspec.yaml').read_text(), BASE)
            self.assertEqual((flutter / 'pubspec.lock').read_text(), 'original lockfile\n')


if __name__ == '__main__':
    unittest.main()
