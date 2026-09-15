#!/usr/bin/env python3
"""AppGallery Connect upload-only contract regressions.

Every request is served from a scripted fake transport, so no request leaves the
machine and no credential is required to run these tests.
"""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock
import urllib.parse
import zipfile


ROOT = Path(__file__).resolve().parents[1]

TOKEN = 'test-access-token'
CLIENT_SECRET = 'test-client-secret'
UPLOAD_URL = 'https://obs.cn-north-4.myhuaweicloud.com/bucket/object?signature=test-signature'
OBJECT_ID = 'object-test-1'
PACKAGE_ID = 'package-test-1'
APP_FILE = 'rustdesk-1.4.9-ohos-arm64.app'
API_HOST = 'connect-api.cloud.huawei.com'
ALLOWED_API_PATHS = ('/api/oauth2/v1/token', '/api/publish/v2/upload-url/for-obs',
                     '/api/publish/v3/app-package-info',
                     '/api/publish/v3/package/compile/status')
FORBIDDEN_MARKERS = ('app-submit', 'submit', 'test/version', 'test-group', 'app-test',
                     'release', 'review')


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


UPLOAD = load('upload_agc', ROOT / 'scripts/upload-agc.py')


def json_response(payload, status=200):
    return status, json.dumps(payload).encode('utf-8')


def token_response(token=TOKEN):
    # The OAuth2 token response of the AppGallery Connect API has no ret section.
    return json_response({'access_token': token, 'expires_in': 172800})

def registered_response():
    return json_response({'ret': {'code': 0}, 'packageId': PACKAGE_ID})


def package_response(status=0, package_id=PACKAGE_ID):
    return json_response({'ret': {'code': 0}, 'pkgStateList': [
        {'pkgId': package_id, 'successStatus': status}]})


def upload_url_response(url=UPLOAD_URL, method='PUT', object_id=OBJECT_ID, headers=None):
    if headers is None:
        headers = [{'key': 'Content-Type', 'value': 'application/octet-stream'}]
    return json_response({'ret': {'code': '0'}, 'urlInfo': {'url': url, 'method': method,
                                                            'objectId': object_id,
                                                            'headers': headers}})


class Call:
    def __init__(self, method, label, url, headers, body, timeout):
        self.method = method
        self.label = label
        self.url = url
        self.headers = headers
        self.body = body
        self.timeout = timeout
        self.streamed = hasattr(body, 'read')

    @property
    def path(self):
        return urllib.parse.urlsplit(self.url).path

    @property
    def query(self):
        return urllib.parse.parse_qs(urllib.parse.urlsplit(self.url).query)

    def json(self):
        return json.loads(self.body.decode('utf-8'))


class FakeTransport:
    """Answers the uploader from a script and records every request it makes."""

    def __init__(self, *responses):
        self.responses = list(responses)
        self.calls = []

    def __call__(self, method, label, url, headers, body, timeout):
        self.calls.append(Call(method, label, url, dict(headers), body, timeout))
        if not self.responses:
            raise AssertionError(f'unexpected request: {method} {label}')
        response = self.responses.pop(0)
        if isinstance(response, BaseException):
            raise response
        return response


class UploadContractTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='rustdesk-agc-upload-test-')
        self.addCleanup(self.directory.cleanup)
        self.app = self.app_pack()

    def app_pack(self, bundle=UPLOAD.BUNDLE_NAME, code=67, name=APP_FILE):
        path = Path(self.directory.name) / name
        with zipfile.ZipFile(path, 'w') as archive:
            archive.writestr('pack.info', json.dumps({
                'summary': {'app': {'bundleName': bundle,
                                    'version': {'code': code, 'name': '1.4.9'}}},
            }))
            archive.writestr('entry.hap', b'payload')
        return path

    def patch(self, *responses):
        original = UPLOAD._https
        transport = FakeTransport(*responses)
        UPLOAD._https = transport
        self.addCleanup(setattr, UPLOAD, '_https', original)
        return transport

    def run_main(self, app, environment=None):
        variables = {key: value for key, value in os.environ.items()
                     if key not in (UPLOAD.CLIENT_ID_ENV, UPLOAD.CLIENT_SECRET_ENV,
                                    UPLOAD.APP_ID_ENV)}
        variables.update(environment or {'AGC_CLIENT_ID': 'client-id',
                                         'AGC_CLIENT_SECRET': CLIENT_SECRET,
                                         'AGC_APP_ID': UPLOAD.APP_ID})
        stdout = io.StringIO()
        stderr = io.StringIO()
        with mock.patch.dict(os.environ, variables, clear=True), \
                contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            code = UPLOAD.main([str(app)])
        return code, stdout.getvalue(), stderr.getvalue()

    def api_calls(self, transport):
        return [call for call in transport.calls if API_HOST in call.url]

    def test_uploads_and_registers_the_app_without_publishing(self):
        transport = self.patch(token_response(), upload_url_response(), (200, b''),
                               registered_response(), package_response())
        report = UPLOAD.upload_app(self.app, 'client-id', CLIENT_SECRET, UPLOAD.APP_ID, 67)

        self.assertEqual([call.method for call in transport.calls],
                         ['POST', 'GET', 'PUT', 'PUT', 'GET'])
        self.assertEqual([call.path for call in self.api_calls(transport)],
                         list(ALLOWED_API_PATHS))

        upload = transport.calls[2]
        self.assertEqual(upload.url, UPLOAD_URL)
        self.assertTrue(upload.streamed, 'the App Pack must be streamed, not buffered')
        self.assertEqual(upload.headers['Content-Length'], str(self.app.stat().st_size))
        self.assertEqual(upload.headers['Content-Type'], 'application/octet-stream')

        registration = transport.calls[3]
        self.assertEqual(registration.json(),
                         {'fileName': APP_FILE, 'objectId': OBJECT_ID})
        self.assertEqual(transport.calls[4].query['pkgIds'], [PACKAGE_ID])
        self.assertEqual(transport.calls[4].query['appId'], [UPLOAD.APP_ID])

        self.assertTrue(any(OBJECT_ID in line for line in report))
        self.assertTrue(any('nothing was submitted for review' in line for line in report))

    def test_upload_url_request_carries_the_app_identity(self):
        transport = self.patch(token_response(), upload_url_response(), (200, b''),
                               registered_response(), package_response())
        UPLOAD.upload_app(self.app, 'client-id', CLIENT_SECRET, UPLOAD.APP_ID, 67)

        query = transport.calls[1].query
        self.assertEqual(query['appId'], [UPLOAD.APP_ID])
        self.assertEqual(query['fileName'], [APP_FILE])
        self.assertEqual(query['contentLength'], [str(self.app.stat().st_size)])
        self.assertEqual(query['sha256'], [UPLOAD._sha256(self.app)])
        self.assertEqual(transport.calls[1].headers['client_id'], 'client-id')
        self.assertEqual(transport.calls[1].headers['Authorization'], f'Bearer {TOKEN}')

    def test_signed_upload_headers_are_forwarded_whatever_the_shape(self):
        for headers in ({'x-obs-acl': 'private'},
                        [{'key': 'x-obs-acl', 'value': 'private'}]):
            with self.subTest(headers=headers):
                transport = self.patch(token_response(), upload_url_response(headers=headers),
                                       (200, b''), registered_response(), package_response())
                UPLOAD.upload_app(self.app, 'client-id', CLIENT_SECRET, UPLOAD.APP_ID, 67)
                self.assertEqual(transport.calls[2].headers['x-obs-acl'], 'private')
                self.assertEqual(transport.calls[2].headers['Content-Length'],
                                 str(self.app.stat().st_size))

    def test_malformed_upload_headers_are_refused(self):
        self.assertEqual(UPLOAD._upload_headers(None), ())
        for raw in ('nope', {'x-obs-acl': 1}, [{'value': 'private'}], [{'key': ''}], [None]):
            with self.subTest(raw=raw):
                with self.assertRaises(UPLOAD.UploadError):
                    UPLOAD._upload_headers(raw)

    def test_never_touches_a_publish_submit_or_testing_endpoint(self):
        transport = self.patch(token_response(), upload_url_response(), (200, b''),
                               registered_response(), package_response())
        code, _, _ = self.run_main(self.app)

        self.assertEqual(code, 0)
        self.assertEqual({call.path for call in self.api_calls(transport)},
                         set(ALLOWED_API_PATHS))
        for call in transport.calls:
            lowered = urllib.parse.urlsplit(call.url).path.lower()
            for marker in FORBIDDEN_MARKERS:
                self.assertNotIn(marker, lowered, f'{call.method} {call.path} looks like {marker}')

    def test_credentials_and_upload_url_are_never_printed(self):
        self.patch(token_response(), upload_url_response(), (200, b''),
                   registered_response(), package_response())
        code, stdout, stderr = self.run_main(self.app)

        self.assertEqual(code, 0)
        for secret in (TOKEN, CLIENT_SECRET, UPLOAD_URL, 'signature=test-signature'):
            self.assertNotIn(secret, stdout)
            self.assertNotIn(secret, stderr)

    def test_identity_mismatch_fails_before_any_request(self):
        foreign = self.app_pack(bundle='top.frankhan.resk')
        transport = self.patch()
        code, _, stderr = self.run_main(foreign)

        self.assertEqual(code, 1)
        self.assertEqual(transport.calls, [])
        self.assertIn('top.frankhan.resk', stderr)

    def test_missing_credentials_fail_before_any_request(self):
        transport = self.patch()
        code, _, stderr = self.run_main(self.app, {'AGC_APP_ID': UPLOAD.APP_ID})

        self.assertEqual(code, 2)
        self.assertEqual(transport.calls, [])
        self.assertIn('AGC_CLIENT_ID', stderr)

    def test_superseded_and_foreign_app_ids_are_refused(self):
        for app_id in ('6917605780518421882', '12345', ''):
            with self.subTest(app_id=app_id):
                transport = self.patch()
                code, _, stderr = self.run_main(
                    self.app, {'AGC_CLIENT_ID': 'client-id',
                               'AGC_CLIENT_SECRET': CLIENT_SECRET, 'AGC_APP_ID': app_id})
                self.assertEqual(code, 2)
                self.assertEqual(transport.calls, [])
                self.assertIn('AGC_APP_ID', stderr)

    def test_superseded_app_id_is_named(self):
        with self.assertRaisesRegex(UPLOAD.SetupError, 'superseded'):
            UPLOAD._target_app_id('6917605780518421882')
        with self.assertRaisesRegex(UPLOAD.SetupError, 'must be'):
            UPLOAD._target_app_id('9999')

    def test_token_without_access_token_stops_the_flow(self):
        transport = self.patch(json_response({'ret': {'code': '1101', 'msg': 'bad client'}}))
        code, _, stderr = self.run_main(self.app)

        self.assertEqual(code, 1)
        self.assertEqual(len(transport.calls), 1)
        self.assertIn('access token', stderr)
        self.assertIn('1101', stderr)

    def test_rejected_upload_url_stops_the_flow(self):
        transport = self.patch(token_response(),
                               json_response({'ret': {'code': '1102', 'msg': 'app not found'}}))
        code, _, stderr = self.run_main(self.app)

        self.assertEqual(code, 1)
        self.assertEqual(len(transport.calls), 2)
        self.assertIn('1102', stderr)
        self.assertNotIn('app not found', stderr)

    def test_failed_upload_never_registers_the_app_file(self):
        transport = self.patch(token_response(), upload_url_response(),
                               UPLOAD.UploadError('the upload answered with HTTP 403'))
        code, _, stderr = self.run_main(self.app)

        self.assertEqual(code, 1)
        self.assertEqual([call.path for call in self.api_calls(transport)],
                         list(ALLOWED_API_PATHS[:2]))
        self.assertIn('HTTP 403', stderr)

    def test_rejected_registration_surfaces_the_ret_code(self):
        transport = self.patch(token_response(), upload_url_response(), (200, b''),
                               json_response({'ret': {'code': '1103', 'msg': 'invalid file'}}))
        code, _, stderr = self.run_main(self.app)

        self.assertEqual(code, 1)
        self.assertEqual(len(transport.calls), 4)
        self.assertIn('1103', stderr)

    def test_insecure_upload_url_is_refused(self):
        transport = self.patch(token_response(),
                               upload_url_response(url=UPLOAD_URL.replace('https://', 'http://')))
        code, _, stderr = self.run_main(self.app)

        self.assertEqual(code, 1)
        self.assertEqual(len(transport.calls), 2)
        self.assertIn('HTTPS', stderr)

    def test_malformed_responses_fail_closed(self):
        for payload in ({'ret': {'code': '0'}}, {'urlInfo': 'nope'},
                        {'urlInfo': {'url': UPLOAD_URL, 'objectId': ''}},
                        {'urlInfo': {'url': UPLOAD_URL, 'objectId': OBJECT_ID,
                                     'headers': 'nope'}},
                        {'urlInfo': {'url': UPLOAD_URL, 'objectId': OBJECT_ID,
                                     'method': 'DELETE'}}):
            with self.subTest(payload=payload):
                self.patch(token_response(), json_response(payload))
                code, _, _ = self.run_main(self.app)
                self.assertEqual(code, 1)

    def test_missing_ret_section_is_a_failure(self):
        with self.assertRaises(UPLOAD.UploadError):
            UPLOAD._require_success({}, 'upload URL request')
        with self.assertRaisesRegex(UPLOAD.UploadError, '1101'):
            UPLOAD._require_success({'ret': {'code': '1101', 'msg': 'denied'}}, 'x')

    def test_identity_check_reads_the_pack_version(self):
        self.assertEqual(UPLOAD._check_app_identity(self.app), 67)
        broken = Path(self.directory.name) / 'broken.app'
        broken.write_bytes(b'not a zip')
        with self.assertRaises(UPLOAD.UploadError):
            UPLOAD._check_app_identity(broken)

    def test_polling_waits_only_for_the_registered_package(self):
        transport = self.patch(token_response(), upload_url_response(), (200, b''),
                               registered_response(), package_response(1), package_response())
        with mock.patch.object(UPLOAD.time, 'sleep') as sleep:
            code, _, _ = self.run_main(self.app)
        self.assertEqual(code, 0)
        sleep.assert_called_once_with(UPLOAD.POLL_INTERVAL_SECONDS)
        self.assertEqual(len(transport.calls), 6)

    def test_failed_or_mismatched_package_is_not_success(self):
        for response in (package_response(2), package_response(0, 'different-package'),
                         package_response(None), package_response(True)):
            with self.subTest(response=response):
                self.patch(token_response(), upload_url_response(), (200, b''),
                           registered_response(), response)
                code, stdout, _ = self.run_main(self.app)
                self.assertEqual(code, 1)
                self.assertNotIn('uploaded and parsed', stdout)

    def test_package_timeout_does_not_repeat_upload(self):
        transport = self.patch(token_response(), upload_url_response(), (200, b''),
                               registered_response(), package_response(1), package_response(1))
        with mock.patch.object(UPLOAD, 'POLL_ATTEMPTS', 2), \
                mock.patch.object(UPLOAD.time, 'sleep'):
            code, _, stderr = self.run_main(self.app)
        self.assertEqual(code, 1)
        self.assertIn('still parsing', stderr)
        self.assertEqual(sum(call.streamed for call in transport.calls), 1)

    def test_untrusted_error_message_cannot_echo_credentials(self):
        self.patch(json_response({'ret': {'code': CLIENT_SECRET, 'msg': TOKEN + UPLOAD_URL}}))
        code, stdout, stderr = self.run_main(self.app)
        self.assertEqual(code, 1)
        for secret in (CLIENT_SECRET, TOKEN, UPLOAD_URL):
            self.assertNotIn(secret, stdout + stderr)

    def test_registration_without_id_does_not_claim_success(self):
        self.patch(token_response(), upload_url_response(), (200, b''),
                   json_response({'ret': {'code': 0}}))
        code, stdout, stderr = self.run_main(self.app)
        self.assertEqual(code, 1)
        self.assertIn('no packageId', stderr)
        self.assertNotIn('uploaded and parsed', stdout)


if __name__ == '__main__':
    unittest.main()
