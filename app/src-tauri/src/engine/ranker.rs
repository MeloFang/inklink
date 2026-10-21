//! 召回与排序：给定拼音串，返回按分数排好序的候选。
//!
//! M0 只做**整词**匹配，不做跨词组合（见 PLAN 步骤 4 的决策）：
//! - 全拼整词 / 全拼前缀（输入未完成）
//! - 简拼整词（`zgrm` → 中国人民）
//! - 模糊音变体（zh/z、n/l、ng/n），命中后降权

use std::collections::{BTreeMap, HashMap};

use crate::engine::lexicon::{Entry, Lexicon};
use crate::engine::pinyin::{decode, fuzzy_variants, is_abbreviation, normalize_input};

/// 候选来源，决定惩罚分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// 全拼完全匹配。
    Exact,
    /// 全拼前缀匹配（输入还没打完）。
    Prefix,
    /// 简拼命中。
    Abbrev,
    /// 模糊音命中。
    Fuzzy,
}

impl Source {
    fn penalty(self) -> f32 {
        match self {
            Source::Exact => 0.0,
            Source::Prefix => 0.4,
            Source::Abbrev => 1.6,
            Source::Fuzzy => 1.0,
        }
    }

    fn rank(self) -> u8 {
        match self {
            Source::Exact => 0,
            Source::Prefix => 1,
            Source::Abbrev => 2,
            Source::Fuzzy => 3,
        }
    }
}

/// 一条候选。
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub text: String,
    pub pinyin: String,
    pub freq: u32,
    pub score: f32,
    pub source: Source,
}

/// 召回器：持有词库引用，并自建简拼索引。
pub struct Ranker<'a> {
    lex: &'a Lexicon,
    /// 简拼串 -> 词条下标
    initials: BTreeMap<String, Vec<u32>>,
}

impl<'a> Ranker<'a> {
    /// 建索引。8 万词条约 30–60ms，只在启动时做一次。
    pub fn new(lex: &'a Lexicon) -> Self {
        let mut initials: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        for (idx, e) in lex.entries().iter().enumerate() {
            let mut init = String::new();
            for syllable in e.pinyin.split_whitespace() {
                if let Some(c) = syllable.chars().next() {
                    init.push(c);
                }
            }
            if init.is_empty() {
                continue;
            }
            initials.entry(init).or_default().push(idx as u32);
        }
        Self { lex, initials }
    }

    /// 词条总数（便于外部核对索引）。
    pub fn len(&self) -> usize {
        self.lex.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.lex.is_empty()
    }

    /// 召回并排序。
    pub fn candidates(&self, input: &str, limit: usize) -> Vec<Candidate> {
        let _ = (input, limit);
        unimplemented!("TDD 红线：先写测试，再实现")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手写小词库：期望值全部来自这些字面量。
    const FIXTURE: &str = "\
你好	nihao	332885
项目	xiangmu	502761
中国	zhongguo	500000
中国人	zhongguoren	451625
中国人民	zhongguorenmin	186195
银行	yinhang	500769
行长	hangzhang	50370
北京	beijing	1119506
背景	beijing	500302
";

    fn ranker<'a>() -> (Lexicon, ()) {
        let _ = ranker;
        (Lexicon::parse_str(FIXTURE), ())
    }

    #[test]
    fn 全拼整词排第一() {
        let lex = Lexicon::parse_str(FIXTURE);
        let r = Ranker::new(&lex);
        let c = r.candidates("nihao", 10);
        assert_eq!(c.first().map(|x| x.text.as_str()), Some("你好"));
        assert_eq!(c[0].source, Source::Exact);
    }

    #[test]
    fn 同码候选按词频降序() {
        let lex = Lexicon::parse_str(FIXTURE);
        let r = Ranker::new(&lex);
        let c = r.candidates("beijing", 10);
        let names: Vec<&str> = c.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(names, vec!["北京", "背景"]);
    }

    #[test]
    fn 前缀输入也能出候选() {
        let lex = Lexicon::parse_str(FIXTURE);
        let r = Ranker::new(&lex);
        let c = r.candidates("zhonggu", 10);
        let names: Vec<&str> = c.iter().map(|x| x.text.as_str()).collect();
        assert!(names.contains(&"中国"), "应命中中国，实际 {names:?}");
        assert_eq!(c[0].source, Source::Prefix);
    }

    #[test]
    fn 简拼命中整词() {
        let lex = Lexicon::parse_str(FIXTURE);
        let r = Ranker::new(&lex);
        let c = r.candidates("zgrm", 10);
        assert_eq!(c.first().map(|x| x.text.as_str()), Some("中国人民"));
        assert_eq!(c[0].source, Source::Abbrev);
    }

    #[test]
    fn 结果确定且去重() {
        let lex = Lexicon::parse_str(FIXTURE);
        let r = Ranker::new(&lex);
        let a = r.candidates("zhongguo", 10);
        let b = r.candidates("zhongguo", 10);
        assert_eq!(a, b, "两次结果必须完全一致");
        let mut texts: Vec<&str> = a.iter().map(|x| x.text.as_str()).collect();
        texts.sort_unstable();
        let n = texts.len();
        texts.dedup();
        assert_eq!(texts.len(), n, "候选不能重复");
    }

    #[test]
    fn 空输入与无法识别输入返回空() {
        let lex = Lexicon::parse_str(FIXTURE);
        let r = Ranker::new(&lex);
        assert!(r.candidates("", 10).is_empty());
        assert!(r.candidates("zzz", 10).is_empty());
    }
}
