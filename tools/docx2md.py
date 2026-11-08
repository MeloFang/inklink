"""把 docx 转成 Markdown（段落 / 标题 / 表格），原件挪到 _docx原件\\。

    python tools\\docx2md.py "E:\\Obsidian\\News Releases\\历史稿件"

为什么转：Obsidian 里不能直接预览 docx，转成 md 才能双链、搜索、改稿；
语料扫描两种格式都吃，但 md 更好维护。
"""
import os
import shutil
import sys

import docx


def cell_text(cell):
    return " ".join(p.text.strip() for p in cell.paragraphs if p.text.strip())


def convert(path):
    d = docx.Document(path)
    out = []
    for p in d.paragraphs:
        t = p.text.strip()
        if not t:
            continue
        style = (p.style.name or "").lower()
        if style.startswith("heading"):
            try:
                level = int(style.split()[-1])
            except ValueError:
                level = 2
            level = max(1, min(6, level))
            out.append("#" * level + " " + t)
        elif style.startswith("title"):
            out.append("# " + t)
        else:
            out.append(t)
        out.append("")
    for tbl in d.tables:
        rows = []
        for row in tbl.rows:
            rows.append([cell_text(c) for c in row.cells])
        if not rows:
            continue
        width = max(len(r) for r in rows)
        rows = [r + [""] * (width - len(r)) for r in rows]
        out.append("| " + " | ".join(rows[0]) + " |")
        out.append("|" + "|".join(["---"] * width) + "|")
        for r in rows[1:]:
            out.append("| " + " | ".join(r) + " |")
        out.append("")
    return "\n".join(out).strip() + "\n"


def main():
    root = sys.argv[1] if len(sys.argv) > 1 else r"E:\Obsidian\News Releases\历史稿件"
    archive = os.path.join(os.path.dirname(root), "_docx原件")
    done = failed = 0
    for dirpath, _dirnames, files in os.walk(root):
        for f in sorted(files):
            if not f.lower().endswith(".docx") or f.startswith("~$"):
                continue
            src = os.path.join(dirpath, f)
            rel_dir = os.path.relpath(dirpath, root)
            md_path = os.path.join(dirpath, os.path.splitext(f)[0] + ".md")
            try:
                md = convert(src)
            except Exception as e:
                print("FAIL", f, e)
                failed += 1
                continue
            with open(md_path, "w", encoding="utf-8", newline="\n") as fp:
                fp.write(md)
            dst_dir = os.path.join(archive, rel_dir) if rel_dir != "." else archive
            os.makedirs(dst_dir, exist_ok=True)
            shutil.move(src, os.path.join(dst_dir, f))
            done += 1
            print("ok  %s -> %s" % (f, os.path.basename(md_path)))
    print("\n转换 %d 个，失败 %d 个；原件在 %s" % (done, failed, archive))


if __name__ == "__main__":
    main()
