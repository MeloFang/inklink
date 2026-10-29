#!/usr/bin/env python3
"""从品牌图形生成产品图标。

输入：docs/brand/variants/ink-script.png（`ink` 手写体，透明底）
输出：
  installer/inklink.ico            多尺寸图标（16/20/24/32/40/48/64/256）
  installer/inklink.png            512 预览（文档/通知用）
  installer/inklink-badge.ico      浮条第一格用的标记（透明底、字形撑满）
  docs/brand/_probe-icon-sizes.png 各尺寸在白底与深底上的对照（检查用，不进 git）

为什认到字母 i：整套 `ink` 在小尺寸下会糊成一团，只留 `i` + 墨点才认得出。

两种底板，各有各的理由：
  - 产品图标：**深蓝圆角方块 + 白色字形**。白底版本在 16/20px 上只剩几根细线，
    语言列表里看着就是一团灰；反过来（深蓝底 + 白字）和品牌徽标同构，
    小尺寸下对比度高得多，深色任务栏上也显眼。
  - 浮条第一格：透明底、字形撑满高度。那一格只有 20~24px，套一层方块等于把
    笔画再缩一圈。

只做裁切与反色，不改字形、不重绘。
"""
import os
import sys

from PIL import Image, ImageDraw, ImageFilter

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SRC = os.path.join(ROOT, "docs", "brand", "variants", "ink-script.png")
OUT_ICO = os.path.join(ROOT, "installer", "inklink.ico")
OUT_PNG = os.path.join(ROOT, "installer", "inklink.png")
OUT_BADGE = os.path.join(ROOT, "installer", "inklink-badge.ico")
OUT_PROBE = os.path.join(ROOT, "docs", "brand", "_probe-icon-sizes.png")

MASTER = 1024               # 预览用母版边长
TILE_RADIUS_RATIO = 0.24    # 圆角半径 / 边长
MARK_WIDTH_RATIO = 0.78     # 字形占画布宽度的比例（四周留白）
MARK_MAX_HEIGHT = 0.92      # 字形最高占画布的多少
CROP = (110, 0, 300, 332)   # 在 ink-script.png 里截取字母 i（含墨点与起笔；右边界 300 刚好切在 i 的收笔处，避免带上 n 的起笔变成"il"）

SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
NAVY = (6, 38, 87, 255)     # 品牌深蓝 #062657（取自徽标椭圆底色）


def load_mark():
    im = Image.open(SRC).convert("RGBA")
    mark = im.crop(CROP)
    bbox = mark.getbbox()          # 去掉裁切带来的多余透明边，让字形自己占满
    return mark.crop(bbox) if bbox else mark


def white_mark(mark):
    """把字形反色成纯白：只换颜色，形状完全来自 alpha 通道。"""
    solid = Image.new("RGBA", mark.size, (255, 255, 255, 255))
    solid.putalpha(mark.split()[3])
    return solid


def fit(mark, side, ratio, max_height):
    tw = max(1, int(side * ratio))
    th = max(1, round(mark.size[1] * tw / mark.size[0]))
    if th > side * max_height:
        th = int(side * max_height)
        tw = max(1, round(mark.size[0] * th / mark.size[1]))
    return tw, th


def tile_icon(mark, side):
    """深蓝圆角方块 + 白色字形。直接从源字形缩到这个尺寸，不用 1024 母版再缩，
    少一次重采样，小尺寸的笔画会硬一点。"""
    tw, th = fit(mark, side, MARK_WIDTH_RATIO, MARK_MAX_HEIGHT)
    glyph = mark.resize((tw, th), Image.LANCZOS)
    if side <= 48:                              # 小尺寸补一点锐度，抵消缩放糊
        glyph = glyph.filter(ImageFilter.UnsharpMask(radius=0.6, percent=90, threshold=0))
    canvas = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    d = ImageDraw.Draw(canvas)
    d.rounded_rectangle([0, 0, side - 1, side - 1], radius=int(side * TILE_RADIUS_RATIO), fill=NAVY)
    canvas.alpha_composite(glyph, ((side - tw) // 2, (side - th) // 2))
    return canvas


def bar_mark(mark, side):
    """浮条第一格：透明底、字形撑满高度。"""
    th = max(1, int(side * 0.96))
    tw = max(1, round(mark.size[0] * th / mark.size[1]))
    if tw > side:
        tw = side
        th = max(1, round(mark.size[1] * tw / mark.size[0]))
    canvas = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    canvas.alpha_composite(mark.resize((tw, th), Image.LANCZOS), ((side - tw) // 2, (side - th) // 2))
    return canvas


def save_ico(squares, path):
    squares[256].save(
        path,
        format="ICO",
        sizes=[(s, s) for s in SIZES],
        append_images=[squares[s] for s in SIZES if s != 256],
    )


def main():
    if not os.path.exists(SRC):
        sys.exit(f"缺少品牌图形：{SRC}")

    mark = load_mark()
    white = white_mark(mark)

    icons = {s: tile_icon(white, s) for s in SIZES}
    save_ico(icons, OUT_ICO)
    icons[256].resize((512, 512), Image.LANCZOS).save(OUT_PNG)

    badges = {s: bar_mark(mark, s) for s in SIZES}
    save_ico(badges, OUT_BADGE)

    # 检查图：白底与深底各一行
    pad, gap = 16, 18
    w = sum(s + pad * 2 for s in SIZES) + gap * (len(SIZES) + 1)
    h = (256 + pad * 2) * 2 + gap * 3
    sheet = Image.new("RGB", (w, h), (245, 245, 245))
    y = gap
    for bg in [(255, 255, 255), (18, 18, 20)]:
        x = gap
        for s in SIZES:
            cell = Image.new("RGBA", (s + pad * 2, s + pad * 2), bg + (255,))
            cell.alpha_composite(icons[s], (pad, pad))
            sheet.paste(cell.convert("RGB"), (x, y + (256 - s) // 2 + pad))
            x += s + pad * 2
        y += 256 + pad * 2 + gap
    sheet.save(OUT_PROBE)

    print(f"mark      : {mark.size[0]}x{mark.size[1]}（裁自 {os.path.relpath(SRC, ROOT)} {CROP}）")
    print(f"ico       : {os.path.relpath(OUT_ICO, ROOT)}  {os.path.getsize(OUT_ICO)} 字节  {SIZES}")
    print(f"png       : {os.path.relpath(OUT_PNG, ROOT)}  {os.path.getsize(OUT_PNG)} 字节")
    print(f"浮条标记  : {os.path.relpath(OUT_BADGE, ROOT)}  {os.path.getsize(OUT_BADGE)} 字节")
    print(f"检查图    : {os.path.relpath(OUT_PROBE, ROOT)}")


if __name__ == "__main__":
    main()
