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
import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_DICT = os.path.join(ROOT, 'data', 'upstream', 'ice', '8105.dict.yaml')


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
            if ch not in best or w > best[ch][0]:
                best[ch] = (w, py)
    return {k: v[1] for k, v in best.items()}


def is_cjk(ch):
    return '\u4e00' <= ch <= '\u9fff'


def build(terms, pinyin, max_terms):
    rows = []
    skipped_no_pinyin = 0
    for term, info in terms.items():
        t = term.strip()
        # 只收 2~8 个汉字：太短没意义，太长是整句（那属于 AI 的活，不是候选）
        if not (2 <= len(t) <= 8) or not all(is_cjk(c) for c in t):
            continue
        syl = []
        for c in t:
            p = pinyin.get(c)
            if not p:
                break
            syl.append(p)
        else:
            weight = int(info.get('weight', 1) or 1)
            rows.append((' '.join(syl), t, weight))
            continue
        skipped_no_pinyin += 1
    seen = set()
    uniq = []
    for r in rows:
        key = (r[0], r[1])
        if key in seen:
            continue
        seen.add(key)
        uniq.append(r)
    uniq.sort(key=lambda r: (-r[2], r[1]))
    return uniq[:max_terms], skipped_no_pinyin


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('-Terms', '--terms', default=os.path.join(ROOT, 'extractor', 'terms.json'))
    ap.add_argument('-Out', '--out', default=os.path.join(
        os.environ.get('LOCALAPPDATA', '.'), 'inklink', 'corpus.tsv'))
    ap.add_argument('-Max', '--max', type=int, default=2000)
    ap.add_argument('-Dict', '--dict', default=DEFAULT_DICT)
    args = ap.parse_args()

    if not os.path.exists(args.terms):
        sys.exit('找不到 terms.json：%s（先跑 tools\\import-corpus.ps1 或 scan_vault.py）' % args.terms)
    terms = json.load(io.open(args.terms, encoding='utf-8'))
    pinyin = load_pinyin(args.dict)
    rows, skipped = build(terms, pinyin, args.max)

    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    with io.open(args.out, 'w', encoding='utf-8', newline='\n') as f:
        for code, word, weight in rows:
            f.write('%s\t%s\t%d\n' % (code, word, weight))

    print('terms.json 词条：%d' % len(terms))
    print('写进语料表：%d 条（上限 %d）' % (len(rows), args.max))
    if skipped:
        print('跳过（字表里没有读音）：%d' % skipped)
    for code, word, weight in rows[:8]:
        print('  %-18s %s  (w=%d)' % (code, word, weight))


if __name__ == '__main__':
    main()
