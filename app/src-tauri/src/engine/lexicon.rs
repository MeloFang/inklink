//! 词库：TSV 格式的加载与拼音前缀查询。
//!
//! 文件格式（UTF-8 无 BOM），三列以制表符分隔：
//! - 第 1 列：词条文本
//! - 第 2 列：拼音，音节之间用空格分隔，如 `xiang mu dai hao chi tu`
//! - 第 3 列：词频，可缺省（缺省为 0）
//!
//! 以 `#` 开头的行是注释；坏行会被跳过并计数。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 一条词条。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 词条文本。
    pub text: String,
    /// 原始拼音，空格分音节，如 `xiang mu`。
    pub pinyin: String,
    /// 归一化后的编码：小写、去空格，如 `xiangmu`。
    pub code: String,
    /// 词频，越大越靠前。
    pub freq: u32,
}

/// 把拼音归一化成查询用的编码：去空白、转小写。
pub fn normalize_code(pinyin: &str) -> String {
    pinyin
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 词库加载错误。
#[derive(Debug, thiserror::Error)]
pub enum LexError {
    #[error("词库文件不存在: {0}")]
    NotFound(PathBuf),
    #[error("词库读取失败: {0}")]
    Io(#[from] std::io::Error),
}

/// 词库索引。
#[derive(Debug, Default)]
pub struct Lexicon {
    entries: Vec<Entry>,
    /// 编码 -> 词条下标，按编码有序，支持前缀范围扫描。
    index: BTreeMap<String, Vec<u32>>,
    /// 被跳过的坏行数量。
    skipped: usize,
}

impl Lexicon {
    /// 从文件加载。
    pub fn load(path: &Path) -> Result<Self, LexError> {
        if !path.is_file() {
            return Err(LexError::NotFound(path.to_path_buf()));
        }
        let content = std::fs::read_to_string(path)?;
        Ok(Self::parse_str(&content))
    }

    /// 从字符串加载（测试与资源包内嵌用）。
    pub fn parse_str(input: &str) -> Self {
        let mut entries: Vec<Entry> = Vec::new();
        let mut index: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        let mut skipped = 0usize;

        for raw in input.lines() {
            let line = raw.trim_end_matches('\r');
            if line.trim().is_empty() || line.starts_with('#') {
                continue;
            }

            let mut fields = line.split('\t');
            let text = fields.next().unwrap_or("").trim();
            let pinyin = fields.next().unwrap_or("").trim();
            let freq_raw = fields.next().unwrap_or("");

            if text.is_empty() || pinyin.is_empty() {
                skipped += 1;
                continue;
            }

            let code = normalize_code(pinyin);
            if code.is_empty() {
                skipped += 1;
                continue;
            }

            let id = entries.len() as u32;
            entries.push(Entry {
                text: text.to_string(),
                pinyin: pinyin.to_string(),
                code: code.clone(),
                freq: freq_raw.trim().parse().unwrap_or(0),
            });
            index.entry(code).or_default().push(id);
        }

        Self {
            entries,
            index,
            skipped,
        }
    }

    /// 词条数。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 被跳过的坏行数。
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    /// 按编码前缀查询，返回匹配词条的迭代器（编码序 + 同码内加入顺序）。
    pub fn lookup(&self, prefix: &str) -> impl Iterator<Item = &Entry> + '_ {
        let needle = normalize_code(prefix);
        let guard = needle.clone();
        self.index
            .range(needle..)
            .take_while(move |(key, _)| key.starts_with(&guard))
            .flat_map(|(_, ids)| ids.iter())
            .map(|id| &self.entries[*id as usize])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# 这是注释
项目代号赤兔\txiang mu dai hao chi tu\t35
光纤入户\tguang xian ru hu\t20
嘉兴分公司\tjia xing fen gong si\t20

坏行没有制表符
只有词没有拼音\t
背景\tbei jing\t10
";

    #[test]
    fn 跳过注释空行与坏行并计数() {
        let lex = Lexicon::parse_str(SAMPLE);
        assert_eq!(lex.len(), 4);
        assert_eq!(lex.skipped(), 2);
    }

    #[test]
    fn 编码归一化为小写去空格() {
        let lex = Lexicon::parse_str(SAMPLE);
        let e = lex
            .lookup("Xiang Mu Dai Hao Chi Tu")
            .next()
            .expect("应能按去空格的小写编码找到");
        assert_eq!(e.text, "项目代号赤兔");
        assert_eq!(e.pinyin, "xiang mu dai hao chi tu");
        assert_eq!(e.code, "xiangmudaihaochitu");
        assert_eq!(e.freq, 35);
    }

    #[test]
    fn 前缀查询返回全部匹配() {
        let lex = Lexicon::parse_str(SAMPLE);
        let names: Vec<&str> = lex.lookup("xiangmu").map(|e| e.text.as_str()).collect();
        assert_eq!(names, vec!["项目代号赤兔"]);
    }

    #[test]
    fn 前缀无匹配时为空() {
        let lex = Lexicon::parse_str(SAMPLE);
        assert_eq!(lex.lookup("zzz").count(), 0);
    }

    #[test]
    fn 文件不存在返回错误() {
        let err = Lexicon::load(Path::new("C:/不存在的路径/lexicon.tsv")).unwrap_err();
        assert!(matches!(err, LexError::NotFound(_)));
    }

    #[test]
    fn 六万条加载与前缀查询满足预算() {
        let mut src = String::with_capacity(6_000_000);
        for i in 0..60_000u32 {
            src.push_str(&format!("词{i}\txiang mu {:04}\t{}\n", i % 1000, i % 5000));
        }

        let t0 = std::time::Instant::now();
        let lex = Lexicon::parse_str(&src);
        let load_ms = t0.elapsed().as_millis();
        assert_eq!(lex.len(), 60_000);
        // debug 构建放宽到 800ms；release 下应远低于 200ms
        assert!(load_ms < 800, "加载耗时 {load_ms}ms 超预算");

        // 真实场景：6 字符以上的编码前缀，命中几十到几百条
        let t1 = std::time::Instant::now();
        let mut hits = 0usize;
        for _ in 0..100_000 {
            hits += lex.lookup("xiangmu0042").count();
        }
        let q_ms = t1.elapsed().as_millis();
        eprintln!("[perf] 6万条: load={load_ms}ms, 10万次前缀查询={q_ms}ms");
        assert!(hits > 0);
        assert!(q_ms < 400, "10 万次前缀查询耗时 {q_ms}ms 超预算");
    }
}
