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
}