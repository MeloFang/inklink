//! 手动验证工具：加载真实词库并做几次前缀查询。
//!
//! 用法（在 app/src-tauri 下）：
//! ```powershell
//! cargo run --example lexicon_check -- ../../data/base.tsv
//! ```

use std::path::PathBuf;
use std::time::Instant;

use inklink_lib::engine::lexicon::Lexicon;

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("../../data/base.tsv"));

    let started = Instant::now();
    let lex = match Lexicon::load(&path) {
        Ok(lex) => lex,
        Err(e) => {
            eprintln!("加载失败 {path:?}: {e}");
            std::process::exit(1);
        }
    };
    let load = started.elapsed();

    println!("文件      : {}", path.display());
    println!("词条数    : {}", lex.len());
    println!("坏行      : {}", lex.skipped());
    println!("加载耗时  : {load:?}");

    for prefix in ["de", "xiangmu", "zhongguo", "jiangzemin", "renmin"] {
        let mut hits = 0usize;
        let t = Instant::now();
        let mut first: Option<&str> = None;
        for (i, e) in lex.lookup(prefix).enumerate() {
            if i == 0 {
                first = Some(e.text.as_str());
            }
            hits += 1;
        }
        println!(
            "查询 {prefix:<10} 命中 {hits:<4} 首条 {:?} 耗时 {:?}",
            first.unwrap_or("-"),
            t.elapsed()
        );
    }
}
