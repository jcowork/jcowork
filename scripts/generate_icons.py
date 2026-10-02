#!/usr/bin/env python3
"""Generate all Jcowork brand assets from the master logo.

Master asset: assets/logo.png — the circular "jcowork" logo with transparent
corners. If the master does not exist yet (or --source is given), it is
extracted from a raw square image: detect the circular mark, crop to its
bounds and apply an anti-aliased circular alpha mask.

Outputs (overwritten in place, same paths Tauri/favicon references use):
  crates/jcowork-desktop/icons/32x32.png
  crates/jcowork-desktop/icons/128x128.png
  crates/jcowork-desktop/icons/128x128@2x.png
  crates/jcowork-desktop/icons/icon.icns
  crates/jcowork-desktop/icons/icon.ico
  web/public/favicon.svg
  web/public/favicon-32.png
  web/public/favicon-192.png
  web/public/apple-touch-icon.png
  web/public/logo.png
  miniprogram/images/logo.png

Usage:
  python3 scripts/generate_icons.py
  python3 scripts/generate_icons.py --source /path/to/raw-logo.png
"""

import argparse
import base64
import io
import os
import shutil
import subprocess
import sys

from PIL import Image, ImageDraw, ImageFilter

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MASTER = os.path.join(REPO, "assets", "logo.png")

DESKTOP_ICONS = os.path.join(REPO, "crates", "jcowork-desktop", "icons")
WEB_PUBLIC = os.path.join(REPO, "web", "public")
MP_IMAGES = os.path.join(REPO, "miniprogram", "images")

# Background luminance below this counts as the logo mark when detecting the
# circle bounds of a raw source image (excludes light watermarks).
DARK_THRESHOLD = 230
# The raw image may carry a thin light watermark outside the mark; eroding the
# dark mask by this many pixels removes it before the bounding box is taken.
ERODE = 15
MASTER_SIZE = 1024


def extract_master(source_path: str) -> None:
    """Crop the circular mark from a raw square image into assets/logo.png."""
    img = Image.open(source_path).convert("RGBA")
    # Bounding box of the solid mark: erode so thin strokes (watermarks) drop
    # out, then re-expand to recover the true circle edge.
    dark = img.convert("L").point(lambda v: 255 if v < DARK_THRESHOLD else 0)
    eroded = dark.filter(ImageFilter.MinFilter(ERODE))
    bbox = eroded.getbbox()
    if not bbox:
        sys.exit("no logo mark found in %s" % source_path)
    grow = (ERODE - 1) // 2
    left, top = bbox[0] - grow, bbox[1] - grow
    right, bottom = bbox[2] + grow, bbox[3] + grow
    side = max(right - left, bottom - top)
    cx, cy = (left + right) // 2, (top + bottom) // 2
    crop = img.crop((cx - side // 2, cy - side // 2, cx + side // 2, cy + side // 2))
    crop = crop.resize((MASTER_SIZE, MASTER_SIZE), Image.LANCZOS)

    # Anti-aliased circular mask (draw 4x, downsample) with a small inset so
    # the white halo of the raw background is fully removed.
    ss = 4
    mask = Image.new("L", (MASTER_SIZE * ss, MASTER_SIZE * ss), 0)
    inset = 2 * ss
    ImageDraw.Draw(mask).ellipse(
        [inset, inset, MASTER_SIZE * ss - 1 - inset, MASTER_SIZE * ss - 1 - inset],
        fill=255,
    )
    mask = mask.resize((MASTER_SIZE, MASTER_SIZE), Image.LANCZOS)
    crop.putalpha(mask)

    os.makedirs(os.path.dirname(MASTER), exist_ok=True)
    crop.save(MASTER, "PNG")
    print("master   -> %s (%dx%d)" % (MASTER, MASTER_SIZE, MASTER_SIZE))


def load_master() -> Image.Image:
    if not os.path.exists(MASTER):
        sys.exit("master logo missing: %s (run with --source <raw.png> once)" % MASTER)
    return Image.open(MASTER).convert("RGBA")


def circle(logo: Image.Image, size: int) -> Image.Image:
    return logo.resize((size, size), Image.LANCZOS)


def save_png(img: Image.Image, path: str) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    img.save(path, "PNG", optimize=True)
    print("         -> %s (%dx%d)" % (path, img.width, img.height))


def make_desktop_icons(logo: Image.Image) -> None:
    print("desktop icons:")
    save_png(circle(logo, 32), os.path.join(DESKTOP_ICONS, "32x32.png"))
    save_png(circle(logo, 128), os.path.join(DESKTOP_ICONS, "128x128.png"))
    save_png(circle(logo, 256), os.path.join(DESKTOP_ICONS, "128x128@2x.png"))

    # .ico with a proper multi-size set (PIL writes real ICO, not icns-in-.ico)
    ico_path = os.path.join(DESKTOP_ICONS, "icon.ico")
    circle(logo, 256).save(
        ico_path,
        format="ICO",
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
    )
    print("         -> %s" % ico_path)

    # .icns via iconutil iconset; fallback to sips, then to a plain PNG copy
    icns_path = os.path.join(DESKTOP_ICONS, "icon.icns")
    iconset = os.path.join(DESKTOP_ICONS, "icon.iconset")
    if os.path.isdir(iconset):
        shutil.rmtree(iconset)
    os.makedirs(iconset, exist_ok=True)
    for size, name in [
        (16, "icon_16x16.png"),
        (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"),
        (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"),
        (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"),
        (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"),
        (1024, "icon_512x512@2x.png"),
    ]:
        circle(logo, size).save(os.path.join(iconset, name), "PNG")
    rc = subprocess.call(["iconutil", "-c", "icns", iconset, "-o", icns_path])
    shutil.rmtree(iconset, ignore_errors=True)
    if rc != 0 or not os.path.exists(icns_path):
        subprocess.call(["sips", "-s", "format", "icns", MASTER, "--out", icns_path])
    if not os.path.exists(icns_path):
        shutil.copy2(MASTER, icns_path)
    print("         -> %s" % icns_path)


def make_web_assets(logo: Image.Image) -> None:
    print("web assets:")
    save_png(circle(logo, 32), os.path.join(WEB_PUBLIC, "favicon-32.png"))
    save_png(circle(logo, 192), os.path.join(WEB_PUBLIC, "favicon-192.png"))
    save_png(circle(logo, 256), os.path.join(WEB_PUBLIC, "logo.png"))

    # apple-touch-icon: iOS composites transparency on black, so flatten the
    # circle on white to keep the intended look.
    at = Image.new("RGBA", (180, 180), (255, 255, 255, 255))
    inner = circle(logo, int(180 * 0.92))
    at.paste(inner, ((180 - inner.width) // 2, (180 - inner.height) // 2), inner)
    save_png(at.convert("RGB"), os.path.join(WEB_PUBLIC, "apple-touch-icon.png"))

    # SVG favicon embedding the 128px PNG (circular, transparent corners)
    buf = io.BytesIO()
    circle(logo, 128).save(buf, "PNG", optimize=True)
    b64 = base64.b64encode(buf.getvalue()).decode("ascii")
    svg = (
        '<svg xmlns="http://www.w3.org/2000/svg" width="128" height="128" '
        'viewBox="0 0 128 128">\n'
        '  <image width="128" height="128" '
        'href="data:image/png;base64,%s"/>\n'
        "</svg>\n" % b64
    )
    with open(os.path.join(WEB_PUBLIC, "favicon.svg"), "w", encoding="utf-8") as f:
        f.write(svg)
    print("         -> %s/favicon.svg (embedded 128px PNG)" % WEB_PUBLIC)


def make_miniprogram_assets(logo: Image.Image) -> None:
    print("miniprogram assets:")
    save_png(circle(logo, 256), os.path.join(MP_IMAGES, "logo.png"))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--source",
        help="raw square logo image to (re)extract assets/logo.png from",
    )
    args = parser.parse_args()

    if args.source:
        extract_master(args.source)
    elif not os.path.exists(MASTER):
        sys.exit("master logo missing: %s (run with --source <raw.png> once)" % MASTER)

    logo = load_master()
    make_desktop_icons(logo)
    make_web_assets(logo)
    make_miniprogram_assets(logo)
    print("\nAll brand assets generated!")


if __name__ == "__main__":
    main()
