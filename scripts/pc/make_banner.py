# -*- coding: utf-8 -*-
"""Баннер README в бренде Omnes (OmnesProducts brandbook):
Dark Charcoal #111827 / Off-White #F4F5F7 / Brand Orange #FF6B00.
Логотип — официальный src/viewer/assets/ob2h.png (слон в угольном круге).
Запуск: python scripts/pc/make_banner.py (из корня репо; пишет docs/assets/banner.png).
"""
from PIL import Image, ImageDraw, ImageFont

CHARCOAL = (17, 24, 39, 255)      # #111827
CHARCOAL_2 = (30, 37, 53, 255)    # чуть светлее — карточка/акцент фона
OFFWHITE = (244, 245, 247, 255)   # #F4F5F7
ORANGE = (255, 107, 0, 255)       # #FF6B00
GRAY = (148, 163, 184, 255)       # нейтральный серый для подзаголовка

W, H = 1500, 500

def font(name, size):
    import os
    root = os.environ.get("SystemRoot", r"C:\Windows")
    return ImageFont.truetype(os.path.join(root, "Fonts", name), size)

def main():
    img = Image.new("RGBA", (W, H), CHARCOAL)
    d = ImageDraw.Draw(img)

    # тонкая оранжевая полоса-акцент по нижней кромке (брендовый приём)
    d.rectangle([0, H - 8, W, H], fill=ORANGE)

    # логотип слева
    logo = Image.open("src/viewer/assets/ob2h.png").convert("RGBA")
    L = 360
    logo = logo.resize((L, L), Image.LANCZOS)
    img.alpha_composite(logo, (70, (H - L) // 2))

    # заголовок: ob2h + оранжевый промпт-курсор
    f_title = font("segoeuib.ttf", 130)
    f_prompt = font("consolab.ttf", 130) if _exists("consolab.ttf") else f_title
    f_sub = font("segoeui.ttf", 44)
    f_sub2 = font("segoeui.ttf", 30)

    x = 500
    y = 118
    d.text((x, y), "ob2h", font=f_title, fill=OFFWHITE)
    w = d.textlength("ob2h", font=f_title)
    d.text((x + w + 24, y), ">_", font=f_prompt, fill=ORANGE)

    d.text((x, y + 168), "Локальный мозг для AI-агентов:", font=f_sub, fill=OFFWHITE)
    d.text((x, y + 228), "память, граф знаний и AST-граф кода", font=f_sub, fill=GRAY)
    d.text((x, y + 286), "OmnesProducts · Rust · MCP · SQLite", font=f_sub2, fill=GRAY)

    img.convert("RGB").save("docs/assets/banner.png", "PNG")
    print("ok: docs/assets/banner.png")

def _exists(name):
    import os
    root = os.environ.get("SystemRoot", r"C:\Windows")
    return os.path.exists(os.path.join(root, "Fonts", name))

if __name__ == "__main__":
    main()
