# -*- coding: utf-8 -*-
"""生成 inklink 的输入法图标（多尺寸 .ico）。

用法：
    python tools/make-icon.py

产出：
    installer/inklink.ico   （安装时复制到 C:\\Program Files\\inklink\\inklink.ico）
    installer/inklink.png   （512×512，文档/商店用）

设计：品牌橙圆角方块 + 白色「墨」。为什么是「墨」——inklink 的 ink 就是墨，
而且输入法列表里微软是「拼」、搜狗是「S」，一个汉字在小尺寸下最好认。

小尺寸（<24px）换成白色「i」：16/20px 上「墨」会糊成一块橙色，语言栏里认不出来。
这是唯一一处按尺寸换字形的地方，其余尺寸保持同一套品牌。
"""
import os
from PIL import Image, ImageDraw, ImageFont

# 品牌色：和候选窗高亮同色（#FF6600）
BRAND = (0xFF, 0x66, 0x00, 0xFF)
WHITE = (0xFF, 0xFF, 0xFF, 0xFF)

GLYPH = "墨"
SMALL_GLYPH = "i"      # < 24px 用这个
SMALL_LIMIT = 24
FONT_CANDIDATES = [
    r"C:\Windows\Fonts\msyhbd.ttc",   # 微软雅黑 Bold
    r"C:\Windows\Fonts\msyh.ttc",     # 微软雅黑
    r"C:\Windows\Fonts\simhei.ttf",   # 黑体
]


def load_font(size: int) -> ImageFont.FreeTypeFont:
    for path in FONT_CANDIDATES:
        if os.path.exists(path):
            try:
                return ImageFont.truetype(path, size)
            except OSError:
                continue
    raise SystemExit("找不到可用的中文字体（微软雅黑/黑体）")


def render(size: int = 1024, glyph: str = GLYPH, ratio: float = 0.62) -> Image.Image:
    """按 size 画，最后用 LANCZOS 缩小，边缘才干净。"""
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # 圆角方块：留 4% 边距，圆角半径 22%
    pad = int(size * 0.04)
    radius = int(size * 0.22)
    d.rounded_rectangle([pad, pad, size - pad, size - pad], radius=radius, fill=BRAND)

    # 居中：用字形的实际包围盒（汉字基线不等于视觉中心）
    font = load_font(int(size * ratio))
    box = d.textbbox((0, 0), glyph, font=font)
    tw, th = box[2] - box[0], box[3] - box[1]
    x = (size - tw) / 2 - box[0]
    y = (size - th) / 2 - box[1]
    d.text((x, y), glyph, font=font, fill=WHITE)
    return img


def main() -> None:
    here = os.path.dirname(os.path.abspath(__file__))
    out_dir = os.path.join(os.path.dirname(here), "installer")
    os.makedirs(out_dir, exist_ok=True)

    master = render(1024)
    master.resize((512, 512), Image.LANCZOS).save(os.path.join(out_dir, "inklink.png"))

    # Windows 需要的尺寸：16/20/24/32/40/48/64/256
    sizes = [16, 20, 24, 32, 40, 48, 64, 256]
    frames = []
    for s in sizes:
        if s < SMALL_LIMIT:
            frames.append(render(1024, SMALL_GLYPH, 0.72).resize((s, s), Image.LANCZOS))
        else:
            frames.append(master.resize((s, s), Image.LANCZOS))

    ico_path = os.path.join(out_dir, "inklink.ico")
    frames[-1].save(ico_path, format="ICO", sizes=[(s, s) for s in sizes], append_images=frames[:-1])

    for s, frame in zip(sizes, frames):
        frame.save(os.path.join(out_dir, f"preview-{s}.png"))

    print(f"已生成 {ico_path}")
    print(f"已生成 {os.path.join(out_dir, 'inklink.png')}")


if __name__ == "__main__":
    main()
