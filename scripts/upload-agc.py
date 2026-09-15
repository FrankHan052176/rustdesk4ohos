#!/usr/bin/env python3
"""Upload a verified signed Flutter HarmonyOS APP without publishing it.

Uses API Client authentication, OBS upload, app-package-info registration and
package compilation status only. No review or invitation-test API is called.

Official contracts:
https://developer.huawei.com/consumer/cn/doc/app/agc-help-upload-api-upload-url-0000002236201294
https://developer.huawei.com/consumer/cn/doc/app/agc-help-upload-api-upload-file-0000002271160621
https://developer.huawei.com/consumer/cn/doc/app/agc-help-publish-api-app-package-info-update-0000002236201250
https://developer.huawei.com/consumer/cn/doc/app/agc-help-publish-api-query-compile-status-0000002236041434

Exit codes: 0 uploaded and parsed, 1 upload failed, 2 usage/environment problem.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import sys
import time
from typing import NamedTuple
import urllib.error
import urllib.parse
import urllib.request
import zipfile


BUNDLE_NAME = 'top.frankhan.resk.flutter'
APP_ID = '6917615823381371195'
SUPERSEDED_APP_IDS = ('6917605780518421882',)
API_BASE = 'https://connect-api.cloud.huawei.com/api'
TOKEN_ENDPOINT = f'{API_BASE}/oauth2/v1/token'
UPLOAD_URL_ENDPOINT = f'{API_BASE}/publish/v2/upload-url/for-obs'
PACKAGE_ENDPOINT = f'{API_BASE}/publish/v3/app-package-info'
PACKAGE_STATUS_ENDPOINT = f'{API_BASE}/publish/v3/package/compile/status'
CLIENT_ID_ENV = 'AGC_CLIENT_ID'
CLIENT_SECRET_ENV = 'AGC_CLIENT_SECRET'
APP_ID_ENV = 'AGC_APP_ID'
API_TIMEOUT_SECONDS = 60
UPLOAD_TIMEOUT_SECONDS = 3600
HASH_CHUNK_BYTES = 1024 * 1024
POLL_ATTEMPTS = 30
POLL_INTERVAL_SECONDS = 20
UPLOAD_METHODS = ('PUT',)


class UploadError(Exception):
    """The signed App Pack could not be uploaded."""


class SetupError(Exception):
    """The uploader is missing an input or an environment credential."""


class UploadTarget(NamedTuple):
    url: str
    method: str
    object_id: str
    headers: tuple


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """Refuse redirects so the App Pack only ever reaches the returned upload URL."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise UploadError(f'the AppGallery Connect API answered with an unexpected '
                          f'redirect (HTTP {code})')


_OPENER = urllib.request.build_opener(_NoRedirect)


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__.strip().splitlines()[0],
        epilog='The App Pack identity is checked before any request, TLS verification is '
               'always required, and no credential, access token or upload URL is printed.',
    )
    parser.add_argument('app', type=Path, help='signed release App Pack (.app) to upload')
    args = parser.parse_args(argv)

    try:
        app = _existing_file(args.app, 'release APP')
        client_id = _required_env(CLIENT_ID_ENV)
        client_secret = _required_env(CLIENT_SECRET_ENV)
        app_id = _target_app_id(_required_env(APP_ID_ENV))
        version = _check_app_identity(app)
    except SetupError as error:
        print(f'error: {error}', file=sys.stderr)
        return 2
    except UploadError as error:
        print(f'error: {error}', file=sys.stderr)
        return 1

    try:
        report = upload_app(app, client_id, client_secret, app_id, version)
    except UploadError as error:
        print(f'error: {error}', file=sys.stderr)
        return 1

    for line in report:
        print(line)
    return 0


def upload_app(app: Path, client_id: str, client_secret: str, app_id: str, version) -> list:
    """Upload and register the App Pack, then verify server-side parsing."""
    size = app.stat().st_size
    digest = _sha256(app)
    token = _obtain_token(client_id, client_secret)
    target = _request_upload_target(token, client_id, app_id, app.name, size, digest)
    _upload_object(target, app, size)
    package_id = _register_package(token, client_id, app_id, app.name, target.object_id)
    _wait_for_package(token, client_id, app_id, package_id)
    return [
        f'appId: {app_id} bundle: {BUNDLE_NAME}'
        + (f' versionCode: {version}' if version is not None else ''),
        f'{app.name}: {size} bytes sha256={digest} uploaded and parsed '
        f'(packageId {package_id}, objectId {target.object_id})',
        'no app version was created and nothing was submitted for review',
    ]


def _obtain_token(client_id: str, client_secret: str) -> str:
    # The OAuth2 token response carries no ``ret`` section, so the access token
    # itself decides success and ``ret`` is only read to explain a rejection.
    payload = _api('POST', 'token request', TOKEN_ENDPOINT,
                   {'Content-Type': 'application/json'},
                   {'grant_type': 'client_credentials',
                    'client_id': client_id,
                    'client_secret': client_secret})
    token = payload.get('access_token')
    if not isinstance(token, str) or not token:
        ret = payload.get('ret')
        code = ret.get('code') if isinstance(ret, dict) else None
        raise UploadError(f'the token request returned no access token '
                          f'(ret.code={_safe_code(code)})')
    return token


def _request_upload_target(token: str, client_id: str, app_id: str, name: str, size: int,
                           digest: str) -> UploadTarget:
    query = urllib.parse.urlencode({'appId': app_id, 'fileName': name,
                                    'sha256': digest, 'contentLength': size})
    payload = _api('GET', 'upload URL request', f'{UPLOAD_URL_ENDPOINT}?{query}',
                   _authorized(token, client_id))
    _require_success(payload, 'upload URL request')
    info = payload.get('urlInfo')
    if not isinstance(info, dict):
        raise UploadError('the upload URL response has no urlInfo section')
    url = info.get('url')
    if not isinstance(url, str) or not url.startswith('https://'):
        raise UploadError('the upload URL response did not contain an HTTPS upload URL')
    method = info.get('method') or 'PUT'
    if method not in UPLOAD_METHODS:
        raise UploadError(f'the upload URL response requested the unsupported method {method!r}')
    object_id = info.get('objectId')
    if not isinstance(object_id, str) or not object_id:
        raise UploadError('the upload URL response has no objectId')
    return UploadTarget(url, method, object_id, _upload_headers(info.get('headers')))


def _upload_headers(raw) -> tuple:
    """Return the signed upload headers as pairs, accepting either documented shape."""
    if raw is None:
        return ()
    if isinstance(raw, dict):
        entries = list(raw.items())
    elif isinstance(raw, list):
        entries = [(entry.get('key'), entry.get('value')) if isinstance(entry, dict)
                   else (None, None) for entry in raw]
    else:
        raise UploadError('the upload URL response has malformed upload headers')
    headers = []
    for key, value in entries:
        if not isinstance(key, str) or not key or not isinstance(value, str):
            raise UploadError('the upload URL response has malformed upload headers')
        headers.append((key, value))
    return tuple(headers)


def _upload_object(target: UploadTarget, app: Path, size: int) -> None:
    headers = dict(target.headers)
    if not any(key.lower() == 'content-length' for key in headers):
        headers['Content-Length'] = str(size)
    with app.open('rb') as stream:
        _https(target.method, 'upload', target.url, headers, stream, UPLOAD_TIMEOUT_SECONDS)


def _register_package(token: str, client_id: str, app_id: str, name: str,
                      object_id: str) -> str:
    query = urllib.parse.urlencode({'appId': app_id})
    payload = _api('PUT', 'package registration', f'{PACKAGE_ENDPOINT}?{query}',
                   _authorized(token, client_id),
                   {'fileName': name, 'objectId': object_id})
    _require_success(payload, 'package registration')
    package_id = payload.get('packageId')
    if not isinstance(package_id, str) or not package_id:
        raise UploadError('package registration returned no packageId; do not replay the upload')
    return package_id


def _wait_for_package(token: str, client_id: str, app_id: str, package_id: str) -> None:
    query = urllib.parse.urlencode({'appId': app_id, 'pkgIds': package_id})
    for attempt in range(POLL_ATTEMPTS):
        payload = _api('GET', 'package status', f'{PACKAGE_STATUS_ENDPOINT}?{query}',
                       _authorized(token, client_id))
        _require_success(payload, 'package status')
        states = payload.get('pkgStateList')
        if not isinstance(states, list):
            raise UploadError('package status returned no pkgStateList')
        matches = [state for state in states if isinstance(state, dict)
                   and state.get('pkgId') == package_id]
        if len(matches) != 1:
            raise UploadError('package status did not identify the registered package')
        status = matches[0].get('successStatus')
        if type(status) is not int or status not in (0, 1, 2):
            raise UploadError('package status returned an unknown successStatus')
        if status == 0:
            return
        if status == 2:
            raise UploadError(f'AGC rejected package {package_id} during parsing')
        if attempt + 1 < POLL_ATTEMPTS:
            time.sleep(POLL_INTERVAL_SECONDS)
    raise UploadError(f'package {package_id} is still parsing; check AGC before retrying')


def _authorized(token: str, client_id: str) -> dict:
    return {'Authorization': f'Bearer {token}', 'client_id': client_id,
            'Content-Type': 'application/json'}


def _api(method: str, label: str, url: str, headers: dict, payload=None) -> dict:
    """Call one JSON AppGallery Connect interface and return the decoded response."""
    body = None if payload is None else json.dumps(payload).encode('utf-8')
    status, raw = _https(method, label, url, headers, body, API_TIMEOUT_SECONDS)
    if not 200 <= status < 300:
        raise UploadError(f'the {label} answered with HTTP {status}')
    try:
        decoded = json.loads(raw.decode('utf-8-sig'))
    except (UnicodeDecodeError, ValueError):
        raise UploadError(f'the {label} response was not valid JSON')
    if not isinstance(decoded, dict):
        raise UploadError(f'the {label} response was not a JSON object')
    return decoded


def _require_success(payload: dict, label: str) -> None:
    ret = payload.get('ret')
    code = ret.get('code') if isinstance(ret, dict) else None
    if str(code) != '0':
        raise UploadError(f'AppGallery Connect rejected the {label} '
                          f'(ret.code={_safe_code(code)})')


def _safe_code(code) -> str:
    value = str(code)
    return value if value.isdecimal() and len(value) <= 16 else 'unknown'


def _https(method: str, label: str, url: str, headers: dict, body, timeout: int):
    """Perform one HTTPS request and return ``(status, body)``.

    Certificate verification is never disabled, error responses are reported by
    status only because their body can carry a signed upload URL, and the URLs
    themselves are never included in a message.
    """
    request = urllib.request.Request(url, data=body, method=method)
    for name, value in headers.items():
        request.add_header(name, value)
    try:
        with _OPENER.open(request, timeout=timeout) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as error:
        error.close()
        raise UploadError(f'the {label} answered with HTTP {error.code}')
    except (urllib.error.URLError, TimeoutError, OSError) as error:
        raise UploadError(f'the {label} failed ({type(error).__name__}); '
                          'check AGC before replaying an upload') from None


def _check_app_identity(app: Path):
    """Return the App Pack versionCode after proving it is the RustDesk Flutter app."""
    if not zipfile.is_zipfile(app):
        raise UploadError(f'{app.name} is not a valid App Pack container')
    with zipfile.ZipFile(app) as pack:
        try:
            raw = pack.read('pack.info')
        except (KeyError, zipfile.BadZipFile, OSError) as error:
            raise UploadError(f'cannot read pack.info from {app.name}: {error}')
    try:
        pack_info = json.loads(raw.decode('utf-8-sig'))
    except (UnicodeDecodeError, ValueError):
        raise UploadError(f'the pack.info of {app.name} is not valid JSON')
    summary = pack_info.get('summary') if isinstance(pack_info, dict) else None
    entry = summary.get('app') if isinstance(summary, dict) else None
    if not isinstance(entry, dict):
        raise UploadError(f'the pack.info of {app.name} has no app summary')
    bundle = entry.get('bundleName')
    if bundle != BUNDLE_NAME:
        raise UploadError(f'{app.name} describes bundle {bundle!r}, expected {BUNDLE_NAME!r}')
    version = entry.get('version')
    code = version.get('code') if isinstance(version, dict) else None
    return code if isinstance(code, int) and not isinstance(code, bool) else None


def _target_app_id(value: str) -> str:
    if value in SUPERSEDED_APP_IDS:
        raise SetupError(f'{APP_ID_ENV} {value} is the superseded RustDesk app ID; '
                         f'{BUNDLE_NAME} uploads to {APP_ID}')
    if value != APP_ID:
        raise SetupError(f'{APP_ID_ENV} must be {APP_ID} for {BUNDLE_NAME}, got {value!r}')
    return value


def _required_env(name: str) -> str:
    value = os.environ.get(name, '').strip()
    if not value:
        raise SetupError(f'{name} is empty; store the AppGallery Connect API client '
                         'in the repository secrets')
    return value


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(HASH_CHUNK_BYTES), b''):
            digest.update(block)
    return digest.hexdigest()


def _existing_file(path: Path, description: str) -> Path:
    if not path.is_file():
        raise SetupError(f'{description} {path} does not exist')
    return path.resolve()


if __name__ == '__main__':
    sys.exit(main())
