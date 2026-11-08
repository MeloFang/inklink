//! 笔记目录 → 语料表（`corpus.tsv`）的转换器。
//!
//! 为什么放在设置窗里：引擎是 C++，只认 TSV；用户不该为了"用上自己的词"
//! 去装 Python 跑脚本。所以把 `extractor/scan_vault.py` + `build_corpus_tsv.py`
//! 的逻辑搬到这里，设置窗点保存时顺带生成。
//!
//! 规则和 Python 版保持一致：
//! - 只收 2~4 个连续汉字（5、6 字的滑窗几乎全是碎片）
//! - 权重：标题 10 / 双链 8 / 标签 6 / 加粗 4 / 正文 1
//! - 跳过"被更长词包含且次数不更多"的碎片
//! - 一个词按多音字的所有读音组合各登记一条（`熟客通` shu/shou 都能打）

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::Path;

const MAX_TERMS: usize = 5000;

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

/// 字 -> 读音（最多两个，按常用度排序）。
fn load_pinyin(exe_dir: &Path) -> HashMap<char, Vec<String>> {
    let mut map = HashMap::new();
    for name in ["pinyin.tsv", "inklink-pinyin.tsv"] {
        let p = exe_dir.join(name);
        let Ok(text) = fs::read_to_string(&p) else {
            continue;
        };
        for line in text.lines() {
            let mut it = line.split('\t');
            let (Some(ch), Some(py)) = (it.next(), it.next()) else {
                continue;
            };
            let Some(c) = ch.chars().next() else { continue };
            let list: Vec<String> = py.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
            if !list.is_empty() {
                map.insert(c, list);
            }
        }
        if !map.is_empty() {
            return map;
        }
    }
    map
}

/// 一篇文章里的词 -> 权重。
fn scan_text(text: &str, out: &mut HashMap<String, u32>) {
    for line in text.lines() {
        let trim = line.trim();
        // 标题：`## 词`
        if let Some(rest) = trim.strip_prefix('#') {
            let t = rest.trim_start_matches('#').trim();
            add(t, 10, out);
            continue;
        }
        // 加粗：`**词**`
        for part in trim.split("**").skip(1).step_by(2) {
            add(part.trim(), 4, out);
        }
        // 双链：`[[词]]`
        let mut s = trim;
        while let Some(i) = s.find("[[") {
            let rest = &s[i + 2..];
            let Some(j) = rest.find("]]") else { break };
            let inner = &rest[..j];
            let name = inner.rsplit('/').next().unwrap_or(inner).trim();
            add(name, 8, out);
            s = &rest[j + 2..];
        }
        // 标签：`#词`
        for tok in trim.split(|c: char| c.is_whitespace()) {
            if let Some(t) = tok.strip_prefix('#') {
                add(t, 6, out);
            }
        }
        // 正文：连续汉字的 2~4 字滑窗
        let han: Vec<char> = trim.chars().filter(|c| is_cjk(*c)).collect();
        for n in 2..=4 {
            if han.len() < n {
                break;
            }
            for w in han.windows(n) {
                let term: String = w.iter().collect();
                *out.entry(term).or_insert(0) += 1;
            }
        }
    }
}

fn add(term: &str, weight: u32, out: &mut HashMap<String, u32>) {
    let t = term.trim_matches(|c: char| c == '#' || c.is_whitespace());
    if t.is_empty() || !t.chars().any(is_cjk) {
        return;
    }
    // 含空白的整句不要
    if t.chars().any(char::is_whitespace) {
        return;
    }
    *out.entry(t.to_string()).or_insert(0) += weight;
}

/// 结果：写了几条、候选多少。
pub struct Report {
    pub files: usize,
    pub entries: usize,
}

/// 扫目录，生成 `out_path`。
pub fn rebuild(notes: &Path, out_path: &Path, exe_dir: &Path) -> Result<Report, String> {
    if !notes.is_dir() {
        return Err(format!("目录不存在：{}", notes.display()));
    }
    let pinyin = load_pinyin(exe_dir);
    if pinyin.is_empty() {
        return Err("找不到 pinyin.tsv（拼音表），请重装".into());
    }

    let mut terms: HashMap<String, u32> = HashMap::new();
    let mut files = 0usize;
    let mut stack = vec![notes.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_lowercase();
            if path.is_dir() {
                // 和脚本一致：这些目录不看
                if !name.starts_with('.')
                    && name != "_docx原件"
                    && name != "_archive"
                    && !name.contains("模板")
                {
                    stack.push(path);
                }
                continue;
            }
            if !(name.ends_with(".md") || name.ends_with(".txt")) {
                continue;
            }
            if let Ok(text) = fs::read_to_string(&path) {
                files += 1;
                scan_text(&text, &mut terms);
            }
        }
    }

    // 只留 2~4 个汉字
    terms.retain(|t, _| {
        let n = t.chars().count();
        (2..=4).contains(&n) && t.chars().all(is_cjk)
    });

    // 碎片：被更长词包含、且次数不比它多
    let snapshot: Vec<(String, u32)> = terms.iter().map(|(k, v)| (k.clone(), *v)).collect();
    let mut containing: HashMap<&str, u32> = HashMap::new();
    for (t, w) in &snapshot {
        let chars: Vec<char> = t.chars().collect();
        let n = chars.len();
        if n < 3 {
            continue;
        }
        for i in 0..n {
            for j in (i + 2)..=n {
                if j - i == n {
                    continue;
                }
                let sub: String = chars[i..j].iter().collect();
                let e = containing.entry(Box::leak(sub.into_boxed_str())).or_insert(0);
                if *w > *e {
                    *e = *w;
                }
            }
        }
    }
    terms.retain(|t, w| containing.get(t.as_str()).map_or(true, |cw| *cw < *w));

    // 按权重排序，截断
    let mut rows: Vec<(String, u32)> = terms.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    rows.truncate(MAX_TERMS);

    // 生成 TSV：每个词按读音组合各一条
    let mut lines: Vec<String> = Vec::with_capacity(rows.len());
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (word, weight) in &rows {
        let mut choices: Vec<&Vec<String>> = Vec::new();
        let mut ok = true;
        for c in word.chars() {
            match pinyin.get(&c) {
                Some(v) => choices.push(v),
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        // 组合：每个字最多两个读音，最多 4 条
        let mut combos: Vec<Vec<String>> = vec![Vec::new()];
        for v in &choices {
            let mut next = Vec::new();
            for base in &combos {
                for py in v.iter().take(2) {
                    let mut b = base.clone();
                    b.push(py.clone());
                    next.push(b);
                }
            }
            combos = next;
            if combos.len() > 4 {
                combos.truncate(4);
            }
        }
        for combo in combos {
            let code = combo.join(" ");
            if seen.insert((code.clone(), word.clone())) {
                lines.push(format!("{code}\t{word}\t{weight}"));
            }
        }
    }
    if let Some(parent) = out_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let mut f = fs::File::create(out_path).map_err(|e| format!("写语料表失败：{e}"))?;
    for l in &lines {
        let _ = writeln!(f, "{l}");
    }
    Ok(Report { files, entries: lines.len() })
}
