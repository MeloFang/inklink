//! 召回与排序预览：给定拼音，打印当前引擎给出的候选、来源与耗时。
//!
//! 用法（在 app/src-tauri 下）：
//! ```powershell
//! cargo run --example preview_rank -- ../../data/base.tsv
//! cargo run --example preview_rank -- ../../data/base.tsv nihao zgrm bjdx
//! ```

use std::path::PathBuf;
use std::time::Instant;

use inklink_lib::engine::lexicon::Lexicon;
use inklink_lib::engine::ranker::{Ranker, Source};

const DEFAULT_QUERIES: &[&str] = &[
    "nihao",
    "xiangmu",
    "zhongguo",
    "shijian",
    "shanghai",
    "yinhang",
    "beijing",
    "chifan",
    "women",
    "zhonggu",
    "zgrm",
    "bjdx",
    "xwlb",
    "yhhz",
    "yinhanghangzhang",
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

    let t = Instant::now();
    let ranker = Ranker::new(lex);
    let build_ms = t.elapsed().as_millis();
    println!("词库 {} 条 | 建索引 {}ms\n", ranker.len(), build_ms);

    let custom: Vec<String> = args.collect();
    let queries: Vec<&str> = if custom.is_empty() {
        DEFAULT_QUERIES.to_vec()
    } else {
        custom.iter().map(String::as_str).collect()
    };

    let mut total_us = 0u128;
    for q in &queries {
        let t = Instant::now();
        let cands = ranker.candidates(q, 6);
        let us = t.elapsed().as_micros();
        total_us += us;

        let head: Vec<String> = cands
            .iter()
            .map(|c| {
                format!(
                    "{}({},{:.1},{})",
                    c.text,
                    c.freq,
                    c.score,
                    match c.source {
                        Source::Exact => "整",
                        Source::Prefix => "前",
                        Source::Abbrev => "简",
                        Source::Fuzzy => "糊",
                    }
                )
            })
            .collect();

        println!(
            "{q:<18} {us:>5}µs | {}",
            if head.is_empty() {
                "无候选".to_string()
            } else {
                head.join("  ")
            }
        );
    }

    println!(
        "\n平均 {:.0}µs/次（共 {} 次）",
        total_us as f64 / queries.len() as f64,
        queries.len()
    );
}
