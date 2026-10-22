// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `inklink.exe --serve`：以引擎服务模式运行，供 TSF DLL 通过命名管道查询。
    // 引擎和词库始终留在本进程，绝不进宿主应用（见 docs/adr/0006）。
    if std::env::args().any(|a| a == "--serve") {
        let path = inklink_lib::lexicon_path();
        inklink_lib::engine_server::run(&path);
    }
    inklink_lib::run()
}