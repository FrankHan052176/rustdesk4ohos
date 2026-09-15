#!/usr/bin/env python3
"""Prepare the OHOS release signing material carried by the CI secret.

The repository secret ``OHOS_RELEASE_SIGNING_BASE64`` holds a base64 encoded
tar.gz with the publish signing material: the keystore, the certificate chain,
the ``RustDesk_UO_PublishRelease.p7b`` publish profile and the Hvigor password
encryption auxiliaries. The archive is written to a fresh owner-only directory
and the ``publish`` entry of ``signingConfigs.json`` is repointed at the
extracted files, because the archive may carry paths from the machine that
produced it.

Usage::

    python3 .github/scripts/prepare-ohos-signing.py OUTPUT_DIRECTORY

The secret is read from the environment and is never printed.
"""

from __future__ import annotations

import base64
import binascii
import io
import json
import os
import sys
import tarfile
from pathlib import Path, PurePosixPath

ARCHIVE_ENV = "OHOS_RELEASE_SIGNING_BASE64"
CONFIG_NAME = "signingConfigs.json"
PUBLISH_NAME = "publish"
PUBLISH_PROFILE_NAME = "RustDesk_UO_PublishRelease.p7b"
MATERIAL_PATH_FIELDS = ("storeFile", "certpath", "profile")


class SigningError(Exception):
    """Raised when the signing material cannot be prepared safely."""


def decode_archive(encoded: str) -> bytes:
    compact = "".join(encoded.split())
    if not compact:
        raise SigningError(f"{ARCHIVE_ENV} is empty")
    try:
        archive = base64.b64decode(compact, validate=True)
    except (binascii.Error, ValueError) as error:
        raise SigningError(f"{ARCHIVE_ENV} is not valid base64") from error
    if not archive:
        raise SigningError(f"{ARCHIVE_ENV} decodes to an empty archive")
    return archive


def prepare_directory(directory: Path) -> None:
    if directory.is_symlink():
        raise SigningError(f"{directory} must not be a symlink")
    if directory.exists():
        if not directory.is_dir():
            raise SigningError(f"{directory} exists and is not a directory")
        if any(directory.iterdir()):
            raise SigningError(f"{directory} is not empty")
    else:
        directory.mkdir(parents=True, exist_ok=True)
    os.chmod(directory, 0o700)


def member_parts(name: str) -> list[str]:
    path = PurePosixPath(name)
    if path.is_absolute():
        raise SigningError(f"archive member {name!r} uses an absolute path")
    parts = [part for part in path.parts if part not in ("", ".")]
    if ".." in parts:
        raise SigningError(f"archive member {name!r} escapes the output directory")
    return parts


def extract(archive: bytes, directory: Path) -> None:
    try:
        with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as tar:
            for member in tar:
                parts = member_parts(member.name)
                if not parts:
                    if not member.isdir():
                        raise SigningError(
                            f"archive member {member.name!r} has no path"
                        )
                    continue
                destination = directory.joinpath(*parts)
                if member.isdir():
                    destination.mkdir(parents=True, exist_ok=True)
                    os.chmod(destination, 0o700)
                    continue
                if not member.isfile():
                    raise SigningError(
                        f"archive member {member.name!r} is not a regular file"
                    )
                source = tar.extractfile(member)
                if source is None:
                    raise SigningError(f"archive member {member.name!r} is not readable")
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_bytes(source.read())
                os.chmod(destination, 0o600)
    except tarfile.TarError as error:
        raise SigningError(f"{ARCHIVE_ENV} is not a valid tar archive") from error


def find_single(directory: Path, name: str) -> Path:
    matches = sorted(
        path for path in directory.rglob("*") if path.name == name and path.is_file()
    )
    if not matches:
        raise SigningError(f"{name} is missing from the signing archive")
    if len(matches) > 1:
        raise SigningError(f"{name} appears more than once in the signing archive")
    return matches[0]


def basename(value: str) -> str:
    return PurePosixPath(value.replace("\\", "/")).name


def resolve_material(directory: Path, material: dict, field: str) -> Path:
    value = material.get(field)
    if not isinstance(value, str) or not value.strip():
        raise SigningError(f"{PUBLISH_NAME} configuration has no {field}")
    name = basename(value)
    if not name:
        raise SigningError(f"{PUBLISH_NAME} configuration has an empty {field}")
    resolved = find_single(directory, name)
    if resolved.stat().st_size == 0:
        raise SigningError(f"{PUBLISH_NAME} {field} material {name} is empty")
    return resolved


def validate_passwords(material: dict) -> None:
    for key in ('storePassword', 'keyPassword'):
        value = material.get(key)
        if not isinstance(value, str) or not value.strip():
            raise SigningError(f"{PUBLISH_NAME} configuration has an empty {key}")


def normalize_config(directory: Path) -> Path:
    config_path = find_single(directory, CONFIG_NAME)
    try:
        configs = json.loads(config_path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise SigningError(f"{CONFIG_NAME} is not readable JSON") from error
    if not isinstance(configs, list):
        raise SigningError(f"{CONFIG_NAME} must contain a JSON array")
    config = next(
        (
            entry
            for entry in configs
            if isinstance(entry, dict) and entry.get("name") == PUBLISH_NAME
        ),
        None,
    )
    if config is None:
        raise SigningError(f"{CONFIG_NAME} has no {PUBLISH_NAME} configuration")
    material = config.get("material")
    if not isinstance(material, dict):
        raise SigningError(f"{PUBLISH_NAME} configuration has no material")

    profile = material.get("profile")
    profile_name = basename(profile) if isinstance(profile, str) else ""
    if profile_name != PUBLISH_PROFILE_NAME:
        raise SigningError(
            f"{PUBLISH_NAME} configuration must use the {PUBLISH_PROFILE_NAME} profile"
        )

    for field in MATERIAL_PATH_FIELDS:
        material[field] = str(resolve_material(directory, material, field))
    validate_passwords(material)

    config_path.write_text(json.dumps(configs, indent=2) + "\n", encoding="utf-8")
    os.chmod(config_path, 0o600)
    return config_path


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(f"usage: {Path(argv[0]).name} OUTPUT_DIRECTORY", file=sys.stderr)
        return 2
    directory = Path(argv[1]).expanduser()
    if not directory.is_absolute():
        directory = Path.cwd() / directory
    try:
        archive = decode_archive(os.environ.get(ARCHIVE_ENV, ""))
        prepare_directory(directory)
        extract(archive, directory)
        normalize_config(directory)
        file_count = sum(1 for path in directory.rglob("*") if path.is_file())
        print(
            f"Prepared the {PUBLISH_NAME} OHOS signing material: "
            f"{directory} ({file_count} files)"
        )
    except SigningError as error:
        print(f"OHOS release signing material is unusable: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
