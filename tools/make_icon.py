"""Draws the app icon: a square of bottle ink with a gold till-display bar and
'OS' set in Bahnschrift. Square corners, matching the UI. Writes assets/icon.png
and assets/icon.ico.  Usage: python tools/make_icon.py"""
from PIL import Image, ImageDraw, ImageFont

INK, GOLD, WHITE = (0x12, 0x30, 0x3A), (0xE2, 0xA7, 0x2E), (0xE6, 0xEE, 0xEC)
S = 256
img = Image.new("RGBA", (S, S), INK)
d = ImageDraw.Draw(img)
font = ImageFont.truetype(r"C:\Windows\Fonts\bahnschrift.ttf", 150)
text = "OS"
box = d.textbbox((0, 0), text, font=font)
w, h = box[2] - box[0], box[3] - box[1]
d.text(((S - w) / 2 - box[0], 52 - box[1]), text, font=font, fill=WHITE)
d.rectangle([40, 196, S - 40, 214], fill=GOLD)  # the till-display bar
img.save("assets/icon.png")
img.save("assets/icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
print("wrote assets/icon.png, assets/icon.ico")

# Installer artwork (NSIS wants 24-bit BMP). Drawn at 2x for high-DPI screens;
# the installer scales them to fit.
def panel(w, h, mark_size, bar_y):
    im = Image.new("RGB", (w, h), INK)
    dr = ImageDraw.Draw(im)
    f = ImageFont.truetype(r"C:\Windows\Fonts\bahnschrift.ttf", mark_size)
    b = dr.textbbox((0, 0), "OS", font=f)
    dr.text((40 - b[0], 60 - b[1]), "OS", font=f, fill=WHITE)
    dr.rectangle([40, bar_y, 40 + (b[2] - b[0]), bar_y + mark_size // 9], fill=GOLD)
    return im

panel(328, 628, 120, 196).save("assets/installer-side.bmp")
hdr = Image.new("RGB", (300, 114), (255, 255, 255))
hd = ImageDraw.Draw(hdr)
hd.rectangle([186, 14, 286, 100], fill=INK)
hf = ImageFont.truetype(r"C:\Windows\Fonts\bahnschrift.ttf", 54)
hb = hd.textbbox((0, 0), "OS", font=hf)
hd.text((236 - (hb[2] - hb[0]) / 2 - hb[0], 26 - hb[1]), "OS", font=hf, fill=WHITE)
hd.rectangle([204, 84, 268, 90], fill=GOLD)
hdr.save("assets/installer-header.bmp")
print("wrote assets/installer-side.bmp, assets/installer-header.bmp")
