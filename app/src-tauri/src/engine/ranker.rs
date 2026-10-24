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
    /// 整句组合（词图 DP，长输入时给出「我来说说」这种整句候选）。
    Sentence,
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
            Source::Sentence => 0.0,
            Source::Exact => 0.0,
            Source::Prefix => 0.4,
            Source::Abbrev => 1.6,
            Source::Fuzzy => 1.0,
        }
    }

    fn rank(self) -> u8 {
        match self {
            // 整句组合排最前（长输入时用户要的就是整句）
            Source::Sentence => 0,
            // 完全匹配的整词
            Source::Exact => 1,
            // 模糊音命中也是「整词匹配」，只是读音近似
            Source::Fuzzy => 2,
            // 前缀命中（输入还没打完）排在这之后，避免 `sha` 首选「上」(shang)
            Source::Prefix => 3,
            Source::Abbrev => 4,
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

/// 召回器：**持有**词库，并自建编码与简拼索引。
///
/// 选择持有而非借用，是为了能存进 Tauri 的全局状态（`State<Mutex<Ranker>>`）——
/// 借用会让结构体变成自引用。
pub struct Ranker {
    lex: Lexicon,
    /// 全拼码 -> 词条下标
    codes: BTreeMap<String, Vec<u32>>,
    /// 简拼串 -> 词条下标
    initials: BTreeMap<String, Vec<u32>>,
}

impl Ranker {
    /// 建索引。8 万词条约 90ms，只在启动时做一次。
    pub fn new(lex: Lexicon) -> Self {
        let mut codes: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        let mut initials: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        for (idx, e) in lex.entries().iter().enumerate() {
            let id = idx as u32;
            codes.entry(e.code.clone()).or_default().push(id);

            let mut init = String::new();
            for syllable in e.pinyin.split_whitespace() {
                if let Some(c) = syllable.chars().next() {
                    init.push(c);
                }
            }
            if !init.is_empty() {
                initials.entry(init).or_default().push(id);
            }
        }
        Self {
            lex,
            codes,
            initials,
        }
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
    ///
    /// 顺序：全拼整词 → 全拼前缀 → 模糊音变体 → 简拼。同一词条只保留最高分。
    pub fn candidates(&self, input: &str, limit: usize) -> Vec<Candidate> {
        let compact: String = normalize_input(input).split_whitespace().collect();
        if compact.is_empty() {
            return Vec::new();
        }

        // 词条下标 -> (score, source)
        let mut best: HashMap<u32, (f32, Source)> = HashMap::new();

        if let Some(syllables) = decode(input) {
            let code = syllables.concat();

            // 全拼整词 / 前缀
            for (idx, entry) in self.indexed_lookup(&code) {
                let source = if entry.code == code {
                    Source::Exact
                } else {
                    Source::Prefix
                };
                self.consider(&mut best, idx, entry, source);
            }

            // 模糊音：逐音节替换，命中降权
            for (pos, syllable) in syllables.iter().enumerate() {
                for variant in fuzzy_variants(syllable).into_iter().take(4) {
                    let mut alt = syllables.clone();
                    alt[pos] = variant;
                    let alt_code = alt.concat();
                    if alt_code == code {
                        continue;
                    }
                    for (idx, entry) in self.indexed_lookup(&alt_code) {
                        self.consider(&mut best, idx, entry, Source::Fuzzy);
                    }
                }
            }
        }

        // 简拼整词
        if is_abbreviation(input) {
            for (_, ids) in self
                .initials
                .range(compact.clone()..)
                .take_while(|(k, _)| k.starts_with(&compact))
            {
                for id in ids {
                    let entry = &self.lex.entries()[*id as usize];
                    self.consider(&mut best, *id, entry, Source::Abbrev);
                }
            }
        }

        let mut out: Vec<Candidate> = best
            .into_iter()
            .map(|(idx, (score, source))| {
                let e = &self.lex.entries()[idx as usize];
                Candidate {
                    text: e.text.clone(),
                    pinyin: e.pinyin.clone(),
                    freq: e.freq,
                    score,
                    source,
                }
            })
            .collect();

        // 先按匹配类型分组，再按分数：打 `sha` 时 `上`（shang 的前缀）不能排在
        // 真正的 `sha` 候选前面——主流输入法都是这个行为。
        out.sort_by(|a, b| {
            a.source
                .rank()
                .cmp(&b.source.rank())
                .then_with(|| {
                    b.score
                        .partial_cmp(&a.score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| b.freq.cmp(&a.freq))
                .then_with(|| a.text.cmp(&b.text))
        });
        out.truncate(limit);

        // 整句组合：整串没有精确命中时，把音节序列按词图 DP 切成最优词序列。
        // 这是长输入（wolaishuoshuo → 我来说说）的关键，且不会干扰 `xian` 这类
        // 本来就有整词命中的输入。
        let has_exact = out.iter().any(|c| c.source == Source::Exact);
        if !has_exact {
            if let Some(syllables) = decode(input) {
                if syllables.len() >= 2 {
                    if let Some(sent) = self.best_sentence(&syllables) {
                        if !out.iter().any(|c| c.text == sent) {
                            out.insert(
                                0,
                                Candidate {
                                    text: sent,
                                    pinyin: syllables.join(" "),
                                    freq: u32::MAX,
                                    score: f32::MAX,
                                    source: Source::Sentence,
                                },
                            );
                            out.truncate(limit);
                        }
                    }
                }
            }
        }

        out
    }

    /// 词图 DP：把音节序列切成最优的词序列，返回整句文本。
    ///
    /// `dp[i]` = 覆盖 `syllables[0..i]` 的最优（累计分，词下标序列）。
    /// 转移：枚举最后一个词 `syllables[j..i]`，要求词库里存在该编码。
    /// 打分偏好「词频高 + 切得少（长词）」的切法。
    fn best_sentence(&self, syllables: &[String]) -> Option<String> {
        let n = syllables.len();
        if n < 2 {
            return None;
        }
        let mut dp: Vec<Option<(f32, Vec<u32>)>> = vec![None; n + 1];
        dp[0] = Some((0.0, Vec::new()));

        for i in 1..=n {
            for j in (0..i).rev() {
                let code: String = syllables[j..i].concat();
                let Some(ids) = self.codes.get(&code) else {
                    continue;
                };
                let Some((prev_score, prev_words)) = dp[j].as_ref() else {
                    continue;
                };
                // 同码取最高频的那个词
                let Some(&best_id) = ids
                    .iter()
                    .max_by_key(|id| self.lex.entries()[**id as usize].freq)
                else {
                    continue;
                };
                let e = &self.lex.entries()[best_id as usize];
                let word_score = (1.0 + e.freq as f32).ln();
                // 单字词重罚：中文里单字多是语素（进/起/翻 词频极高），
                // 不罚的话 DP 会把「今天天气很好」切成「进+天天+起+很好」。
                let single_penalty = if i - j == 1 { -8.0 } else { 0.0 };
                // 长词奖励：每个词奖励 (音节数-1)，累加后等价于「切得越少越好」。
                let len_bonus = ((i - j) - 1) as f32 * 3.0;
                let score = prev_score + word_score + single_penalty + len_bonus;
                let better = dp[i].as_ref().is_none_or(|(s, _)| score > *s);
                if better {
                    let mut words = prev_words.clone();
                    words.push(best_id);
                    dp[i] = Some((score, words));
                }
            }
        }

        let (_, words) = dp[n].as_ref()?;
        Some(
            words
                .iter()
                .map(|id| self.lex.entries()[*id as usize].text.as_str())
                .collect(),
        )
    }

    /// 按编码前缀取候选，同时给出下标。
    fn indexed_lookup<'e>(&'e self, code: &str) -> impl Iterator<Item = (u32, &'e Entry)> + 'e {
        let entries = self.lex.entries();
        let needle = code.to_string();
        let guard = needle.clone();
        self.codes
            .range(needle..)
            .take_while(move |(k, _)| k.starts_with(&guard))
            .flat_map(move |(_, ids)| ids.iter().map(move |id| (*id, &entries[*id as usize])))
    }

    fn consider(
        &self,
        best: &mut HashMap<u32, (f32, Source)>,
        idx: u32,
        entry: &Entry,
        source: Source,
    ) {
        let score = (1.0 + entry.freq as f32).ln() - source.penalty();
        let slot = best.entry(idx).or_insert((f32::NEG_INFINITY, source));
        if score > slot.0 {
            *slot = (score, source);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 手写小词库：期望值全部来自这些字面量。
    const FIXTURE: &str = "\
你好\tni hao\t332885
项目\txiang mu\t502761
中国\tzhong guo\t500000
中国人\tzhong guo ren\t451625
中国人民\tzhong guo ren min\t186195
银行\tyin hang\t500769
行长\thang zhang\t50370
北京\tbei jing\t1119506
背景\tbei jing\t500302
";

    #[test]
    fn 全拼整词排第一() {
        let r = Ranker::new(Lexicon::parse_str(FIXTURE));
        let c = r.candidates("nihao", 10);
        assert_eq!(c.first().map(|x| x.text.as_str()), Some("你好"));
        assert_eq!(c[0].source, Source::Exact);
    }

    #[test]
    fn 同码候选按词频降序() {
        let r = Ranker::new(Lexicon::parse_str(FIXTURE));
        let c = r.candidates("beijing", 10);
        let names: Vec<&str> = c.iter().map(|x| x.text.as_str()).collect();
        assert_eq!(names, vec!["北京", "背景"]);
    }

    #[test]
    fn 前缀输入也能出候选() {
        let r = Ranker::new(Lexicon::parse_str(FIXTURE));
        let c = r.candidates("zhonggu", 10);
        let names: Vec<&str> = c.iter().map(|x| x.text.as_str()).collect();
        assert!(names.contains(&"中国"), "应命中中国，实际 {names:?}");
        assert_eq!(c[0].source, Source::Prefix);
    }

    #[test]
    fn 简拼命中整词() {
        let r = Ranker::new(Lexicon::parse_str(FIXTURE));
        let c = r.candidates("zgrm", 10);
        assert_eq!(c.first().map(|x| x.text.as_str()), Some("中国人民"));
        assert_eq!(c[0].source, Source::Abbrev);
    }

    #[test]
    fn 结果确定且去重() {
        let r = Ranker::new(Lexicon::parse_str(FIXTURE));
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
        let r = Ranker::new(Lexicon::parse_str(FIXTURE));
        assert!(r.candidates("", 10).is_empty());
        assert!(r.candidates("zzz", 10).is_empty());
    }
}
