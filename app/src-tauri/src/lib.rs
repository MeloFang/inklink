//! inklink 面板应用的入口。
//!
//! 铁律（见根目录 AGENTS.md）：
//! 1. 不进入别人的进程；
//! 2. 同步路径 50ms 封顶；
//! 3. 默认不联网。

pub mod engine;
pub mod platform;

use std::path::PathBuf;
use std::sync::Mutex;

use tauri::{AppHandle, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use engine::lexicon::Lexicon;
use engine::ranker::{Candidate, Ranker, Source};

/// 全局状态：词库索引。`None` 表示词库没加载成功（面板仍可开，只是没候选）。
struct AppState(Mutex<Option<Ranker>>);

/// 传给前端的候选（只暴露需要渲染的字段）。
#[derive(serde::Serialize)]
struct CandidateDto {
    text: String,
    pinyin: String,
    freq: u32,
    score: f32,
    source: &'static str,
}

fn source_name(s: Source) -> &'static str {
    match s {
        Source::Exact => "exact",
        Source::Prefix => "prefix",
        Source::Abbrev => "abbrev",
        Source::Fuzzy => "fuzzy",
    }
}

impl From<Candidate> for CandidateDto {
    fn from(c: Candidate) -> Self {
        Self {
            text: c.text,
            pinyin: c.pinyin,
            freq: c.freq,
            score: c.score,
            source: source_name(c.source),
        }
    }
}

/// 前端查询接口。同步路径：必须在 50ms 内返回。
#[tauri::command]
fn search(state: State<'_, AppState>, input: String, limit: Option<usize>) -> Vec<CandidateDto> {
    let guard = state
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(ranker) = guard.as_ref() else {
        return Vec::new();
    };
    ranker
        .candidates(&input, limit.unwrap_or(9))
        .into_iter()
        .map(CandidateDto::from)
        .collect()
}

/// 词库路径：环境变量 > 开发期相对路径 > exe 同级 `data/base.tsv`。
fn lexicon_path() -> PathBuf {
    if let Ok(p) = std::env::var("INKLINK_LEXICON") {
        return PathBuf::from(p);
    }
    let dev = PathBuf::from("../../data/base.tsv");
    if dev.is_file() {
        return dev;
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let p = dir.join("data").join("base.tsv");
        if p.is_file() {
            return p;
        }
    }
    dev
}

fn toggle_panel(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let visible = win.is_visible().unwrap_or(false);
    if visible {
        let _ = win.hide();
    } else {
        let _ = win.show();
        let _ = win.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        toggle_panel(app);
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![search])
        .setup(|app| {
            // 词库加载与建索引放到后台线程：debug 构建要 300–600ms，
            // 放主线程会拖住窗口/WebView2 的创建。
            app.manage(AppState(Mutex::new(None)));
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let path = lexicon_path();
                let ranker = match Lexicon::load(&path) {
                    Ok(lex) => {
                        println!("词库已加载: {} 条 ({})", lex.len(), path.display());
                        Some(Ranker::new(lex))
                    }
                    Err(e) => {
                        eprintln!("词库加载失败 {}: {e}", path.display());
                        None
                    }
                };
                if let Some(state) = handle.try_state::<AppState>() {
                    *state.0.lock().unwrap_or_else(|p| p.into_inner()) = ranker;
                }
            });

            // 全局热键：Ctrl+`
            match app.global_shortcut().register("ctrl+`") {
                Ok(()) => println!("热键已注册: Ctrl+`"),
                Err(e) => eprintln!("热键注册失败: {e}"),
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
