#!/usr/bin/env python3
"""Verify a signed HarmonyOS release App Pack built for the Flutter RustDesk client.

Exit codes: 0 verified, 1 verification failed, 2 usage or environment problem.
"""

import argparse
import base64
import binascii
import contextlib
import json
import os
from pathlib import Path
import posixpath
import re
import shutil
import subprocess
import sys
import tempfile
import time
from typing import NamedTuple
import zipfile
import zlib


BUNDLE_NAME = 'top.frankhan.resk.flutter'
NATIVE_ABI = 'arm64-v8a'
AOT_PAYLOAD = 'libapp.so'
REQUIRED_NATIVE_LIBS = ('liblibrustdesk.so', 'libc++_shared.so', 'libflutter.so', AOT_PAYLOAD)
AOT_SNAPSHOT_MARKERS = (b'_kDartIsolateSnapshotInstructions', b'_kDartVmSnapshotData')
MIN_NATIVE_LIB_BYTES = 4096
MODULE_SUFFIXES = ('.hap', '.hsp')
VERIFY_TIMEOUT_SECONDS = 900
CHUNK_BYTES = 1024 * 1024
TOOL_DETAIL_LINES = 4

CERTIFICATE_BLOCK = re.compile(rb'-----BEGIN CERTIFICATE-----(.*?)-----END CERTIFICATE-----', re.DOTALL)
BASE64_LINE = re.compile(r'^[A-Za-z0-9+/]{60,}={0,2}$')
ELF_MACHINE_AARCH64 = 183


class VerificationError(Exception):
    """A release artifact could not be proven genuine."""


class SetupError(Exception):
    """The verifier itself is missing an input or a required tool."""


class ProfileClaims(NamedTuple):
    kind: str
    distribution: str
    bundle: str
    certificate: bytes


class Container(NamedTuple):
    modules: list
    extras: list


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(
        description=__doc__.strip().splitlines()[0],
        epilog='The APP signature and embedded release profile are verified with the SDK '
               'signing tool. Module metadata and native payloads are read only from that '
               'authenticated container. Nothing is written outside the temporary directory.',
    )
    parser.add_argument('app', type=Path, help='signed release App Pack (.app) to verify')
    parser.add_argument('--profile', type=Path, required=True,
                        help='release Provision Profile (.p7b) the APP must be signed with')
    parser.add_argument('--sign-tool', type=Path, required=True, dest='sign_tool',
                        help='hap-sign-tool.jar from the HarmonyOS SDK')
    parser.add_argument('--java', help='java executable (defaults to JAVA_HOME, then PATH)')
    args = parser.parse_args(argv)

    try:
        java = _resolve_java(args.java)
        jar = _existing_file(args.sign_tool, 'hap-sign-tool.jar')
        profile = _existing_file(args.profile, 'release profile')
        app = _existing_file(args.app, 'release APP')
    except SetupError as error:
        print(f'error: {error}', file=sys.stderr)
        return 2

    try:
        report = verify_release(app, profile, java, jar)
    except VerificationError as error:
        print(f'error: {error}', file=sys.stderr)
        return 1

    for line in report:
        print(line)
    return 0


def verify_release(app: Path, profile: Path, java: str, jar: Path) -> list:
    """Verify the release App Pack and return the safe summary lines."""
    profile_bytes = _read_bytes(profile, 'release profile')
    workspace = Path(tempfile.mkdtemp(prefix='rustdesk-ohos-verify-'))
    try:
        claims = _verify_profile(java, jar, profile, workspace)
        chain_index = _verify_signature(java, jar, app, app.name, 0, profile_bytes, claims, workspace)
        if not zipfile.is_zipfile(app):
            raise VerificationError(f'{app.name} is not a valid App Pack container')
        with _open_archive(app, app.name) as pack:
            container = _container_entries(pack)
            lines = [
                f'profile: type={claims.kind} distribution={claims.distribution} bundle={claims.bundle}',
                f'{app.name}: App Pack signature OK, release profile matched, signed with the profile '
                f'distribution certificate (chain position {chain_index}); {len(container.modules)} module(s)',
            ]
            applications = []
            payloads = 0
            for index, label in enumerate(container.modules):
                hap = _extract(pack, label, index, workspace)
                document = _module_document(hap, label)
                application = _check_bundle(document, label)
                _check_release_semantics(application, label)
                applications.append((label, application))
                lines.append(f'{label}: release metadata verified inside the authenticated App Pack')
                if document['module'].get('type') == 'entry':
                    _check_native_payload(hap, label)
                    payloads += 1
                    lines.append(f'{label}: native {NATIVE_ABI} payload verified '
                                 + ', '.join(f'libs/{NATIVE_ABI}/{library}' for library in REQUIRED_NATIVE_LIBS))
            if not payloads:
                raise VerificationError('the APP has no verified entry module carrying the native payload')
            version = _check_pack_info(pack, applications)
            lines.append(f'pack.info: bundle={BUNDLE_NAME}'
                         + (f' versionCode={version}' if version is not None else ''))
        return lines
    finally:
        _remove(workspace)


def _verify_profile(java: str, jar: Path, profile: Path, workspace: Path) -> ProfileClaims:
    result_path = workspace / 'profile-verify.json'
    completed = _run_tool(java, jar, 'verify-profile',
                          ('-inFile', profile, '-outFile', result_path), workspace)
    if completed.returncode != 0:
        raise _tool_failure('verify-profile', profile.name, completed)
    profile_data = _find_profile(_read_json(result_path, 'verified profile content'))

    kind = _require_text(profile_data, 'type', 'profile type')
    if kind != 'release':
        raise VerificationError(f'the supplied profile type is {kind!r}, expected release')
    distribution = _require_text(profile_data, 'app-distribution-type', 'profile distribution type')
    if distribution != 'app_gallery':
        raise VerificationError(f'the supplied profile distributes through {distribution!r}, expected app_gallery')
    if profile_data.get('debug-info'):
        raise VerificationError('the supplied profile is a debug profile')
    bundle_info = profile_data.get('bundle-info')
    if not isinstance(bundle_info, dict):
        raise VerificationError('the supplied profile has no bundle-info section')
    bundle = _require_text(bundle_info, 'bundle-name', 'profile bundle name')
    if bundle != BUNDLE_NAME:
        raise VerificationError(f'the supplied profile is for bundle {bundle!r}, expected {BUNDLE_NAME!r}')
    certificate = _profile_certificate(_require_text(bundle_info, 'distribution-certificate',
                                                     'profile distribution certificate'))
    _check_profile_expiry(profile_data)
    return ProfileClaims(kind, distribution, bundle, certificate)


def _verify_signature(java: str, jar: Path, hap: Path, label: str, index: int,
                      profile_bytes: bytes, claims: ProfileClaims, workspace: Path) -> int:
    stamp = f'{index:02d}-{posixpath.basename(label)}'
    chain_path = workspace / f'{stamp}.cer'
    profile_path = workspace / f'{stamp}.p7b'
    completed = _run_tool(java, jar, 'verify-app',
                          ('-inFile', hap, '-outCertChain', chain_path, '-outProfile', profile_path),
                          workspace)
    if completed.returncode != 0:
        raise _tool_failure('verify-app', label, completed)
    if _read_bytes(profile_path, f'profile embedded in {label}') != profile_bytes:
        raise VerificationError(f'{label}: the profile embedded in the module is not the supplied release profile')
    chain = _pem_certificates(_read_bytes(chain_path, f'certificate chain of {label}'))
    if claims.certificate not in chain:
        raise VerificationError(f'{label}: the module is not signed with the distribution certificate '
                                'of the supplied profile')
    return chain.index(claims.certificate)


def _check_pack_info(pack: zipfile.ZipFile, applications: list):
    try:
        raw = pack.read('pack.info')
    except (KeyError, zipfile.BadZipFile, OSError) as error:
        raise VerificationError(f'cannot read pack.info from the APP: {error}')
    try:
        pack_info = json.loads(raw.decode('utf-8-sig'))
    except (UnicodeDecodeError, ValueError) as error:
        raise VerificationError(f'pack.info is not valid JSON: {error}')
    summary = pack_info.get('summary') if isinstance(pack_info, dict) else None
    entry = summary.get('app') if isinstance(summary, dict) else None
    if not isinstance(entry, dict):
        raise VerificationError('pack.info has no app summary')
    bundle = entry.get('bundleName')
    if bundle != BUNDLE_NAME:
        raise VerificationError(f'pack.info describes bundle {bundle!r}, expected {BUNDLE_NAME!r}')
    bundle_kind = entry.get('bundleType')
    if isinstance(bundle_kind, str) and bundle_kind != 'app':
        raise VerificationError(f'pack.info describes a {bundle_kind!r} package, expected an app')
    version = entry.get('version')
    code = version.get('code') if isinstance(version, dict) else None
    if not isinstance(code, int) or isinstance(code, bool):
        return None
    for label, application in applications:
        if application.get('versionCode') != code:
            raise VerificationError(f'pack.info versionCode {code} does not match {label} versionCode '
                                    f'{application.get("versionCode")!r}')
    return code


def _container_entries(pack: zipfile.ZipFile) -> Container:
    modules = []
    extras = []
    seen = set()
    for info in pack.infolist():
        if info.is_dir():
            continue
        name = info.filename
        if info.flag_bits & 0x1:
            raise VerificationError(f'APP entry {name} is encrypted')
        if name.startswith('/') or '\\' in name or posixpath.normpath(name) != name:
            raise VerificationError(f'APP contains an unsafe entry path: {name}')
        if name in seen:
            raise VerificationError(f'APP contains a duplicate entry: {name}')
        seen.add(name)
        if name.endswith(MODULE_SUFFIXES):
            modules.append(name)
        elif name != 'pack.info':
            extras.append(name)
    if 'pack.info' not in seen:
        raise VerificationError('APP does not contain pack.info')
    if not modules:
        raise VerificationError('APP does not contain any HAP or HSP module')
    if not any(name.endswith('.hap') for name in modules):
        raise VerificationError('APP does not contain any HAP module')
    return Container(modules, extras)


def _extract(pack: zipfile.ZipFile, name: str, index: int, workspace: Path) -> Path:
    target = workspace / f'{index:02d}-{posixpath.basename(name)}'
    try:
        info = pack.getinfo(name)
        with pack.open(info) as source, target.open('wb') as destination:
            checksum = 0
            written = 0
            for chunk in iter(lambda: source.read(CHUNK_BYTES), b''):
                checksum = zlib.crc32(chunk, checksum)
                written += len(chunk)
                destination.write(chunk)
    except (KeyError, zipfile.BadZipFile, OSError) as error:
        raise VerificationError(f'cannot extract {name} from the APP: {error}')
    if written != info.file_size or checksum != info.CRC:
        raise VerificationError(f'{name} is corrupt inside the APP')
    if not written:
        raise VerificationError(f'{name} is empty inside the APP')
    return target


def _module_document(hap: Path, label: str) -> dict:
    try:
        with _open_archive(hap, label) as archive:
            raw = archive.read('module.json')
    except KeyError:
        raise VerificationError(f'{label} does not contain module.json')
    try:
        document = json.loads(raw.decode('utf-8-sig'))
    except (UnicodeDecodeError, ValueError) as error:
        raise VerificationError(f'{label} has an unreadable module.json: {error}')
    if not isinstance(document, dict) or not isinstance(document.get('app'), dict) \
            or not isinstance(document.get('module'), dict):
        raise VerificationError(f'{label} has an unexpected module.json structure')
    return document


def _check_bundle(document: dict, label: str) -> dict:
    application = document['app']
    bundle = application.get('bundleName')
    if bundle != BUNDLE_NAME:
        raise VerificationError(f'{label} belongs to bundle {bundle!r}, expected {BUNDLE_NAME!r}')
    return application


def _check_release_semantics(application: dict, label: str) -> None:
    debug = application.get('debug')
    if debug is not False:
        raise VerificationError(f'{label} does not explicitly disable debugging (debug={debug!r})')
    mode = application.get('buildMode')
    if mode != 'release':
        raise VerificationError(f'{label} was built with buildMode={mode!r}, expected release')
    version = application.get('versionCode')
    if not isinstance(version, int) or isinstance(version, bool) or version <= 0:
        raise VerificationError(f'{label} has no valid versionCode')


def _check_native_payload(hap: Path, label: str) -> None:
    with _open_archive(hap, label) as archive:
        members = set(archive.namelist())
        missing = [library for library in REQUIRED_NATIVE_LIBS
                   if f'libs/{NATIVE_ABI}/{library}' not in members]
        if missing:
            raise VerificationError(f'{label} is missing native {NATIVE_ABI} libraries: {", ".join(missing)}')
        for library in REQUIRED_NATIVE_LIBS:
            name = f'libs/{NATIVE_ABI}/{library}'
            info = archive.getinfo(name)
            if info.file_size < MIN_NATIVE_LIB_BYTES:
                raise VerificationError(f'{label}: {name} holds only {info.file_size} bytes')
            with archive.open(info) as stream:
                if not _is_arm64_elf(stream.read(20)):
                    raise VerificationError(f'{label}: {name} is not an arm64 ELF shared object')
        payload = archive.getinfo(f'libs/{NATIVE_ABI}/{AOT_PAYLOAD}')
        absent = _absent_aot_markers(archive, payload)
        if absent:
            raise VerificationError(f'{label}: libs/{NATIVE_ABI}/{AOT_PAYLOAD} carries no Dart AOT snapshot '
                                    f'(missing {", ".join(marker.decode() for marker in absent)})')


def _is_arm64_elf(header: bytes) -> bool:
    return (len(header) >= 20 and header[:4] == b'\x7fELF' and header[4] == 2
            and header[5] == 1 and header[6] == 1
            and int.from_bytes(header[18:20], 'little') == ELF_MACHINE_AARCH64)


def _absent_aot_markers(archive: zipfile.ZipFile, info: zipfile.ZipInfo) -> list:
    pending = list(AOT_SNAPSHOT_MARKERS)
    with archive.open(info) as stream:
        tail = b''
        while pending:
            chunk = stream.read(CHUNK_BYTES)
            if not chunk:
                break
            window = tail + chunk
            pending = [marker for marker in pending if marker not in window]
            tail = window[-64:]
    return pending


def _check_profile_expiry(profile_data: dict) -> None:
    validity = profile_data.get('validity')
    not_after = validity.get('not-after') if isinstance(validity, dict) else None
    if isinstance(not_after, int) and not isinstance(not_after, bool) and not_after < time.time():
        raise VerificationError('the supplied release profile expired at '
                                + time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(not_after)))


def _find_profile(data) -> dict:
    """Locate the provision profile inside whatever the signing tool reported."""
    found = []
    pending = [data]
    budget = 10000
    while pending and budget > 0:
        budget -= 1
        value = pending.pop()
        if isinstance(value, dict):
            found.append(value)
            pending.extend(value.values())
        elif isinstance(value, list):
            pending.extend(value)
        elif isinstance(value, str) and value.lstrip().startswith('{'):
            try:
                pending.append(json.loads(value))
            except ValueError:
                continue
    for required in (('bundle-info', 'type'), ('type', 'app-distribution-type')):
        for candidate in found:
            if all(key in candidate for key in required):
                return candidate
    raise VerificationError('the signing tool reported no provision profile')


def _require_text(section: dict, key: str, description: str) -> str:
    value = section.get(key)
    if not isinstance(value, str) or not value.strip():
        raise VerificationError(f'the supplied profile declares no {description}')
    return value.strip()


def _profile_certificate(value: str) -> bytes:
    block = CERTIFICATE_BLOCK.search(value.encode())
    return _certificate_der(block.group(1) if block else value.encode())


def _pem_certificates(raw: bytes) -> list:
    certificates = [_certificate_der(block) for block in CERTIFICATE_BLOCK.findall(raw)]
    if not certificates:
        raise VerificationError('the signing tool reported no certificate chain')
    return certificates


def _certificate_der(body: bytes) -> bytes:
    try:
        der = base64.b64decode(re.sub(rb'\s+', b'', body), validate=True)
    except (ValueError, binascii.Error) as error:
        raise VerificationError(f'cannot decode a certificate: {error}')
    if not der:
        raise VerificationError('a certificate is empty')
    return der


def _run_tool(java: str, jar: Path, command: str, arguments: tuple, workspace: Path):
    argv = [java, '-jar', str(jar), command] + [str(argument) for argument in arguments]
    try:
        return subprocess.run(argv, cwd=str(workspace), stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT, timeout=VERIFY_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        raise VerificationError(f'hap-sign-tool {command} timed out after {VERIFY_TIMEOUT_SECONDS}s')
    except OSError as error:
        raise VerificationError(f'cannot run hap-sign-tool {command}: {error}')


def _tool_failure(command: str, label: str, completed) -> VerificationError:
    detail = _tool_detail(completed.stdout or b'')
    return VerificationError(f'hap-sign-tool {command} rejected {label} (exit {completed.returncode})'
                             + (f': {detail}' if detail else ''))


def _tool_detail(output: bytes) -> str:
    lines = []
    for line in output.decode('utf-8', 'replace').splitlines():
        line = line.strip()[:200]
        if not line or '-----BEGIN' in line or '-----END' in line or BASE64_LINE.match(line):
            continue
        lines.append(line)
    return '; '.join(lines[-TOOL_DETAIL_LINES:])


def _read_json(path: Path, description: str):
    raw = _read_bytes(path, description)
    try:
        return json.loads(raw.decode('utf-8-sig'))
    except (UnicodeDecodeError, ValueError) as error:
        raise VerificationError(f'{description} is not valid JSON: {error}')


def _read_bytes(path, description: str) -> bytes:
    try:
        data = Path(path).read_bytes()
    except OSError as error:
        raise VerificationError(f'cannot read {description}: {error}')
    if not data:
        raise VerificationError(f'{description} is empty')
    return data


@contextlib.contextmanager
def _open_archive(path, label: str):
    try:
        archive = zipfile.ZipFile(path)
    except (zipfile.BadZipFile, OSError) as error:
        raise VerificationError(f'cannot read {label}: {error}')
    try:
        yield archive
    finally:
        archive.close()


def _resolve_java(explicit) -> str:
    if explicit:
        java = shutil.which(explicit)
        if java is None:
            raise SetupError(f'java executable not found: {explicit}')
        return java
    candidates = []
    java_home = os.environ.get('JAVA_HOME')
    if java_home:
        candidates.append(os.path.join(java_home, 'bin', 'java'))
    discovered = shutil.which('java')
    if discovered:
        candidates.append(discovered)
    for candidate in candidates:
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    raise SetupError('java is not available; install a JDK or pass --java')


def _existing_file(path: Path, description: str) -> Path:
    if not path.is_file():
        raise SetupError(f'{description} not found: {path}')
    try:
        size = path.stat().st_size
    except OSError as error:
        raise SetupError(f'cannot inspect {description}: {error}')
    if not size:
        raise SetupError(f'{description} is empty: {path}')
    return path.resolve()


def _remove(workspace: Path) -> None:
    shutil.rmtree(workspace, ignore_errors=True)
    if os.path.exists(workspace):
        print(f'warning: could not remove verifier workspace {workspace}', file=sys.stderr)


if __name__ == '__main__':
    sys.exit(main())
