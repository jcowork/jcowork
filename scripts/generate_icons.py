#!/usr/bin/env python3
"""Generate Jcowork app icons with 'Jc' text."""

from PIL import Image, ImageDraw, ImageFont
import os

ICON_DIR = "/Users/jiang/jcowork/crates/jcowork-desktop/icons"

def create_icon(size: int, output_path: str):
    """Create a rounded-square icon with 'Jc' text."""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)

    # Background: dark navy blue gradient-like solid color
    bg_color = (26, 26, 46, 255)  # #1a1a2e

    # Draw rounded rectangle background
    margin = max(2, size // 32)
    radius = size // 5
    draw.rounded_rectangle(
        [margin, margin, size - margin - 1, size - margin - 1],
        radius=radius,
        fill=bg_color,
    )

    # Add a subtle accent bar at the bottom
    accent_color = (59, 130, 246, 200)  # Blue accent
    bar_height = max(3, size // 16)
    bar_y = size - margin - bar_height - 2
    draw.rounded_rectangle(
        [margin + radius // 2, bar_y, size - margin - radius // 2 - 1, size - margin - 2],
        radius=bar_height // 2,
        fill=accent_color,
    )

    # Draw "Jc" text
    text = "Jc"
    # Try to use a bold system font
    font_size = int(size * 0.42)
    font = None
    font_paths = [
        "/System/Library/Fonts/Helvetica.ttc",
        "/System/Library/Fonts/HelveticaNeue.ttc",
        "/System/Library/Fonts/SFNSDisplay.ttf",
        "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
        "/Library/Fonts/Arial Bold.ttf",
    ]
    for fp in font_paths:
        if os.path.exists(fp):
            try:
                font = ImageFont.truetype(fp, font_size)
                break
            except Exception:
                continue

    if font is None:
        font = ImageFont.load_default()

    # Center the text
    bbox = draw.textbbox((0, 0), text, font=font)
    text_w = bbox[2] - bbox[0]
    text_h = bbox[3] - bbox[1]
    x = (size - text_w) // 2 - bbox[0]
    y = (size - text_h) // 2 - bbox[1] - size // 16  # Slightly above center

    # Text shadow
    shadow_color = (0, 0, 0, 80)
    draw.text((x + 2, y + 2), text, fill=shadow_color, font=font)

    # Main text: white
    text_color = (255, 255, 255, 255)
    draw.text((x, y), text, fill=text_color, font=font)

    img.save(output_path, "PNG")
    print(f"Created {output_path} ({size}x{size})")


# Generate all required sizes
os.makedirs(ICON_DIR, exist_ok=True)

create_icon(32, os.path.join(ICON_DIR, "32x32.png"))
create_icon(128, os.path.join(ICON_DIR, "128x128.png"))
create_icon(256, os.path.join(ICON_DIR, "128x128@2x.png"))

# Generate icon.icns from the 256px PNG using sips
icns_path = os.path.join(ICON_DIR, "icon.icns")
png_256 = os.path.join(ICON_DIR, "128x128@2x.png")

# Use sips to create icns
os.system(f'sips -s format icns "{png_256}" --out "{icns_path}" 2>/dev/null')
if os.path.exists(icns_path):
    print(f"Created {icns_path}")
else:
    # Fallback: copy png as icns (Tauri can handle this)
    import shutil
    shutil.copy2(png_256, icns_path)
    print(f"Created {icns_path} (fallback copy)")

# Generate icon.ico from the 256px PNG using sips
ico_path = os.path.join(ICON_DIR, "icon.ico")
os.system(f'sips -s format icns "{png_256}" --out "{ico_path}" 2>/dev/null')
if not os.path.exists(ico_path) or os.path.getsize(ico_path) == 0:
    import shutil
    shutil.copy2(png_256, ico_path)
print(f"Created {ico_path}")

print("\nAll icons generated!")
