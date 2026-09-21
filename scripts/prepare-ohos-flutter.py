#!/usr/bin/env python3
"""Select Flutter-OH dependencies without changing standard Flutter builds.

--check asserts the reverse: the committed pubspec still selects the standard
dependencies, so every leg that consumes it unchanged keeps compiling.
"""

import argparse
import re
from pathlib import Path


DEPENDENCIES = (
    ('settings_ui', 'https://github.com/FrankHan052176/flutter-settings-ui',
     '2bdac5d1670c53734428898589516d18d4677477',
     '79b5c4712fc4594e94aacd9e2bc2996d8de1fd5d'),
    ('flex_color_picker', 'https://github.com/FrankHan052176/flex_color_picker',
     '086e7767b609baad0ebab20d76a894ccdfa628dd',
     '29d06e3368448ebd2eebdad4766d1cb4d688a0c2'),
    ('xterm', 'https://github.com/FrankHan052176/xterm.dart',
     '4266ec2e501568f9dc494a88c8e56288db21ad7c',
     'e901d133ceba2b5e3a54f690cd45b4b0988ff158'),
)


def prepare(source: str) -> str:
    for name, url, standard_ref, ohos_ref in DEPENDENCIES:
        prefix = f'  {name}:\n    git:\n      url: {url}\n      ref: '
        pattern = rf'(?m)^{re.escape(prefix)}(?:{standard_ref}|{ohos_ref})$'
        source, count = re.subn(pattern, f'{prefix}{ohos_ref}', source)
        if count != 1:
            raise ValueError(f'Expected one supported {name} Git dependency')
    # Flutter-OH 3.41 requires the newer SelectionHandler implementation.
    source, count = re.subn(
        r'(?m)^  extended_text: (?:\^14\.2\.0|\^15\.0\.2)$',
        '  extended_text: ^15.0.2', source,
    )
    if count != 1:
        raise ValueError('Expected one supported extended_text dependency')
    # FontWeight has non-primitive equality in Flutter-OH 3.41.
    source, count = re.subn(
        r'(?m)^  google_fonts: (?:\^6\.2\.1|\^8\.1\.0|\^8\.2\.1)$',
        '  google_fonts: ^8.1.0', source,
    )
    if count != 1:
        raise ValueError('Expected one supported google_fonts dependency')
    return source


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pubspec', type=Path,
                        default=Path(__file__).resolve().parents[1] / 'flutter/pubspec.yaml')
    parser.add_argument('--check', action='store_true',
                        help='fail if the committed pubspec is already prepared for Flutter-OH')
    args = parser.parse_args()
    source = args.pubspec.read_text(encoding='utf-8')
    try:
        patched = prepare(source)
    except ValueError as error:
        parser.exit(1, f'{error}; pubspec was not changed\n')
    if args.check:
        if patched == source:
            parser.exit(1, 'The committed pubspec is already prepared for Flutter-OH; '
                           'keep the standard dependencies committed\n')
        print('The committed pubspec selects the standard dependencies.')
        return
    if patched != source:
        args.pubspec.write_text(patched, encoding='utf-8')
    print('Flutter-OH dependency constraints are ready.')


if __name__ == '__main__':
    main()
