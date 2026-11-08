"""从稿件里提炼"术语候选"，生成一张供人工过目的表。

    python tools\\term-candidates.py "E:\\Obsidian\\News Releases\\历史稿件"

产出：E:\\Obsidian\\News Releases\\_术语候选.md（表格：词 / 次数 / 篇数 / 建议）

为什么要有这一步：术语表里的词权重最高（`## 词` = 10 分），
手打容易漏；全自动塞进去又会混进"推进/落实"这种通用词。所以先出候选，
人工删掉不要的，再把留下的写进术语表。
"""
import collections
import io
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BASE_DICT = os.path.join(ROOT, 'data', 'upstream', 'ice', 'base.dict.yaml')
KNOWN_WEIGHT = 1000          # 基础词库里权重 >= 这个值 = 太常用，不进术语表
MIN_COUNT = 3                # 至少出现 3 次
MIN_FILES = 2                # 至少出现在 2 篇文章里
CJK_MIN, CJK_MAX = 2, 6


def is_cjk(ch):
    return '\u4e00' <= ch <= '\u9fff'


def load_common(path, threshold):
    """基础词库里足够常用的词 —— 这些不该进术语表。"""
    out = set()
    if not os.path.exists(path):
        return out
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
                out.add(parts[0])
    return out


def read_text(path):
    low = path.lower()
    if low.endswith('.docx'):
        try:
            import docx
        except ImportError:
            return ''
        try:
            d = docx.Document(path)
        except Exception:
            return ''
        parts = [p.text for p in d.paragraphs]
        for t in d.tables:
            for row in t.rows:
                for cell in row.cells:
                    parts.append(cell.text)
        return '\n'.join(parts)
    try:
        return io.open(path, encoding='utf-8', errors='ignore').read()
    except Exception:
        return ''


def main():
    vault = sys.argv[1] if len(sys.argv) > 1 else r'E:\Obsidian\News Releases\历史稿件'
    out_path = sys.argv[2] if len(sys.argv) > 2 else r'E:\Obsidian\News Releases\_术语候选.md'

    files = []
    for dirpath, _d, names in os.walk(vault):
        if '_docx原件' in dirpath:
            continue
        for n in names:
            if n.lower().endswith(('.md', '.docx')) and not n.startswith('~$'):
                files.append(os.path.join(dirpath, n))
    files.sort()

    count = collections.Counter()
    per_file = collections.defaultdict(set)
    for p in files:
        text = read_text(p)
        if not text:
            continue
        han = ''.join(ch for ch in text if is_cjk(ch))
        for n in range(CJK_MIN, CJK_MAX + 1):
            for i in range(len(han) - n + 1):
                term = han[i:i + n]
                count[term] += 1
                per_file[term].add(p)

    common = load_common(BASE_DICT, KNOWN_WEIGHT)
    cand = {t: c for t, c in count.items()
            if c >= MIN_COUNT and len(per_file[t]) >= MIN_FILES and t not in common}

    # 碎片：被更长的候选包含、且次数不比它少
    longer = collections.defaultdict(int)
    for t, c in cand.items():
        n = len(t)
        if n < 3:
            continue
        for i in range(n):
            for j in range(i + 2, n + 1):
                if j - i == n:
                    continue
                longer[t[i:j]] = max(longer[t[i:j]], c)
    cand = {t: c for t, c in cand.items() if longer.get(t, -1) < c}

    rows = sorted(cand.items(), key=lambda kv: (-kv[1], -len(per_file[kv[0]]), kv[0]))[:300]

    lines = [
        '# 术语候选',
        '',
        '> 从 %d 篇文章里自动挑的（出现 >= %d 次、跨 >= %d 篇、非通用词）。'
        % (len(files), MIN_COUNT, MIN_FILES),
        '> **把不要的整行删掉**，剩下的会写进 `术语表.md`（`## 词` 形式，权重最高）。',
        '',
        '| 词 | 次数 | 篇数 | 建议 |',
        '|---|---|---|---|',
    ]
    for t, c in rows:
        lines.append('| %s | %d | %d | 保留 |' % (t, c, len(per_file[t])))
    lines.append('')
    io.open(out_path, 'w', encoding='utf-8', newline='\n').write('\n'.join(lines))
    print('扫了 %d 篇，候选 %d 条 -> %s' % (len(files), len(rows), out_path))


if __name__ == '__main__':
    main()
