"""用几何重新绘制 inklink 的产品标记（不是缩放/描摹位图）。

标记 = 白底深蓝圆角方块 + 白色手写体 `i`（一滴墨点 + 一笔带锋的连笔）。

绘制方式：把这一笔定义成"中心线 + 半宽剖面"（毛笔的压力变化），
沿法向偏移得到闭合轮廓多边形；墨点是一段闭合贝塞尔。
多边形同时输出成两份：
  - `docs/brand/ink-mark.svg`  —— 真正的矢量源（任意放大都锐利）
  - `installer/inklink.ico` / `inklink.png` —— 每个尺寸**独立**按该尺寸原生渲染
    （16/20/24/32/40/48/64/128/256），不做位图缩放，所以小尺寸也不糊。

用法：
  python tools\\draw-logo.py            # 生成 svg + png + ico
  python tools\\draw-logo.py --preview  # 另外输出放大对照图 ui\\logo-check.png
"""

from __future__ import annotations

import math
import struct
import sys
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
BRAND_DIR = ROOT / "docs" / "brand"
INSTALLER = ROOT / "installer"
UI_DIR = ROOT / "ui"

# ── 配色（与产品图标一致，不改动）────────────────────────────────────────────
NAVY = (0x06, 0x26, 0x57)
WHITE = (0xFF, 0xFF, 0xFF)

# 画布：所有几何坐标都在这套 1000x1000 的单位里；输出时按目标尺寸缩放。
UNIT = 1000.0
CORNER_RATIO = 0.24          # 圆角半径 / 边长（与原图标一致）
MARK_BOX = (0.184, 0.057, 0.810, 0.953)   # 标记在画布里的包围盒（比例）

# ── 几何：一笔连写的 i ───────────────────────────────────────────────────────
# 记号 = 一个连续的钢笔笔顺：左侧游丝起笔 → 右上拱顶 → 急转回落的发夹 →
# 笔直的降笔（略向左收）→ 碗底 → 一路向右上挑出的长锋。
# 墨点是独立的一滴。
# 每段是 (点列, 半宽列)；点列用 Catmull-Rom 过点插值展平，半宽沿同参数插值。

BODY = (
    [
        (194, 628),
        (272, 596),
        (352, 560),
        (432, 519),
        (500, 479),
        (560, 443),
        (602, 420),
        (639, 428),
        (655, 453),
        (635, 477),
        (588, 498),
        (556, 530),
        (532, 572),
        (520, 628),
        (515, 690),
        (515, 752),
        (522, 798),
        (540, 840),
        (573, 861),
        (621, 866),
        (665, 852),
        (705, 824),
        (742, 790),
        (786, 745),
        (812, 700),
        (822, 668),
    ],
    [
        # 发夹处（第 6~9 点）的半宽不能超过中心线的曲率半径，否则内侧轮廓会翻折，
        # 大尺寸下能看到一道带疙瘩的暗缝（原来是 42/38/31/16）。
        0.8, 5.0, 14.0, 24.0, 31.0, 37.0, 37.0, 30.0, 23.0, 17.0, 12.0,
        14.0, 18.0, 22.0, 26.0, 28.0, 30.0, 31.0, 31.0, 29.0, 26.0, 22.0,
        16.0, 10.0, 5.0, 0.8,
    ],
)

# 墨点：从顶尖开始的闭合曲线（左半边略鼓，右半边略直，与原标记一致）
DROP = (
    (600, 55),
    ((642, 120), (664, 180), (662, 232)),
    ((660, 280), (628, 306), (598, 306)),
    ((558, 306), (529, 276), (529, 230)),
    ((529, 176), (560, 112), (600, 55)),
)


def catmull_rom(points: list[tuple[float, float]], per_seg: int = 24):
    """过点插值的 Catmull-Rom，返回展平后的点列（含首尾）。"""
    pts = [points[0]] + list(points) + [points[-1]]
    out: list[tuple[float, float]] = []
    for i in range(1, len(pts) - 2):
        p0, p1, p2, p3 = pts[i - 1], pts[i], pts[i + 1], pts[i + 2]
        for k in range(per_seg):
            t = k / per_seg
            t2, t3 = t * t, t * t * t
            x = 0.5 * ((2 * p1[0]) + (-p0[0] + p2[0]) * t
                       + (2 * p0[0] - 5 * p1[0] + 4 * p2[0] - p3[0]) * t2
                       + (-p0[0] + 3 * p1[0] - 3 * p2[0] + p3[0]) * t3)
            y = 0.5 * ((2 * p1[1]) + (-p0[1] + p2[1]) * t
                       + (2 * p0[1] - 5 * p1[1] + 4 * p2[1] - p3[1]) * t2
                       + (-p0[1] + 3 * p1[1] - 3 * p2[1] + p3[1]) * t3)
            out.append((x, y))
    out.append(pts[-2])
    return out


def lerp_width(widths: list[float], t: float) -> float:
    if len(widths) == 1:
        return widths[0]
    pos = t * (len(widths) - 1)
    i = min(int(pos), len(widths) - 2)
    f = pos - i
    return widths[i] * (1 - f) + widths[i + 1] * f


def stroke_outline(points, widths, per_seg: int = 24) -> list[tuple[float, float]]:
    """把"中心线 + 半宽"变成闭合轮廓：左法向走一遍，右法向反着回来。"""
    dense = catmull_rom(points, per_seg)
    n = len(dense)
    left: list[tuple[float, float]] = []
    right: list[tuple[float, float]] = []
    for i, (x, y) in enumerate(dense):
        if i == 0:
            dx, dy = dense[1][0] - x, dense[1][1] - y
        elif i == n - 1:
            dx, dy = x - dense[-2][0], y - dense[-2][1]
        else:
            dx, dy = dense[i + 1][0] - dense[i - 1][0], dense[i + 1][1] - dense[i - 1][1]
        length = math.hypot(dx, dy) or 1.0
        nx, ny = -dy / length, dx / length
        w = lerp_width(widths, i / (n - 1))
        left.append((x + nx * w, y + ny * w))
        right.append((x - nx * w, y - ny * w))
    return left + right[::-1]


def cubic(p0, p1, p2, p3, t):
    mt = 1 - t
    x = (mt ** 3) * p0[0] + 3 * (mt ** 2) * t * p1[0] + 3 * mt * (t ** 2) * p2[0] + (t ** 3) * p3[0]
    y = (mt ** 3) * p0[1] + 3 * (mt ** 2) * t * p1[1] + 3 * mt * (t ** 2) * p2[1] + (t ** 3) * p3[1]
    return (x, y)


def offset_closed(poly, d: float) -> list[tuple[float, float]]:
    """闭合曲线沿外法向整体外扩 d（只用于光滑、近似凸的墨点）。"""
    n = len(poly)
    area = sum(poly[i][0] * poly[(i + 1) % n][1] - poly[(i + 1) % n][0] * poly[i][1] for i in range(n))
    sign = 1.0 if area > 0 else -1.0
    out = []
    for i in range(n):
        (px, py), (nx_, ny_) = poly[i - 1], poly[(i + 1) % n]
        dx, dy = nx_ - px, ny_ - py
        length = math.hypot(dx, dy) or 1.0
        # 逆时针（area>0，y 向下的坐标里看是顺时针）时外法向是 (dy, -dx)
        out.append((poly[i][0] + sign * dy / length * d, poly[i][1] - sign * dx / length * d))
    return out


def shape_polygons(bold: float = 0.0) -> list[list[tuple[float, float]]]:
    """标记的全部闭合轮廓（单位：UNIT 画布）。

    `bold` 是笔画每侧额外加粗的量（同一单位）：只给小尺寸用，见 `polys_for()`。
    """
    polys = [stroke_outline(BODY[0], [w + bold for w in BODY[1]])]

    drop: list[tuple[float, float]] = []
    start = DROP[0]
    prev = start
    for seg in DROP[1:]:
        p1, p2, p3 = seg
        for k in range(1, 33):
            drop.append(cubic(prev, p1, p2, p3, k / 32))
        prev = p3
    if bold:
        drop = offset_closed(drop, bold)
    polys.append(drop)
    return polys


def bold_px(size: int) -> float:
    """小尺寸的光学加粗（每侧，单位：目标像素）。

    这一笔最细处只有 1~2px（游丝起笔、长锋），16~32px 下按真实宽度渲染会变成一片
    灰色半透明像素，看起来就是"糊"。像字体的小字号光学修正那样每侧补一点墨：
    16px 补 0.35px，32px 补 0.25px，64px 起不补，造型不变。
    """
    return max(0.0, 0.45 - size / 160)


def polys_for(size: int) -> list[list[tuple[float, float]]]:
    """该尺寸要渲染的轮廓（已经 fit 到 MARK_BOX）。"""
    base = shape_polygons()
    xs = [p[0] for poly in base for p in poly]
    ys = [p[1] for poly in base for p in poly]
    bx0, by0, bx1, by1 = (v * UNIT for v in MARK_BOX)
    s = min((bx1 - bx0) / (max(xs) - min(xs)), (by1 - by0) / (max(ys) - min(ys)))
    extra = bold_px(size) * UNIT / size / s     # 目标像素 → 原始几何单位
    if extra <= 0:
        return fit(base, MARK_BOX)
    # 加粗后外框会变大一点；用未加粗的外框做 fit，保证各尺寸的位置/大小完全一致。
    bold = shape_polygons(extra)
    mx, my = (min(xs) + max(xs)) / 2, (min(ys) + max(ys)) / 2
    cx, cy = (bx0 + bx1) / 2, (by0 + by1) / 2
    return [[((x - mx) * s + cx, (y - my) * s + cy) for x, y in poly] for poly in bold]


def fit(polys, box) -> list[list[tuple[float, float]]]:
    """等比缩放 + 居中到给定比例框。"""
    xs = [p[0] for poly in polys for p in poly]
    ys = [p[1] for poly in polys for p in poly]
    x0, y0, x1, y1 = min(xs), min(ys), max(xs), max(ys)
    bx0, by0, bx1, by1 = (box[0] * UNIT, box[1] * UNIT, box[2] * UNIT, box[3] * UNIT)
    s = min((bx1 - bx0) / (x1 - x0), (by1 - by0) / (y1 - y0))
    cx, cy = (bx0 + bx1) / 2, (by0 + by1) / 2
    mx, my = (x0 + x1) / 2, (y0 + y1) / 2
    return [[((x - mx) * s + cx, (y - my) * s + cy) for x, y in poly] for poly in polys]


def render(size: int, polys=None, ss: int = 16) -> Image.Image:
    """在该尺寸下原生渲染：ss 倍超采样后按**面积平均**（BOX）降采样。

    BOX 得到的就是每个像素被覆盖的比例，边缘干净；原来用 LANCZOS，它的核比一个像素宽，
    小尺寸下会把笔画边缘抹开一圈、还带一点振铃，正是"糊"的来源之一。
    `polys` 不给就按尺寸取 `polys_for(size)`（含小尺寸光学加粗）。
    """
    if polys is None:
        polys = polys_for(size)
    ss = max(4, min(ss, 4096 // size))      # 大图不需要 16 倍（1024×16 的画布要 256MB）
    big = size * ss
    scale = big / UNIT
    canvas = Image.new("L", (big, big), 0)
    draw = ImageDraw.Draw(canvas)
    for poly in polys:
        pts = [(x * scale, y * scale) for x, y in poly]
        if len(pts) >= 3:
            draw.polygon(pts, fill=255)
    mark = canvas.resize((size, size), Image.BOX)

    tile = Image.new("RGB", (size, size), NAVY)
    mask = Image.new("L", (big, big), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        (0, 0, big - 1, big - 1), radius=int(CORNER_RATIO * big), fill=255
    )
    mask = mask.resize((size, size), Image.BOX)

    out = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    out.paste(tile, (0, 0), mask)
    out.paste(Image.new("RGBA", (size, size), WHITE + (255,)), (0, 0), mark)
    return out


def write_svg(polys, path: Path) -> None:
    r = CORNER_RATIO * UNIT
    body = []
    for poly in polys:
        pts = " ".join(f"{x:.2f},{y:.2f}" for x, y in poly)
        body.append(f'    <polygon points="{pts}" />')
    svg = f"""<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {UNIT:.0f} {UNIT:.0f}" width="512" height="512">
  <title>inklink</title>
  <desc>inklink 产品标记：深蓝圆角方块 + 白色手写体 i（一笔连写，几何重绘）。</desc>
  <rect width="{UNIT:.0f}" height="{UNIT:.0f}" rx="{r:.2f}" ry="{r:.2f}" fill="#062657" />
  <g fill="#FFFFFF">
{chr(10).join(body)}
  </g>
</svg>
"""
    path.write_text(svg, encoding="utf-8")


def _dib(im: Image.Image) -> bytes:
    """把图像编成 ICO 里的老式 DIB 条目（32 位 BGRA + 1 位 AND 掩码）。

    小尺寸用 DIB 而不是 PNG：PNG 压缩条目在部分老解析器/老外壳上只对 256 认，
    16~64 走 DIB 最稳。
    """
    w, h = im.size
    px = im.convert("RGBA").load()
    rows = []
    for y in range(h - 1, -1, -1):          # DIB 是自下而上
        row = bytearray()
        for x in range(w):
            r, g, b, a = px[x, y]
            row += bytes((b, g, r, a))
        rows.append(bytes(row))
    xor = b"".join(rows)

    stride = ((w + 31) // 32) * 4           # AND 掩码按 32 位对齐
    mask_rows = []
    for y in range(h - 1, -1, -1):
        bits = bytearray(stride)
        for x in range(w):
            if px[x, y][3] < 128:           # 全透明处掩码置 1
                bits[x // 8] |= 0x80 >> (x % 8)
        mask_rows.append(bytes(bits))
    and_mask = b"".join(mask_rows)

    header = struct.pack("<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, len(xor) + len(and_mask), 0, 0, 0, 0)
    return header + xor + and_mask


def write_ico(images: list[Image.Image], path: Path) -> None:
    """手写 ICO：每个尺寸放自己的数据（小尺寸 DIB、256 用 PNG），不做位图缩放。"""
    entries: list[tuple[int, int, bytes]] = []
    for im in images:
        if im.width >= 256:
            tmp = path.with_suffix(f".{im.width}.tmp.png")
            im.save(tmp, format="PNG", optimize=True)
            entries.append((im.width, im.height, tmp.read_bytes()))
            tmp.unlink()
        else:
            entries.append((im.width, im.height, _dib(im)))

    header = struct.pack("<HHH", 0, 1, len(entries))
    dir_bytes, offset = b"", 6 + 16 * len(entries)
    for w, h, data in entries:
        dir_bytes += struct.pack("<BBBBHHII", w % 256, h % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    path.write_bytes(header + dir_bytes + b"".join(d for _, _, d in entries))


def main() -> None:
    polys = fit(shape_polygons(), MARK_BOX)
    BRAND_DIR.mkdir(parents=True, exist_ok=True)

    write_svg(polys, BRAND_DIR / "ink-mark.svg")

    for size, name in ((1024, BRAND_DIR / "ink-mark.png"), (512, INSTALLER / "inklink.png")):
        render(size).save(name, format="PNG", optimize=True)

    # 每档按自己的尺寸取轮廓（小尺寸带光学加粗），不从大图缩
    write_ico([render(s) for s in (16, 20, 24, 32, 40, 48, 64, 128, 256)],
              INSTALLER / "inklink.ico")

    if "--preview" in sys.argv:
        # 检查图：各实际尺寸 + 16/24px 放大 8 倍。整张图最后用 NEAREST 放大 2 倍，
        # 看到的就是真实像素（原来用 LANCZOS 放大，预览图自己就把边缘抹糊了）。
        sheet = Image.new("RGB", (860, 340), (0xF2, 0xF2, 0xF4))
        x = 16
        for s in (16, 20, 24, 32, 64, 128):
            im = render(s)
            sheet.paste(im, (x, 274 - s // 2), im)
            x += s + 18
        z = render(16).resize((128, 128), Image.NEAREST)
        sheet.paste(z, (16, 16), z)
        z = render(24).resize((192, 192), Image.NEAREST)
        sheet.paste(z, (160, 16), z)
        z2 = render(280)
        sheet.paste(z2, (560, 30), z2)
        sheet = sheet.resize((1720, 680), Image.NEAREST)
        UI_DIR.mkdir(parents=True, exist_ok=True)
        sheet.save(UI_DIR / "logo-check.png")

    print("svg  ", (BRAND_DIR / "ink-mark.svg").stat().st_size)
    print("png  ", (INSTALLER / "inklink.png").stat().st_size)
    print("ico  ", (INSTALLER / "inklink.ico").stat().st_size)


if __name__ == "__main__":
    main()
