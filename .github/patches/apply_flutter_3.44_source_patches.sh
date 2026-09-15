#!/usr/bin/env bash
# Shared by Flutter 3.44 builds and their bridge generation. Component themes
# are SDK-neutral in common.dart; only the dependency constraints differ now.
# Run from the repository root. Validate both keys before changing the file.
set -euo pipefail

python3 - <<'PY'
import re
from pathlib import Path

path = Path('flutter/pubspec.yaml')
source = path.read_text(encoding='utf-8')
patched = source
for package, version in (('extended_text', '^15.0.2'), ('google_fonts', '^8.1.0')):
    patched, count = re.subn(
        rf'(?m)^  {package}: [^\n]+$',
        f'  {package}: {version}',
        patched,
    )
    if count != 1:
        raise SystemExit(f'Expected exactly one {package} dependency; pubspec was not changed')
if patched != source:
    path.write_text(patched, encoding='utf-8')
print('Flutter 3.44 dependency constraints are ready.')
PY
