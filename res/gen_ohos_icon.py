#!/usr/bin/env python3
"""Generate the HarmonyOS icon resources from res/icon.png.

The artwork stays the official RustDesk logo; two changes are applied:

* the canvas is cut to a rounded square so the start window icon shows rounded
  corners against any start window background, and
* an "unofficial" badge is placed in the bottom-right corner.

The corners are transparent, which restool accepts (verified with --icon-check)
and which keeps the tile readable on both light and dark backgrounds.
"""

import argparse
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFont, ImageStat

CANVAS = 1024
CORNER_RADIUS = 232
BADGE_SIZE = (392, 140)
BADGE_MARGIN = 50
BADGE_TEXT = '非官方'
BADGE_OUTLINE = 14
BADGE_FONT_SIZE = 96
BADGE_SHADE = 0.84
FONT_CANDIDATES = (
    '/System/Library/Fonts/PingFang.ttc',
    '/System/Library/Fonts/Hiragino Sans GB.ttc',
    '/System/Library/Fonts/STHeiti Medium.ttc',
)
LOGO_TARGETS = (
    'AppScope/resources/base/media/app_icon.png',
    'entry/src/main/resources/base/media/icon.png',
    'entry/src/ohosTest/resources/base/media/icon.png',
)


def logo_colour(source):
    """Mean colour of the logo's coloured ring, used for the badge."""
    rgb = source.convert('RGB')
    red, green, blue = rgb.split()
    lightest = ImageChops.lighter(ImageChops.lighter(red, green), blue)
    darkest = ImageChops.darker(ImageChops.darker(red, green), blue)
    saturation = ImageChops.subtract(lightest, darkest)
    mask = saturation.point(lambda value: 255 if value > 40 else 0)
    mean = ImageStat.Stat(rgb, mask=mask).mean
    return tuple(int(value * BADGE_SHADE) for value in mean)


def badge_font(size):
    for candidate in FONT_CANDIDATES:
        path = Path(candidate)
        if not path.is_file():
            continue
        for index in range(6):
            try:
                return ImageFont.truetype(str(path), size=size, index=index)
            except OSError:
                continue
    raise SystemExit('no CJK-capable font found for the unofficial badge')


def build_icon(source):
    artwork = source.convert('RGBA')
    if artwork.size != (CANVAS, CANVAS):
        artwork = artwork.resize((CANVAS, CANVAS), Image.LANCZOS)

    rounded = Image.new('L', (CANVAS, CANVAS), 0)
    ImageDraw.Draw(rounded).rounded_rectangle(
        [0, 0, CANVAS - 1, CANVAS - 1], radius=CORNER_RADIUS, fill=255)
    icon = Image.new('RGBA', (CANVAS, CANVAS), (0, 0, 0, 0))
    icon.paste(artwork, (0, 0), rounded)

    width, height = BADGE_SIZE
    left, top = CANVAS - BADGE_MARGIN - width, CANVAS - BADGE_MARGIN - height
    draw = ImageDraw.Draw(icon)
    draw.rounded_rectangle([left, top, left + width, top + height],
                           radius=height // 2, fill=logo_colour(source) + (255,),
                           outline=(255, 255, 255, 255), width=BADGE_OUTLINE)
    font = badge_font(BADGE_FONT_SIZE)
    box = draw.textbbox((0, 0), BADGE_TEXT, font=font)
    draw.text((left + (width - box[2] + box[0]) / 2,
               top + (height - box[3] + box[1]) / 2),
              BADGE_TEXT, font=font, fill=(255, 255, 255, 255))
    return icon


def main():
    parser = argparse.ArgumentParser(description=__doc__.strip().splitlines()[0])
    parser.add_argument('--source', type=Path,
                        default=Path(__file__).resolve().parent / 'icon.png')
    parser.add_argument('--ohos', type=Path,
                        default=Path(__file__).resolve().parents[1] / 'flutter/ohos')
    parser.add_argument('--preview', type=Path,
                        help='write a preview instead of the application resources')
    args = parser.parse_args()

    icon = build_icon(Image.open(args.source))
    if args.preview:
        icon.resize((256, 256), Image.LANCZOS).save(args.preview, format='PNG')
        print(args.preview)
        return

    for relative in LOGO_TARGETS:
        target = args.ohos / relative
        icon.save(target, format='PNG', optimize=True)
        print(f'{relative}: {target.stat().st_size} bytes')


if __name__ == '__main__':
    main()
