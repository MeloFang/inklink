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
use tauri_plugin_clipboard_manager::ClipboardExt;
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

/// 前端请求隐藏面板（Esc）。必须走这里，不能直接调 `window.hide()`——
/// 否则 `PANEL_VISIBLE` 不会更新，下次热键会被当成"该隐藏"，看起来就像热键失灵。
#[tauri::command]
fn hide_panel(app: AppHandle) {
    place_panel(&app, false);
    PANEL_VISIBLE.store(false, std::sync::atomic::Ordering::SeqCst);
}

fn toggle_panel(app: &AppHandle) {
    let visible = PANEL_VISIBLE.load(std::sync::atomic::Ordering::SeqCst);
    if !visible {
        // 唤出前记住前台窗口，上屏时要还回去
        let target = platform::foreground_window();
        if target != 0 {
            TARGET_WINDOW.store(target, std::sync::atomic::Ordering::SeqCst);
        }
    }
    place_panel(app, !visible);
    PANEL_VISIBLE.store(!visible, std::sync::atomic::Ordering::SeqCst);
}

/// 上屏的目标窗口（唤出面板前的前台窗口）。
static TARGET_WINDOW: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// 把选中的候选写进目标应用。
///
/// `keep_open = true` 时上屏后把焦点收回面板，支持连续输入（否则打一个词就得重新按热键）。
/// 顺序很关键：焦点先给目标窗口 → 发字符 → 再把焦点抢回来，否则字符会打到自己的窗口里。
#[tauri::command]
fn commit_text(app: AppHandle, text: String, keep_open: Option<bool>) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    let keep_open = keep_open.unwrap_or(false);

    // 1. 焦点还给唤起前的窗口
    let target = TARGET_WINDOW.load(std::sync::atomic::Ordering::SeqCst);
    platform::focus_window(target);
    std::thread::sleep(std::time::Duration::from_millis(35));
    let now_fg = platform::foreground_window();
    eprintln!(
        "[commit] text={text:?} target={target} fg_after_focus={now_fg} keep_open={keep_open}"
    );

    // 2. 直发字符，失败则走剪贴板
    let ok = if platform::send_unicode(&text) {
        true
    } else {
        app.clipboard().write_text(text).is_ok() && platform::send_paste()
    };

    // 3. 连续模式：把焦点收回面板，接着打下个字
    if keep_open {
        if let Some(win) = app.get_webview_window("main") {
            let _ = win.set_focus();
        }
    } else {
        place_panel(&app, false);
        PANEL_VISIBLE.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    if ok {
        Ok(())
    } else {
        Err("上屏失败：目标窗口可能不允许模拟输入".into())
    }
}

/// 面板当前是否可见（由我们自己维护，避免依赖窗口 API 的时序）。
static PANEL_VISIBLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
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
        .invoke_handler(tauri::generate_handler![search, hide_panel, commit_text])
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

            // 全局热键：同时注册几个，谁先按到算谁
            // （` 是 Tab 上方、数字 1 左边那个键，常被误认成单引号）
            use tauri_plugin_global_shortcut::{Code, Modifiers, Shortcut};
            let combos = [
                (Modifiers::CONTROL, Code::Backquote, "Ctrl+`"),
                (Modifiers::ALT, Code::Backquote, "Alt+`"),
                (
                    Modifiers::CONTROL | Modifiers::SHIFT,
                    Code::Backquote,
                    "Ctrl+Shift+`",
                ),
            ];
            for (mods, code, label) in combos {
                match app
                    .global_shortcut()
                    .register(Shortcut::new(Some(mods), code))
                {
                    Ok(()) => println!("热键已注册: {label}"),
                    Err(e) => eprintln!("热键注册失败 {label}: {e}"),
                }
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
