#!/usr/bin/env python3
"""Signing archive safety and real SDK release verification regressions."""

import importlib.util
import io
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile


ROOT = Path(__file__).resolve().parents[1]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


PREPARE = load('prepare_signing', ROOT / '.github/scripts/prepare-ohos-signing.py')
VERIFY = load('verify_release', ROOT / 'scripts/verify-ohos-release.py')


class SigningArchiveTest(unittest.TestCase):
    def test_rejects_traversal_and_links(self):
        for name, kind in (('../escape', tarfile.REGTYPE), ('/escape', tarfile.REGTYPE),
                           ('link', tarfile.SYMTYPE), ('link', tarfile.LNKTYPE)):
            with self.subTest(name=name, kind=kind), tempfile.TemporaryDirectory() as directory:
                stream = io.BytesIO()
                with tarfile.open(fileobj=stream, mode='w:gz') as archive:
                    info = tarfile.TarInfo(name)
                    info.type = kind
                    info.linkname = '../outside'
                    archive.addfile(info)
                with self.assertRaises(PREPARE.SigningError):
                    PREPARE.extract(stream.getvalue(), Path(directory))
                self.assertEqual(list(Path(directory).iterdir()), [])

    def test_existing_material_is_not_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            existing = Path(directory) / 'existing'
            existing.write_text('keep')
            with self.assertRaises(PREPARE.SigningError):
                PREPARE.prepare_directory(Path(directory))
            self.assertEqual(existing.read_text(), 'keep')

    def test_both_password_fields_are_required(self):
        for absent in ('keyPassword', 'storePassword'):
            with self.subTest(absent=absent):
                material = dict(keyPassword='test-only', storePassword='test-only')
                del material[absent]
                with self.assertRaisesRegex(PREPARE.SigningError, absent):
                    PREPARE.validate_passwords(material)

    def test_missing_secret_fails_without_creating_output(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / 'signing'
            environment = dict(os.environ)
            environment.pop('OHOS_RELEASE_SIGNING_BASE64', None)
            result = subprocess.run(
                [sys.executable, str(ROOT / '.github/scripts/prepare-ohos-signing.py'), str(target)],
                env=environment, capture_output=True, text=True, timeout=10,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('empty', result.stderr)
            self.assertFalse(target.exists())

    def test_release_metadata_fails_closed(self):
        valid = dict(debug=False, buildMode='release', versionCode=67)
        VERIFY._check_release_semantics(valid, 'entry.hap')
        for field, value in (('debug', True), ('debug', None), ('buildMode', 'debug'),
                             ('buildMode', None), ('versionCode', 0)):
            with self.subTest(field=field, value=value):
                with self.assertRaises(VERIFY.VerificationError):
                    VERIFY._check_release_semantics({**valid, field: value}, 'entry.hap')


@unittest.skipUnless(all(os.environ.get(key) for key in (
    'OHOS_RELEASE_TEST_APP', 'OHOS_RELEASE_TEST_PROFILE', 'OHOS_RELEASE_TEST_SIGN_TOOL',
)), 'requires a real signed APP, release profile and SDK signing tool')
class RealReleaseSignatureTest(unittest.TestCase):
    def setUp(self):
        self.app = Path(os.environ['OHOS_RELEASE_TEST_APP']).resolve()
        self.profile = Path(os.environ['OHOS_RELEASE_TEST_PROFILE']).resolve()
        self.jar = Path(os.environ['OHOS_RELEASE_TEST_SIGN_TOOL']).resolve()
        self.java = VERIFY._resolve_java(os.environ.get('OHOS_RELEASE_TEST_JAVA'))
        self.directory = tempfile.TemporaryDirectory(prefix='rustdesk-release-test-')
        self.addCleanup(self.directory.cleanup)

    def test_accepts_real_outer_signature_and_native_payload(self):
        report = VERIFY.verify_release(self.app, self.profile, self.java, self.jar)
        self.assertTrue(any('App Pack signature OK' in line for line in report))
        self.assertTrue(any('native arm64-v8a payload verified' in line for line in report))

    def test_rejects_unsigned_container_named_signed(self):
        target = Path(self.directory.name) / 'unsigned-signed.app'
        with zipfile.ZipFile(self.app) as source, zipfile.ZipFile(target, 'w') as destination:
            for info in source.infolist():
                destination.writestr(info, source.read(info))
        with self.assertRaisesRegex(VERIFY.VerificationError, 'verify-app rejected'):
            VERIFY.verify_release(target, self.profile, self.java, self.jar)

    def test_rejects_tampering_with_signed_content(self):
        target = Path(self.directory.name) / 'tampered-signed.app'
        shutil.copyfile(self.app, target)
        with zipfile.ZipFile(target) as archive:
            info = archive.getinfo('pack.info')
        with target.open('r+b') as stream:
            stream.seek(info.header_offset + 26)
            name_length, extra_length = struct.unpack('<HH', stream.read(4))
            stream.seek(info.header_offset + 30 + name_length + extra_length)
            offset = stream.tell()
            original = stream.read(1)
            self.assertEqual(len(original), 1)
            stream.seek(offset)
            stream.write(bytes([original[0] ^ 1]))
        with self.assertRaisesRegex(VERIFY.VerificationError, 'verify-app rejected'):
            VERIFY.verify_release(target, self.profile, self.java, self.jar)


if __name__ == '__main__':
    unittest.main()
