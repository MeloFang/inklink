//! 排序预览：给定拼音，打印当前引擎能给出的候选与权重。
//!
//! 只做「切分 → 查库 → 按权重降序」，**没有**模糊音召回和简拼召回（那是步骤 4）。
//!
//! 用法（在 app/src-tauri 下）：
//! ```powershell
//! cargo run --example preview_rank -- ../../data/base.tsv
//! cargo run --example preview_rank -- ../../data/base.tsv nihao xiangmu
//! ```

use std::path::PathBuf;

use inklink_lib::engine::lexicon::{Entry, Lexicon};
use inklink_lib::engine::pinyin::decode;

const DEFAULT_QUERIES: &[&str] = &[
    "nihao", "xiangmu", "zhongguo", "women", "shijian", "gongzuo", "zhege", "yinhang", "beijing",
    "shanghai", "chifan", "renmin",
];

fn main() {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().unwrap_or_else(|| "../../data/base.tsv".into()));

    let lex = match Lexicon::load(&path) {
        Ok(lex) => lex,
        Err(e) => {
            eprintln!("加载失败 {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    println!("词库 {} 条\n", lex.len());

    let custom: Vec<String> = args.collect();
    let queries: Vec<&str> = if custom.is_empty() {
        DEFAULT_QUERIES.to_vec()
    } else {
        custom.iter().map(String::as_str).collect()
    };

    for query in queries {
        let Some(syllables) = decode(query) else {
            println!("{query:<14} 切分失败");
            continue;
        };
        let code = syllables.concat();

        let mut exact: Vec<&Entry> = lex.lookup(&code).filter(|e| e.code == code).collect();
        let kind = if exact.is_empty() {
            exact = lex.lookup(&code).take(300).collect();
            "前缀"
        } else {
            "整词"
        };

        exact.sort_by(|a, b| b.freq.cmp(&a.freq).then_with(|| a.text.cmp(&b.text)));

        let shown: Vec<String> = exact
            .iter()
            .take(6)
            .map(|e| format!("{}({})", e.text, e.freq))
            .collect();

        println!(
            "{query:<14} [{code:<22}] {kind} {} 条 | {}",
            exact.len(),
            if shown.is_empty() {
                "无候选".to_string()
            } else {
                shown.join("  ")
            }
        );
    }
}
