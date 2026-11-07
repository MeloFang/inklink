# -*- coding: utf-8 -*-
"""scan_vault.py — 扫描 Markdown / Obsidian 仓库，抽取个人术语词库 -> terms.json

提取来源与权重：
  一级~六级标题      权重 10
  [[双链]]           权重 8
  #标签              权重 6
  **粗体术语**       权重 4
  2~4 字中文 n-gram  每出现 1 次权重 +1（高频片段自然上浮）

支持增量：按文件 mtime + size 缓存到 .cache/vault_cache.json，
仅重算变动文件，删除的文件自动从缓存剔除。

用法：
  python extractor/scan_vault.py                # 读 config.yaml 的 vault_path
  python extractor/scan_vault.py "D:/MyVault"   # 指定仓库路径
"""
import os, re, json, sys

try:
    import yaml
    HAVE_YAML = True
except ImportError:
    HAVE_YAML = False

CJK = re.compile(r'[\u4e00-\u9fff]')
TAG = re.compile(r'(?:^|[\s（(])#([A-Za-z0-9_\u4e00-\u9fff\-]+)')
WIKILINK = re.compile(r'\[\[([^\]\|]+)(?:\|[^\]]+)?\]\]')
BOLD = re.compile(r'\*\*(.+?)\*\*')
HEADING = re.compile(r'^(#{1,6})\s+(.+?)\s*#*$')


def load_config():
    cands = [
        os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', 'config.yaml'),
        'config.yaml',
    ]
    for c in cands:
        if HAVE_YAML and os.path.exists(c):
            return yaml.safe_load(open(c, encoding='utf-8')) or {}
    return {}


def extract(text, rel):
    terms = {}

    def add(term, w):
        term = term.strip().strip('#').strip()
        if not term or not CJK.search(term):
            return
        if any(ch.isspace() for ch in term):   # 含空白的整句（如长标题）不入库
            return
        d = terms.setdefault(term, {"weight": 0, "freq": 0, "files": set()})
        d["weight"] += w
        d["freq"] += 1
        d["files"].add(rel)

    for line in text.splitlines():
        m = HEADING.match(line)
        if m:
            add(m.group(2), 10)
            continue
        for t in TAG.findall(line):
            add(t, 6)
        for w in WIKILINK.findall(line):
            add(w.split('/')[-1].strip(), 8)
        for b in BOLD.findall(line):
            add(b, 4)
        han = ''.join(CJK.findall(line))
        # 2~6 字：5、6 字是公司里的长词（嘉兴分公司、降本增效专项），
        # 只出 2~4 字的话长词会被切成碎片（兴分、嘉兴分），反而更脏
        for n in (2, 3, 4, 5, 6):
            for i in range(len(han) - n + 1):
                add(han[i:i + n], 1)
    return terms


def read_docx(path):
    """Word 文档 -> 纯文本（段落 + 表格单元格）。没装 python-docx 就跳过。"""
    try:
        import docx
    except ImportError:
        print('skip (no python-docx):', path)
        return ''
    try:
        d = docx.Document(path)
    except Exception as e:
        print('skip', path, e)
        return ''
    parts = [p.text for p in d.paragraphs]
    for t in d.tables:
        for row in t.rows:
            for cell in row.cells:
                parts.append(cell.text)
    return '\n'.join(parts)


def scan(vault, cache_path):
    cache = {}
    if os.path.exists(cache_path):
        try:
            cache = json.load(open(cache_path, encoding='utf-8'))
        except Exception:
            cache = {}
    agg = {}
    seen = set()
    for root, _, files in os.walk(vault):
        for f in files:
            low = f.lower()
            p = os.path.join(root, f)
            if low.endswith('.docx'):
                text = read_docx(p)
                if not text.strip():
                    continue
            elif low.endswith(('.md', '.markdown', '.txt')):
                try:
                    text = open(p, encoding='utf-8', errors='ignore').read()
                except Exception as e:
                    print("skip", p, e)
                    continue
            else:
                continue
            rel = os.path.relpath(p, vault)
            try:
                st = os.stat(p)
            except OSError:
                continue
            sig = {"m": st.st_mtime, "s": st.st_size}
            old = cache.get(rel)
            if old and old.get('m') == sig['m'] and old.get('s') == sig['s'] and 't' in old:
                for term, info in old['t'].items():
                    d = agg.setdefault(term, {"weight": 0, "freq": 0, "files": set()})
                    d["weight"] += info["weight"]
                    d["freq"] += info["freq"]
                    d["files"].update(info.get("files", []))
                seen.add(rel)
                continue
            t = extract(text, rel)
            cache[rel] = {
                "m": sig['m'], "s": sig['s'],
                "t": {k: {"weight": v["weight"], "freq": v["freq"], "files": list(v["files"])}
                      for k, v in t.items()},
            }
            seen.add(rel)
            for term, info in t.items():
                d = agg.setdefault(term, {"weight": 0, "freq": 0, "files": set()})
                d["weight"] += info["weight"]
                d["freq"] += info["freq"]
                d["files"].update(info["files"])
    for k in list(cache.keys()):
        if k not in seen:
            del cache[k]
    out = {k: {"weight": v["weight"], "freq": v["freq"], "files": sorted(v["files"])}
           for k, v in agg.items()}
    os.makedirs(os.path.dirname(cache_path), exist_ok=True)
    json.dump(cache, open(cache_path, 'w', encoding='utf-8'), ensure_ascii=False)
    return out


if __name__ == '__main__':
    cfg = load_config()
    vault = sys.argv[1] if len(sys.argv) > 1 else cfg.get('vault_path', '.')
    base = os.path.dirname(os.path.abspath(__file__))
    cache_path = os.path.join(base, '.cache', 'vault_cache.json')
    terms = scan(vault, cache_path)
    outp = os.path.join(base, 'terms.json')
    json.dump(terms, open(outp, 'w', encoding='utf-8'), ensure_ascii=False, indent=1)
    print("vault:", os.path.abspath(vault))
    print("提取术语数:", len(terms))
    for t, i in sorted(terms.items(), key=lambda x: -x[1]['weight'])[:20]:
        print(f"  {i['weight']:>4} x{i['freq']:<3} {t}")
