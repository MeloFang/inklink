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

use tauri::{AppHandle, Emitter, Manager, State};
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

const HIDDEN_POS: i32 = -30000;

/// 把面板挪到屏幕外（相当于"隐藏"），或挪回屏幕中央。
///
/// 不用 `hide()/show()`：在 WebView2 上实测 `hide()` 之后 `show()` 虽然返回 Ok、
/// `is_visible()` 也变回 true，但窗口实际再也没出现。挪位置最稳。
fn place_panel(app: &AppHandle, visible: bool) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    if !visible {
        let _ = win.set_position(tauri::PhysicalPosition::new(HIDDEN_POS, HIDDEN_POS));
        return;
    }
    if let Ok(Some(monitor)) = win.primary_monitor() {
        let size = win
            .outer_size()
            .unwrap_or(tauri::PhysicalSize::new(680, 420));
        let origin = monitor.position();
        let screen = monitor.size();
        let x = origin.x + (screen.width as i32 - size.width as i32) / 2;
        let y = origin.y + (screen.height as i32 - size.height as i32) / 3;
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }
    let _ = win.show();
    let _ = win.set_focus();
}

fn toggle_panel(app: &AppHandle) {
    let visible = PANEL_VISIBLE.load(std::sync::atomic::Ordering::SeqCst);
    place_panel(app, !visible);
    PANEL_VISIBLE.store(!visible, std::sync::atomic::Ordering::SeqCst);
}

/// 面板当前是否可见（由我们自己维护，避免依赖窗口 API 的时序）。
static PANEL_VISIBLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    eprintln!("[hotkey] {shortcut:?} state={:?}", event.state());
                    if event.state() == ShortcutState::Pressed {
                        // 热键回调不在主线程：窗口 API 必须切回主线程调用，
                        // 否则会踩到 Tauri 的内部状态直接 fast-fail（0xc0000409）。
                        let handle = app.clone();
                        match app.run_on_main_thread(move || toggle_panel(&handle)) {
                            Ok(()) => eprintln!("[hotkey] 已派发到主线程"),
                            Err(e) => eprintln!("[hotkey] 派发失败: {e}"),
                        }
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
                let (ranker, count) = match Lexicon::load(&path) {
                    Ok(lex) => {
                        let n = lex.len();
                        println!("词库已加载: {n} 条 ({})", path.display());
                        (Some(Ranker::new(lex)), n)
                    }
                    Err(e) => {
                        eprintln!("词库加载失败 {}: {e}", path.display());
                        (None, 0)
                    }
                };
                if let Some(state) = handle.try_state::<AppState>() {
                    *state.0.lock().unwrap_or_else(|p| p.into_inner()) = ranker;
                }
                // 通知前端：索引就绪，可以重新查询了
                let _ = handle.emit("lexicon-ready", count);

                // 就绪后把面板藏起来，进入"常驻托盘、热键唤出"的状态。
                // 注意：不能在窗口创建时就设 visible:false —— WebView2 会创建失败。
                std::thread::sleep(std::time::Duration::from_millis(400));
                place_panel(&handle, false);
                PANEL_VISIBLE.store(false, std::sync::atomic::Ordering::SeqCst);
            });

            // 全局热键：Ctrl+`（用键码显式构造，避免字符串解析歧义）
            let hotkey = tauri_plugin_global_shortcut::Shortcut::new(
                Some(tauri_plugin_global_shortcut::Modifiers::CONTROL),
                tauri_plugin_global_shortcut::Code::Backquote,
            );
            match app.global_shortcut().register(hotkey) {
                Ok(()) => println!("热键已注册: Ctrl+`"),
                Err(e) => eprintln!("热键注册失败: {e}"),
            }

            // 摘掉本窗口的输入法关联：面板自己就是输入法，不能让系统 IME 截走按键
            if let Some(win) = app.get_webview_window("main") {
                match win.hwnd() {
                    Ok(hwnd) => {
                        platform::disable_ime(hwnd.0 as isize);
                        println!("已关闭本窗口的输入法关联");
                    }
                    Err(e) => eprintln!("取窗口句柄失败: {e}"),
                }
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
