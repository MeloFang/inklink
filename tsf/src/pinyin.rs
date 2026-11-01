//! 拼音音节切分：只回答一个窄问题——「选掉 k 个汉字之后，拼音串被消耗了多少个字母」。
//!
//! 不做查询、不碰词库、不判断整串是不是拼音（那是引擎的事）。
//! 音节表复制自 `app/src-tauri/src/engine/pinyin.rs`（402 个），两边必须一致。
//!
//! 为什么需要它：用户打 `pikeqiu` 想选「皮」再接着选「克」「球」，
//! 选完「皮」得知道该从缓冲里剪掉 `pi` 两个字母，剩下的 `keqiu` 留着继续拼。

/// 全部合法音节（402 个），与引擎同一张表，勿手改。
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
/// 单个音节是否合法（表已排序，二分即可）。
pub fn is_syllable(s: &str) -> bool {
    SYLLABLES.binary_search(&s).is_ok()
}

/// 把拼音串切成音节，返回每个音节的长度。
///
/// 贪心最长匹配——和 rime 的切分偏好一致（`xian` 切成一个，不是 `xi an`）。
/// 遇到切不动的部分（英文、打错的字母）就停下，返回已切出的长度。
/// 非 ASCII 输入直接返回空：那不是拼音。
pub fn split_lengths(code: &str) -> Vec<usize> {
    let bytes = code.as_bytes();
    if !bytes.iter().all(|b| b.is_ascii_lowercase()) {
        return Vec::new();
    }
    let mut lens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // 最长音节 6 个字母（zhuang / chuang）
        let max = (bytes.len() - i).min(6);
        let mut picked = 0;
        for len in (1..=max).rev() {
            if is_syllable(&code[i..i + len]) {
                picked = len;
                break;
            }
        }
        if picked == 0 {
            break;
        }
        lens.push(picked);
        i += picked;
    }
    lens
}

/// 选掉 `chars` 个汉字，消耗掉拼音串里多少个字母。
///
/// - 切不出音节（英文/乱码）→ 返回 0，调用方按「整串消耗」处理
/// - 汉字数 ≥ 音节数 → 整串消耗（末尾多打的字母一起吃掉，正是用户要的「多余的删掉」）
pub fn consumed_letters(code: &str, chars: usize) -> usize {
    let lens = split_lengths(code);
    if lens.is_empty() || chars == 0 {
        return 0;
    }
    if chars >= lens.len() {
        return code.len();
    }
    lens[..chars].iter().sum()
}

/// 音节边界在**字母下标**上的位置（在第 k 个字母之前插一个 `'`）。
///
/// 先把用户自己按的 `-` `_` `'` 归一成分隔符，再对每一段做贪心切分补上音节边界。
/// `zheg` 这种切不动的尾巴也单独隔开（`zhe'g`），因为用户一眼能看出"g 是多余的"。
fn boundary_positions(raw: &str) -> Vec<usize> {
    let mut norm = String::with_capacity(raw.len());
    let mut last_sep = true; // 开头不允许分隔符
    for c in raw.chars() {
        if c.is_ascii_alphabetic() {
            norm.push(c.to_ascii_lowercase());
            last_sep = false;
        } else if matches!(c, '-' | '_' | '\'' | ' ') && !last_sep {
            norm.push('\'');
            last_sep = true;
        }
    }
    while norm.ends_with('\'') {
        norm.pop();
    }

    let mut bounds = Vec::new();
    let mut base = 0usize;
    for seg in norm.split('\'') {
        if seg.is_empty() {
            continue;
        }
        if base > 0 {
            bounds.push(base);
        }
        let lens = split_lengths(seg);
        let mut cum = 0usize;
        for (idx, len) in lens.iter().enumerate() {
            cum += *len;
            if idx + 1 < lens.len() {
                bounds.push(base + cum);
            }
        }
        let consumed: usize = lens.iter().sum();
        if consumed > 0 && consumed < seg.len() {
            bounds.push(base + consumed);
        }
        base += seg.len();
    }
    bounds.sort_unstable();
    bounds.dedup();
    bounds
}

/// 组合串的显示形式：音节之间插 `'`，和搜狗/微信一样。
///
/// `raw` 是缓冲里的原始串（字母 + 用户自己按的 `-`/`_`），`cursor` 是它在里面的**字节**下标；
/// 返回 (显示串, 光标在显示串里的位置)。两者都只含 ASCII，字节下标 = 字符下标 = UTF-16 长度，
/// 所以结果可以直接喂给 `SetText` / `ShiftEnd`。
///
/// 规则：
/// - 先归一用户给的边界，再对每段贪心切音节补边界（`xiangmu` → `xiang'mu`）
/// - 切不动的尾巴接在 `'` 后面（`zheg` → `zhe'g`）；整串切不出音节就原样返回（`github`）
/// - 光标落在音节边界上时停在分隔符**前面**（`xi|'an`）：退格删的是它前面那个字母，
///   视觉上不该越过 `'`
pub fn compose_view(raw: &str, cursor: usize) -> (String, usize) {
    let letters: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let before = raw
        .char_indices()
        .filter(|(i, c)| *i < cursor && c.is_ascii_alphabetic())
        .count();

    let bounds = boundary_positions(raw);
    let mut out = String::with_capacity(letters.len() + bounds.len());
    let mut out_cursor = None;
    let mut bi = 0usize;
    for (i, ch) in letters.char_indices() {
        if i == before {
            out_cursor = Some(out.len());
        }
        while bi < bounds.len() && bounds[bi] == i {
            if i > 0 {
                out.push('\'');
            }
            bi += 1;
        }
        out.push(ch);
    }
    let out_cursor = out_cursor.unwrap_or(out.len());
    (out, out_cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn 切分常见拼音串() {
        assert_eq!(split_lengths("pikeqiu"), vec![2, 2, 3]);
        assert_eq!(split_lengths("nihao"), vec![2, 3]);
        assert_eq!(split_lengths("xian"), vec![4]);
        assert_eq!(split_lengths("xiangmu"), vec![5, 2]);
    }

    #[test]
    fn 英文切不动() {
        assert!(split_lengths("github").is_empty());
        assert_eq!(consumed_letters("github", 6), 0);
        assert!(split_lengths("VSCode").is_empty());
    }

    #[test]
    fn 消耗字母数按汉字数走() {
        assert_eq!(consumed_letters("pikeqiu", 1), 2); // 皮 -> pi
        assert_eq!(consumed_letters("pikeqiu", 2), 4); // 皮克 -> pi ke
        assert_eq!(consumed_letters("pikeqiu", 3), 7); // 皮克球 -> 整串
        assert_eq!(consumed_letters("pikeqiu", 9), 7); // 候选比音节多 -> 整串
    }

    #[test]
    fn 尾巴上的错字母一并吃掉() {
        // pikeqiux：切到 qiu 就停，选 3 个字时连多打的 x 一起消耗
        assert_eq!(split_lengths("pikeqiux"), vec![2, 2, 3]);
        assert_eq!(consumed_letters("pikeqiux", 3), 8);
        assert_eq!(consumed_letters("pikeqiux", 1), 2);
    }

    #[test]
    fn 单字母也是音节() {
        assert_eq!(consumed_letters("a", 1), 1);
        assert_eq!(consumed_letters("o", 1), 1);
        assert_eq!(consumed_letters("e", 1), 1);
    }

    #[test]
    fn 组合串显示带音节分隔() {
        // 多音节：贪心切分补分隔符
        assert_eq!(compose_view("xiangmu", 7).0, "xiang'mu");
        assert_eq!(compose_view("nihaoma", 7).0, "ni'hao'ma");
        assert_eq!(compose_view("pikeqiu", 7).0, "pi'ke'qiu");
        assert_eq!(compose_view("jintiantianqi", 13).0, "jin'tian'tian'qi");
        // 用户自己给的边界优先（`-` `_` 都归一成 `'`）
        assert_eq!(compose_view("xi'an", 5).0, "xi'an");
        assert_eq!(compose_view("xi-an", 5).0, "xi'an");
        assert_eq!(compose_view("xi_an", 5).0, "xi'an");
        // 单个音节不加分隔符；切不动的英文原样
        assert_eq!(compose_view("xian", 4).0, "xian");
        assert_eq!(compose_view("github", 6).0, "github");
        // 切不动的尾巴单独隔开，一眼看出哪个字母是多的
        assert_eq!(compose_view("zheg", 4).0, "zhe'g");
        // 空串
        assert_eq!(compose_view("", 0).0, "");
    }

    #[test]
    fn 显示光标停在音节边界前() {
        // 光标在 xiang 之后：落在 `'` 前面（`xiang|'mu`），而不是后面
        assert_eq!(compose_view("xiangmu", 5), ("xiang'mu".to_string(), 5));
        // 光标在末尾：跳过分隔符，落在最后一个字母之后
        assert_eq!(compose_view("xiangmu", 7), ("xiang'mu".to_string(), 8));
        assert_eq!(compose_view("xiangmu", 0), ("xiang'mu".to_string(), 0));
        // 原始串里的 `-` 不占显示位置
        assert_eq!(compose_view("xi-an", 3), ("xi'an".to_string(), 2));
        // 末尾：显示串是 `xi'an` 五个字符，光标在最后
        assert_eq!(compose_view("xi-an", 5), ("xi'an".to_string(), 5));
        assert_eq!(compose_view("xian", 2), ("xian".to_string(), 2));
    }
}
