"""terms.json -> corpus.tsv（个人语料进候选用的表）。

    python extractor/build_corpus_tsv.py -Terms extractor/terms.json -Out %LOCALAPPDATA%\\inklink\\corpus.tsv

输出格式（UTF-8 无 BOM，制表符分隔）：`码 \t 词 \t 权重`
- 码是**空格分音节**的拼音（`chi tu`），引擎查之前会把空格去掉，和用户输入的整串对上
- 权重来自 scan_vault.py 的 weight（标题/双链/标签/粗体/重复都算）

为什么要在抽取侧做字→拼音：引擎只认 librime 的切分，它不知道"赤"读 chi；
8105 字表是现成的真相源（和 rime-ice 同一个源），多音字取权重最高的那个读音。
"""

import argparse
import io
import itertools
import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_DICT = os.path.join(ROOT, 'data', 'upstream', 'ice', '8105.dict.yaml')
# rime-ice 基础词库：里面已经有的常用词不用再塞进语料（打了本来就在前几页）
BASE_DICT = os.path.join(ROOT, 'data', 'upstream', 'ice', 'base.dict.yaml')
# 基础词库里权重 >= 这个值的算"已经够常用"，语料表里跳过
KNOWN_WEIGHT = 1000


def load_pinyin(path):
    """8105 字表 -> {字: 拼音}。多音字取权重最高的那个读音。"""
    best = {}
    body = False
    with io.open(path, encoding='utf-8') as f:
        for line in f:
            s = line.rstrip('\n')
            if not body:
                if s.strip() == '...':
                    body = True
                continue
            if not s or s.startswith('#'):
                continue
            parts = s.split('\t')
            if len(parts) < 2:
                continue
            ch = parts[0]
            py = parts[1].split()[0] if parts[1].strip() else ''
            if not py:
                continue
            try:
                w = int(parts[2]) if len(parts) > 2 else 0
            except ValueError:
                w = 0
            # 多音字：每个读音都留（按权重降序），一个词要按组合登记多条
            lst = best.setdefault(ch, [])
            if py not in [x[1] for x in lst]:
                lst.append((w, py))
            lst.sort(reverse=True)
    return {k: [py for _w, py in v[:3]] for k, v in best.items()}


def load_all_words(path):
    """基础词库里的全部词（用来判断"是不是真词"）。"""
    words = set()
    if not os.path.exists(path):
        return words
    body = False
    with io.open(path, encoding='utf-8', errors='ignore') as f:
        for line in f:
            s = line.rstrip('\n')
            if not body:
                if s.strip() == '...':
                    body = True
                continue
            if not s or s.startswith('#'):
                continue
            parts = s.split('\t')
            if len(parts) >= 2:
                words.add(parts[0])
    return words


def load_known_words(path, threshold):
    """基础词库里权重 >= threshold 的词（这些词 librime 本来就给得出来）。"""
    known = set()
    if not os.path.exists(path):
        return known
    body = False
    with io.open(path, encoding='utf-8', errors='ignore') as f:
        for line in f:
            s = line.rstrip('\n')
            if not body:
                if s.strip() == '...':
                    body = True
                continue
            if not s or s.startswith('#'):
                continue
            parts = s.split('\t')
            if len(parts) < 3:
                continue
            try:
                w = int(parts[2])
            except ValueError:
                continue
            if w >= threshold:
                known.add(parts[0])
    return known


def is_cjk(ch):
    return '\u4e00' <= ch <= '\u9fff'


def build(terms, pinyin, max_terms, known=None, all_words=None):
    known = known or set()
    all_words = all_words or set()
    skipped_fragment = 0
    rows = []
    skipped_known = 0
    skipped_no_pinyin = 0
    for term, info in terms.items():
        t = term.strip()
        # 只收 2~8 个汉字：太短没意义，太长是整句（那属于 AI 的活，不是候选）
        if not (2 <= len(t) <= 8) or not all(is_cjk(c) for c in t):
            continue
        if t in known:
            skipped_known += 1
            continue
        choices = []
        for c in t:
            rs = pinyin.get(c) or []
            if not rs:
                break
            choices.append(rs[:2])  # 每个字最多两种读音
        else:
            weight = int(info.get('weight', 1) or 1)
            total = 1
            for rs in choices:
                total *= len(rs)
            if total > 6:
                variants = [tuple(rs[0] for rs in choices)]
            else:
                variants = list(itertools.product(*choices))
            seen_codes = set()
            for combo in variants:
                code = ' '.join(combo)
                if code in seen_codes:
                    continue
                seen_codes.add(code)
                rows.append((code, t, weight))
            continue
        skipped_no_pinyin += 1
    # 去掉"碎片"：`嘉兴分`、`兴分` 这种是长词（`嘉兴分公司`）的滑窗产物。
    # 规则：只要有一个更长的词包含它、且那个词的出现次数不比它少，它就是碎片。
    # 做法：把所有长词的子串记一份"被包含时的最大权重"，再回头筛（避免 O(n^2)）。
    containing_max = {}
    for _code, word, weight in rows:
        n = len(word)
        if n < 3:
            continue
        for i in range(n):
            for j in range(i + 2, n + 1):
                if j - i == n:
                    continue  # 整词自己不算
                sub = word[i:j]
                if len(sub) < 2:
                    continue
                prev = containing_max.get(sub)
                if prev is None or weight > prev:
                    containing_max[sub] = weight
    drop = {w for _c, w, wt in rows if containing_max.get(w, -1) >= wt}
    rows = [r for r in rows if r[1] not in drop]

    seen = set()
    uniq = []
    for r in rows:
        key = (r[0], r[1])
        if key in seen:
            continue
        seen.add(key)
        uniq.append(r)
    uniq.sort(key=lambda r: (-r[2], r[1]))
    dropped_fragment = len(drop)
    return uniq[:max_terms], skipped_no_pinyin, skipped_known, dropped_fragment + skipped_fragment


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('-Terms', '--terms', default=os.path.join(ROOT, 'extractor', 'terms.json'))
    ap.add_argument('-Out', '--out', default=os.path.join(
        os.environ.get('LOCALAPPDATA', '.'), 'inklink', 'corpus.tsv'))
    ap.add_argument('-Max', '--max', type=int, default=5000)
    ap.add_argument('-Dict', '--dict', default=DEFAULT_DICT)
    args = ap.parse_args()

    if not os.path.exists(args.terms):
        sys.exit('找不到 terms.json：%s（先跑 tools\\import-corpus.ps1 或 scan_vault.py）' % args.terms)
    terms = json.load(io.open(args.terms, encoding='utf-8'))
    pinyin = load_pinyin(args.dict)
    known = load_known_words(BASE_DICT, KNOWN_WEIGHT)
    all_words = load_all_words(BASE_DICT)
    rows, skipped, skipped_known, fragments = build(terms, pinyin, args.max, known, all_words)

    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    with io.open(args.out, 'w', encoding='utf-8', newline='\n') as f:
        for code, word, weight in rows:
            f.write('%s\t%s\t%d\n' % (code, word, weight))

    print('terms.json 词条：%d' % len(terms))
    print('写进语料表：%d 条（上限 %d）' % (len(rows), args.max))
    if skipped:
        print('跳过（字表里没有读音）：%d' % skipped)
    if skipped_known:
        print('跳过（基础词库已够常用）：%d' % skipped_known)
    if fragments:
        print('跳过（是长词的碎片）：%d' % fragments)
    for code, word, weight in rows[:8]:
        print('  %-18s %s  (w=%d)' % (code, word, weight))


if __name__ == '__main__':
    main()
