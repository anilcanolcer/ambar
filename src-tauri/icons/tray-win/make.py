"""Makes the Windows tray icons from the macOS menu bar frames.

The macOS frames (../tray/00..20.png, 40x36) are black alpha masks that
macOS tints itself. Windows shows icons as they are, so each frame is
coloured here, once for a dark and once for a light taskbar:

  00..19  the faint bucket (frame 00) in the taskbar's text colour, with the
          part that frame k fills in Kova red: red means "downloading"
  20      the full bucket in the text colour alone: the icon at rest

Frames are padded to 40x40 (centred) and scaled down to 32x32; Windows
scales that to the tray size (16 px at 100%). Run: python3 -I make.py
"""

from pathlib import Path

from PIL import Image

HERE = Path(__file__).resolve().parent
SOURCE = HERE.parent / "tray"
THEMES = {
    # Folder: (text colour, red), both sRGB.
    "dark": ((0xFF, 0xFF, 0xFF), (0xFF, 0x3B, 0x3B)),
    "light": ((0x1F, 0x1F, 0x1F), (0xE3, 0x12, 0x1B)),
}
SIZE = 32


def mask(k):
    """Alpha of mac frame k, padded to a square and scaled to SIZE."""
    frame = Image.open(SOURCE / f"{k:02}.png").getchannel("A")
    square = Image.new("L", (40, 40), 0)
    square.paste(frame, ((40 - frame.width) // 2, (40 - frame.height) // 2))
    return square.resize((SIZE, SIZE), Image.LANCZOS)


def colour(ghost, full, text, red):
    """`ghost` in the text colour with the rest of `full` in red on top."""
    out = Image.new("RGBA", (SIZE, SIZE))
    for xy in ((x, y) for y in range(SIZE) for x in range(SIZE)):
        a0, ak = ghost.getpixel(xy) / 255, full.getpixel(xy) / 255
        # Red's own alpha, so that red over the ghost ends up as opaque as
        # frame k is: r + a0 * (1 - r) = ak.
        r = max(0.0, (ak - a0) / (1 - a0)) if a0 < 1 else 0.0
        alpha = r + a0 * (1 - r)
        if alpha <= 0:
            out.putpixel(xy, (0, 0, 0, 0))
            continue
        rgb = tuple(round((c_red * r + c_text * a0 * (1 - r)) / alpha) for c_red, c_text in zip(red, text))
        out.putpixel(xy, (*rgb, round(alpha * 255)))
    return out


def main():
    ghost = mask(0)
    full = mask(20)
    empty = Image.new("L", (SIZE, SIZE), 0)
    for name, (text, red) in THEMES.items():
        folder = HERE / name
        folder.mkdir(exist_ok=True)
        for k in range(20):
            colour(ghost, mask(k), text, red).save(folder / f"{k:02}.png", optimize=True)
        # At rest: the whole bucket in the text colour, no red.
        colour(full, empty, text, red).save(folder / "20.png", optimize=True)


if __name__ == "__main__":
    main()
