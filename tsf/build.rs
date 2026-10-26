// 把仓库根目录 VERSION 里的版本号编进 DLL，日志里就能看出跑的是哪一版。
// 版本号本身由 tools\version.ps1 维护（单一真相源），这里只读不写。
fn main() {
    let version = std::fs::read_to_string("../VERSION")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "0.0.0".to_string());
    println!("cargo:rustc-env=INKLINK_VERSION={version}");
    println!("cargo:rerun-if-changed=../VERSION");
}
