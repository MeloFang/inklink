#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""把 rime-ice 词表转成 inklink 的基础词库（TSV + manifest.json）。

输入（均已下载到 data/upstream/ice/，不进 git）：
  8105.dict.yaml   《通用规范汉字表》字表，约 8.8 千行
  base.dict.yaml   大词库，约 54.3 万行，按权重裁剪

输出：
  data/base.tsv        三列 TSV：词 \t 拼音(空格分音节) \t 权重
  data/manifest.json   来源、条数、许可、源文件哈希

设计要点：
- 输出必须可复现：同样的输入两次跑出逐字节相同的文件。
  所以按 (权重降序, 词, 拼音) 全序排序，manifest 里不放时间戳。
- 同一 (词, 拼音) 只保留一次，取最大权重（8105 与 base 有重叠）。
- 权重阈值可调，默认 400：约 29 万条，启动加载约 1.5s（覆盖成语/网络词）。

用法：
  python tools/build_base_lexicon.py
  python tools/build_base_lexicon.py --min-weight 5000
"""
from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ICE = ROOT / "data" / "upstream" / "ice"
OUT_TSV = ROOT / "data" / "base.tsv"
OUT_MANIFEST = ROOT / "data" / "manifest.json"

SOURCES = [
    ("8105.dict.yaml", "《通用规范汉字表》8105 字字表"),
    ("base.dict.yaml", "rime-ice 基础词库"),
]


def parse_dict_text(text: str) -> list[tuple[str, str, int]]:
    """解析 Rime .dict.yaml 文本的表体，返回 (词, 拼音, 权重)。

    规则：表体从单独一行 `...` 之后开始；`#` 开头为注释；
    列数不足 3、词或拼音为空、权重非数字的行一律跳过。
    找不到表体标记时抛 ValueError。
    """
    lines = text.splitlines()
    try:
        start = next(i for i, line in enumerate(lines) if line.strip() == "...") + 1
    except StopIteration:
        raise ValueError("找不到表体起始标记 '...'") from None

    rows: list[tuple[str, str, int]] = []
    for line in lines[start:]:
        if not line.strip() or line.startswith("#"):
            continue
        fields = line.split("\t")
        if len(fields) < 3:
            continue
        word, pinyin, weight = fields[0].strip(), fields[1].strip(), fields[2].strip()
        if not word or not pinyin:
            continue
        try:
            rows.append((word, pinyin, int(weight)))
        except ValueError:
            continue
    return rows


def parse_dict(path: Path) -> list[tuple[str, str, int]]:
    """解析 .dict.yaml 文件（薄封装，便于单测纯函数）。"""
    return parse_dict_text(path.read_text(encoding="utf-8"))


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--min-weight", type=int, default=10000,
                    help="base 词库保留的最小权重（默认 10000）")
    args = ap.parse_args()

    missing = [name for name, _ in SOURCES if not (ICE / name).is_file()]
    if missing:
        print(f"缺少上游文件：{missing}\n请先下载到 {ICE}", file=sys.stderr)
        return 1

    merged: dict[tuple[str, str], int] = {}
    stats = {}
    for name, _desc in SOURCES:
        try:
            rows = parse_dict(ICE / name)
        except ValueError as e:
            print(f"{name}: {e}", file=sys.stderr)
            return 1
        kept = 0
        for word, pinyin, w in rows:
            if w < args.min_weight:
                continue
            key = (word, pinyin)
            if merged.get(key, -1) < w:
                merged[key] = w
            kept += 1
        stats[name] = {"parsed": len(rows), "kept": kept}
        print(f"{name}: 解析 {len(rows)} 条，通过阈值 {kept} 条")

    items = sorted(merged.items(), key=lambda kv: (-kv[1], kv[0][0], kv[0][1]))

    OUT_TSV.parent.mkdir(parents=True, exist_ok=True)
    with OUT_TSV.open("w", encoding="utf-8", newline="\n") as f:
        for (word, pinyin), w in items:
            f.write(f"{word}\t{pinyin}\t{w}\n")

    manifest = {
        "name": "inklink-base",
        "version": 1,
        "entries": len(items),
        "min_weight": args.min_weight,
        "license": "GPL-3.0",
        "license_note": "衍生自 rime-ice（GPL-3.0），本数据文件同样以 GPL-3.0 分发",
        "sources": [
            {
                "file": name,
                "desc": desc,
                "url": f"https://github.com/iDvel/rime-ice/blob/main/cn_dicts/{name}",
                "sha256": sha256(ICE / name),
                "parsed": stats[name]["parsed"],
                "kept": stats[name]["kept"],
            }
            for name, desc in SOURCES
        ],
    }
    OUT_MANIFEST.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )

    print(f"写出 {len(items)} 条 -> {OUT_TSV}")
    print(f"写出 manifest -> {OUT_MANIFEST}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
