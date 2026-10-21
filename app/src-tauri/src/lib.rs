//! inklink 面板应用的入口。
//!
//! 铁律（见根目录 AGENTS.md）：
//! 1. 不进入别人的进程；
//! 2. 同步路径 50ms 封顶；
//! 3. 默认不联网。

mod engine;
mod platform;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
