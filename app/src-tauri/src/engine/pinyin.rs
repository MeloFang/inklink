//! 拼音串切分：最大匹配（带回溯）+ 分隔符处理 + 简拼识别。
//!
//! 音节一律以词库的写法为准：无调、`ü` 记作 `v`（如 `lv`、`nve`）。

/// 全部合法音节（402 个），由 tools 脚本从 rime-ice 词表提取，勿手改。
const SYLLABLES: &[&str] = &[
    "a", "ai", "an", "ang", "ao", "ba", "bai", "ban", "bang", "bao", "bei", "ben", "beng", "bi",
    "bian", "biao", "bie", "bin", "bing", "bo", "bu", "ca", "cai", "can", "cang", "cao", "ce",
    "cen", "ceng", "cha", "chai", "chan", "chang", "chao", "che", "chen", "cheng", "chi", "chong",
    "chou", "chu", "chuai", "chuan", "chuang", "chui", "chun", "chuo", "ci", "cong", "cou", "cu",
    "cuan", "cui", "cun", "cuo", "da", "dai", "dan", "dang", "dao", "de", "dei", "deng", "di",
    "dian", "diao", "die", "ding", "diu", "dong", "dou", "du", "duan", "dui", "dun", "duo", "e",
    "en", "eng", "er", "fa", "fan", "fang", "fei", "fen", "feng", "fo", "fou", "fu", "ga", "gai",
    "gan", "gang", "gao", "ge", "gei", "gen", "geng", "gong", "gou", "gu", "gua", "guai", "guan",
    "guang", "gui", "gun", "guo", "ha", "hai", "han", "hang", "hao", "he", "hei", "hen", "heng",
    "hong", "hou", "hu", "hua", "huai", "huan", "huang", "hui", "hun", "huo", "ji", "jia", "jian",
    "jiang", "jiao", "jie", "jin", "jing", "jiong", "jiu", "ju", "juan", "jue", "jun", "ka", "kai",
    "kan", "kang", "kao", "ke", "ken", "keng", "kong", "kou", "ku", "kua", "kuai", "kuan", "kuang",
    "kui", "kun", "kuo", "la", "lai", "lan", "lang", "lao", "le", "lei", "leng", "li", "lia",
    "lian", "liang", "liao", "lie", "lin", "ling", "liu", "long", "lou", "lu", "luan", "lun",
    "luo", "lv", "lve", "ma", "mai", "man", "mang", "mao", "me", "mei", "men", "meng", "mi",
    "mian", "miao", "mie", "min", "ming", "miu", "mo", "mou", "mu", "na", "nai", "nan", "nang",
    "nao", "ne", "nei", "nen", "neng", "ni", "nian", "niang", "niao", "nie", "nin", "ning", "niu",
    "nong", "nu", "nuan", "nuo", "nv", "nve", "o", "ou", "pa", "pai", "pan", "pang", "pao", "pei",
    "pen", "peng", "pi", "pian", "piao", "pie", "pin", "ping", "po", "pou", "pu", "qi", "qia",
    "qian", "qiang", "qiao", "qie", "qin", "qing", "qiong", "qiu", "qu", "quan", "que", "qun",
    "ran", "rang", "rao", "re", "ren", "reng", "ri", "rong", "rou", "ru", "ruan", "rui", "run",
    "ruo", "sa", "sai", "san", "sang", "sao", "se", "sen", "seng", "sha", "shai", "shan", "shang",
    "shao", "she", "shei", "shen", "sheng", "shi", "shou", "shu", "shua", "shuai", "shuan",
    "shuang", "shui", "shun", "shuo", "si", "song", "sou", "su", "suan", "sui", "sun", "suo", "ta",
    "tai", "tan", "tang", "tao", "te", "teng", "ti", "tian", "tiao", "tie", "ting", "tong", "tou",
    "tu", "tuan", "tui", "tun", "tuo", "wa", "wai", "wan", "wang", "wei", "wen", "weng", "wo",
    "wu", "xi", "xia", "xian", "xiang", "xiao", "xie", "xin", "xing", "xiong", "xiu", "xu", "xuan",
    "xue", "xun", "ya", "yan", "yang", "yao", "ye", "yi", "yin", "ying", "yo", "yong", "you", "yu",
    "yuan", "yue", "yun", "za", "zai", "zan", "zang", "zao", "ze", "zei", "zen", "zeng", "zha",
    "zhai", "zhan", "zhang", "zhao", "zhe", "zhei", "zhen", "zheng", "zhi", "zhong", "zhou", "zhu",
    "zhua", "zhuai", "zhuan", "zhuang", "zhui", "zhun", "zhuo", "zi", "zong", "zou", "zu", "zuan",
    "zui", "zun", "zuo",
];

/// 最长音节长度（`chuang` / `shuang` / `zhuang` 均为 6）。
pub const MAX_SYLLABLE_LEN: usize = 6;

/// 是否是合法音节。
pub fn is_syllable(s: &str) -> bool {
    SYLLABLES.binary_search(&s).is_ok()
}

/// 把用户输入归一化成切分用的编码：小写、去掉分隔符、`ü` 统一成 `v`。
pub fn normalize_input(input: &str) -> String {
    input
        .chars()
        .map(|c| match c {
            // 撇号/空格/隔音符号是**音节边界**，统一成空格保留，不能直接删
            '\'' | ' ' | '`' | '’' => ' ',
            'ü' | 'ǖ' | 'ǘ' | 'ǚ' | 'ǜ' => 'v',
            c => c.to_ascii_lowercase(),
        })
        .collect()
}

/// 把拼音串切成音节。无法完整切分时返回 `None`；空串也返回 `None`。
///
/// 分隔符（空格、撇号）视为强制音节边界：`xi'an` 切成 `xi` + `an`，而不是 `xian`。
pub fn decode(input: &str) -> Option<Vec<String>> {
    let normalized = normalize_input(input);
    let mut out = Vec::new();
    let mut any = false;
    for chunk in normalized.split_whitespace() {
        any = true;
        let mut part = Vec::new();
        if !segment(chunk, 0, &mut part) {
            return None;
        }
        out.extend(part);
    }
    if any { Some(out) } else { None }
}

/// 带回溯的最大匹配：优先长音节，失败则退回短音节。
fn segment(code: &str, pos: usize, out: &mut Vec<String>) -> bool {
    if pos == code.len() {
        return true;
    }
    let remaining = code.len() - pos;
    let max = MAX_SYLLABLE_LEN.min(remaining);
    for len in (1..=max).rev() {
        let cand = &code[pos..pos + len];
        if is_syllable(cand) {
            out.push(cand.to_string());
            if segment(code, pos + len, out) {
                return true;
            }
            out.pop();
        }
    }
    false
}

/// 带模糊音的变体：zh/z、ch/c、sh/s、n/l、ng/n 可以互换。
///
/// 返回**除自身以外**的全部变体，按字典序去重。用于召回时扩大匹配面。
pub fn fuzzy_variants(syllable: &str) -> Vec<String> {
    use std::collections::BTreeSet;

    let mut out: BTreeSet<String> = BTreeSet::new();
    let mut frontier = vec![syllable.to_string()];
    while let Some(cur) = frontier.pop() {
        for next in one_fuzzy_step(&cur) {
            if next != syllable && out.insert(next.clone()) {
                frontier.push(next);
            }
        }
    }
    out.into_iter().collect()
}

/// 单步模糊变体（不含组合）。
///
/// 注意：声母替换必须在「已含双字母声母」时才用单字母版本，否则 `sh` 会被再拆成
/// `s` + `h` 得到 `shh`，进而无限增长。
fn one_fuzzy_step(s: &str) -> Vec<String> {
    let mut v = Vec::new();
    for (two, one) in [("zh", "z"), ("ch", "c"), ("sh", "s")] {
        if let Some(rest) = s.strip_prefix(two) {
            v.push(format!("{one}{rest}"));
        } else if let Some(rest) = s.strip_prefix(one) {
            v.push(format!("{two}{rest}"));
        }
    }
    if let Some(rest) = s.strip_prefix('n') {
        v.push(format!("l{rest}"));
    }
    if let Some(rest) = s.strip_prefix('l') {
        v.push(format!("n{rest}"));
    }
    if let Some(stem) = s.strip_suffix("ng") {
        v.push(format!("{stem}n"));
    } else if let Some(stem) = s.strip_suffix('n') {
        v.push(format!("{stem}ng"));
    }
    v
}

/// 声母集合（含零声母占位）。
const INITIALS: &[char] = &[
    'b', 'p', 'm', 'f', 'd', 't', 'n', 'l', 'g', 'k', 'h', 'j', 'q', 'x', 'r', 'z', 'c', 's', 'y',
    'w',
];

/// 是否可能是简拼：至少 2 个字符，且每个字符都是声母字母。
///
/// 注意：这只是**必要条件**，`zh`/`ch`/`sh` 的两个字母会被当成两个声母，属于可接受的误判。
pub fn is_abbreviation(input: &str) -> bool {
    let code: String = normalize_input(input).split_whitespace().collect();
    code.chars().count() >= 2 && code.chars().all(|c| INITIALS.contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 全拼按最大匹配切分() {
        assert_eq!(decode("xiangmu"), Some(vec!["xiang".into(), "mu".into()]));
        assert_eq!(decode("nihao"), Some(vec!["ni".into(), "hao".into()]));
    }

    #[test]
    fn 无法切分时返回_none() {
        assert_eq!(decode(""), None);
        assert_eq!(decode("xyz"), None);
        assert_eq!(decode("xiangmuu"), None);
    }

    #[test]
    fn 音节判定() {
        assert!(is_syllable("chuang"));
        assert!(is_syllable("a"));
        assert!(!is_syllable("zzz"));
    }

    #[test]
    fn 模糊音生成变体() {
        // zh/z
        assert!(fuzzy_variants("zhang").contains(&"zang".to_string()));
        // ng/n
        assert!(fuzzy_variants("zhang").contains(&"zhan".to_string()));
        // sh/s
        assert!(fuzzy_variants("shi").contains(&"si".to_string()));
        // n/l
        assert!(fuzzy_variants("ni").contains(&"li".to_string()));
        // 自身不在变体里
        assert!(!fuzzy_variants("ni").contains(&"ni".to_string()));
    }

    #[test]
    fn 逗号分隔的输入被归一化() {
        // 分隔符不影响切分
        assert_eq!(decode("xi'an"), Some(vec!["xi".into(), "an".into()]));
        // ü 的各种写法统一成 v
        assert_eq!(decode("lüxing"), Some(vec!["lv".into(), "xing".into()]));
        assert_eq!(decode("LVXING"), Some(vec!["lv".into(), "xing".into()]));
    }

    #[test]
    fn 简拼识别() {
        assert!(is_abbreviation("xmch"));
        assert!(is_abbreviation("zgrm"));
        assert!(!is_abbreviation("xiangmu"));
        assert!(!is_abbreviation("x"));
    }
}
