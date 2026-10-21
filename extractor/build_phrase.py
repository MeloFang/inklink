# -*- coding: utf-8 -*-
"""build_phrase.py — terms.json -> Rime custom_phrase.txt

输出格式（每行）：
  短语<TAB>拼音(空格分隔音节)<TAB>权重

Rime 的 custom_phrase 表翻译器靠拼音列做编码匹配，所以拼音列是必须的。
用法：
  python extractor/build_phrase.py
  python extractor/build_phrase.py terms.json output/custom_phrase.txt
"""
import os, sys, json

try:
    from pypinyin import pinyin, Style
except ImportError:
    print("需要 pypinyin：pip install pypinyin")
    sys.exit(1)

try:
    import yaml
    HAVE_YAML = True
except ImportError:
    HAVE_YAML = False

# 通用停用词 / 虚词，避免把"的/了/我们"之类塞进词库
STOP = set(
    "的 了 和 是 在 我 有 他 这 中 大 来 上 个 到 时 要 就 下 得 说 们 为 子 也 你 对 "
    "之 与 及 或 等 该 各 其 此 那 一个 可以 这样 我们 他们 这个 那个 什么 怎么 进行 通过".split()
)


def load_config():
    cands = [
        os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'config.yaml'),
        'config.yaml',
    ]
    for c in cands:
        if HAVE_YAML and os.path.exists(c):
            return yaml.safe_load(open(c, encoding='utf-8')) or {}
    return {}


def to_pinyin(text):
    return ' '.join(s[0] for s in pinyin(text, style=Style.NORMAL, heteronym=False))


def suppress_substrings(terms):
    """丢弃「是某个更长且权重更高词条的子串」的碎片（如 光纤/纤入 之于 光纤入户）。

    按 长度降序、权重降序 处理：先保留长词，再判断短词是否被已保留长词包含且权重不更高，
    是则丢弃。这样高频独立短词（权重反超长词）仍会保留。
    """
    items = sorted(terms.items(), key=lambda x: (-len(x[0]), -x[1]['weight']))
    kept = []
    result = {}
    for term, info in items:
        bad = False
        for kterm, kinfo in kept:
            if len(kterm) > len(term) and term in kterm and kinfo['weight'] >= info['weight']:
                bad = True
                break
        if bad:
            continue
        kept.append((term, info))
        result[term] = info
    return result


def build(terms_path, out_path, min_weight, min_len, max_len):
    raw = json.load(open(terms_path, encoding='utf-8'))
    terms = suppress_substrings(raw)
    rows = []
    for term, info in terms.items():
        if info['weight'] < min_weight:
            continue
        if len(term) < min_len or len(term) > max_len:
            continue
        if term in STOP:
            continue
        if any(ch.isspace() for ch in term):
            continue
        rows.append((info['weight'], term, to_pinyin(term)))
    rows.sort(reverse=True)
    os.makedirs(os.path.dirname(out_path) or '.', exist_ok=True)
    with open(out_path, 'w', encoding='utf-8') as f:
        for w, term, py in rows:
            f.write(f"{term}\t{py}\t{w}\n")
    print("写出词条:", len(rows), "->", os.path.abspath(out_path))
    for w, t, p in rows[:15]:
        print(f"  {w:>4}  {t}\t{p}")


if __name__ == '__main__':
    cfg = load_config()
    base = os.path.dirname(os.path.abspath(__file__))
    terms_path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(base, 'terms.json')
    out_path = sys.argv[2] if len(sys.argv) > 2 else cfg.get(
        'output_path', os.path.join(base, '..', 'output', 'custom_phrase.txt'))
    build(terms_path, out_path,
          cfg.get('min_weight', 4), cfg.get('min_len', 2), cfg.get('max_len', 12))
