#!/usr/bin/env python3
"""从品牌图形生成产品图标（inklink.ico + 预览图）。

输入：docs/brand/variants/ink-script.png（`ink` 手写体，透明底）
输出：
  installer/inklink.ico            多尺寸图标（16/20/24/32/40/48/64/256）
  installer/inklink.png            512 预览（文档/通知用）
  docs/brand/_probe-icon-sizes.png 各尺寸在白底与深底上的对照（检查用，不进 git）

为什么裁到字母 i：整套 `ink` 在小尺寸下会糊成一团，只留 `i` + 墨点才认得出。
画布是白色圆角方块（满幅），这样深色任务栏上也能看清——图形本身仍是品牌规定的
"深蓝字 + 白底"，没有改色、没有重绘。

用法：python tools/make-icon.py
"""
import os
import sys

from PIL import Image, ImageDraw

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SRC = os.path.join(ROOT, "docs", "brand", "variants", "ink-script.png")
OUT_ICO = os.path.join(ROOT, "installer", "inklink.ico")
OUT_PNG = os.path.join(ROOT, "installer", "inklink.png")
OUT_PROBE = os.path.join(ROOT, "docs", "brand", "_probe-icon-sizes.png")

MASTER = 1024               # 母版边长
TILE_RADIUS_RATIO = 0.22    # 圆角半径 / 边长
MARK_WIDTH_RATIO = 0.66     # 字形占画布宽度的比例（四周留白）
CROP = (110, 0, 300, 332)   # 在 ink-script.png 里截取字母 i（含墨点与起笔；右边界 300 刚好切在 i 的收笔处，避免带上 n 的起笔变成"il"）

SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
BG = (255, 255, 255, 255)   # 白底（品牌规定：图形只能放白底）


def load_mark():
    im = Image.open(SRC).convert("RGBA")
    mark = im.crop(CROP)
    bbox = mark.getbbox()          # 去掉裁切带来的多余透明边，让字形自己占满
    return mark.crop(bbox) if bbox else mark


def build_master(mark):
    side = MASTER
    tile = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    d = ImageDraw.Draw(tile)
    r = int(side * TILE_RADIUS_RATIO)
    d.rounded_rectangle([0, 0, side - 1, side - 1], radius=r, fill=BG)

    target_w = int(side * MARK_WIDTH_RATIO)
    scale = target_w / mark.size[0]
    target_h = max(1, round(mark.size[1] * scale))
    if target_h > side * 0.86:                       # 太高就按高度反算
        target_h = int(side * 0.86)
        target_w = max(1, round(mark.size[0] * target_h / mark.size[1]))
    small = mark.resize((target_w, target_h), Image.LANCZOS)
    tile.alpha_composite(small, ((side - target_w) // 2, (side - target_h) // 2))
    return tile


def build_sizes(master):
    return {s: master.resize((s, s), Image.LANCZOS) for s in SIZES}


def main():
    if not os.path.exists(SRC):
        sys.exit(f"缺少品牌图形：{SRC}")

    mark = load_mark()
    master = build_master(mark)
    sizes = build_sizes(master)

    master.resize((512, 512), Image.LANCZOS).save(OUT_PNG)
    sizes[256].save(
        OUT_ICO,
        format="ICO",
        sizes=[(s, s) for s in SIZES],
        append_images=[sizes[s] for s in SIZES if s != 256],
    )

    # 检查图：白底与深底各一行，按尺寸从小到大
    pad, gap = 16, 18
    cols = SIZES
    w = sum(s + pad * 2 for s in cols) + gap * (len(cols) + 1)
    h = (256 + pad * 2) * 2 + gap * 3
    sheet = Image.new("RGB", (w, h), (245, 245, 245))
    y = gap
    for bg in [(255, 255, 255), (18, 18, 20)]:
        x = gap
        for s in cols:
            cell = Image.new("RGBA", (s + pad * 2, s + pad * 2), bg + (255,))
            cell.alpha_composite(sizes[s], (pad, pad))
            sheet.paste(cell.convert("RGB"), (x, y + (256 - s) // 2 + pad))
            x += s + pad * 2
        y += 256 + pad * 2 + gap
    sheet.save(OUT_PROBE)

    print(f"mark      : {mark.size[0]}x{mark.size[1]}（裁自 {os.path.relpath(SRC, ROOT)} {CROP}）")
    print(f"母版      : {master.size[0]}x{master.size[1]}")
    print(f"ico       : {os.path.relpath(OUT_ICO, ROOT)}  {os.path.getsize(OUT_ICO)} 字节  {SIZES}")
    print(f"png       : {os.path.relpath(OUT_PNG, ROOT)}  {os.path.getsize(OUT_PNG)} 字节")
    print(f"检查图    : {os.path.relpath(OUT_PROBE, ROOT)}")


if __name__ == "__main__":
    main()
