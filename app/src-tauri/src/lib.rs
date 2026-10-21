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
        .candidates(&input, limit.unwrap_or(50))
        .into_iter()
        .map(CandidateDto::from)
        .collect()
}

/// 前端告诉 Rust 当前是否在拼写：决定钩子要不要吞掉退格/数字/标点。
#[tauri::command]
fn set_composing(composing: bool) {
    platform::hook::set_composing(composing);
}

/// 重新贴合光标（输入过程中候选项宽度变化时调用）。
#[tauri::command]
fn reposition_panel(app: AppHandle) {
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    if let Some((caret_x, caret_y, _)) = platform::caret_position()
        && let Ok(Some(monitor)) = win.primary_monitor()
    {
        let size = win
            .outer_size()
            .unwrap_or(tauri::PhysicalSize::new(400, 78));
        let origin = monitor.position();
        let screen = monitor.size();
        let max_x = origin.x + screen.width as i32 - size.width as i32 - 8;
        let max_y = origin.y + screen.height as i32 - size.height as i32 - 8;
        let x = (caret_x - 12).clamp(origin.x, max_x.max(origin.x));
        let y = (caret_y + 8).clamp(origin.y, max_y.max(origin.y));
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }
}
/// 前端按内容宽度调整面板尺寸（候选条要贴合内容，像主流输入法那样）。
#[tauri::command]
fn resize_panel(app: AppHandle, width: f64, height: f64) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.set_size(tauri::LogicalSize::new(width, height));
    }
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
    // 钩子只在面板可见时采集按键
    platform::hook::set_active(visible);
    if !visible {
        let _ = win.set_position(tauri::PhysicalPosition::new(HIDDEN_POS, HIDDEN_POS));
        return;
    }
    // 优先贴着光标显示；拿不到光标就退到屏幕底部居中
    let size = win
        .outer_size()
        .unwrap_or(tauri::PhysicalSize::new(820, 64));
    let mut placed = false;
    let caret = platform::caret_position();
    eprintln!("[panel] caret={caret:?}");
    if let Some((caret_x, caret_y, _)) = caret
        && let Ok(Some(monitor)) = win.primary_monitor()
    {
        {
            let origin = monitor.position();
            let screen = monitor.size();
            let max_x = origin.x + screen.width as i32 - size.width as i32 - 8;
            let max_y = origin.y + screen.height as i32 - size.height as i32 - 8;
            let x = (caret_x - 12).clamp(origin.x, max_x.max(origin.x));
            let y = (caret_y + 8).clamp(origin.y, max_y.max(origin.y));
            let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
            placed = true;
        }
    }
    if !placed && let Ok(Some(monitor)) = win.primary_monitor() {
        let origin = monitor.position();
        let screen = monitor.size();
        // 拿不到光标时贴在鼠标附近（用户通常就在点击处打字），比固定底部更接近真实输入法
        let (mx, my) = platform::cursor_position();
        let x = (mx + 8).clamp(
            origin.x,
            origin.x + screen.width as i32 - size.width as i32 - 8,
        );
        let y = (my + 20).clamp(
            origin.y,
            origin.y + screen.height as i32 - size.height as i32 - 8,
        );
        let _ = win.set_position(tauri::PhysicalPosition::new(x, y));
    }
    // 窗口带 WS_EX_NOACTIVATE：show 不会抢焦点，目标应用保持前台
    let _ = win.show();
    let _ = app.emit("panel-shown", ());
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
/// `keep_open = true` 时上屏后面板保持可见，可以接着打下个词。
///
/// 面板带 `WS_EX_NOACTIVATE`，目标应用一直是前台，所以**不需要切焦点**，
/// 直接 `SendInput` 就会落到目标应用的光标处。
#[tauri::command]
fn commit_text(app: AppHandle, text: String, keep_open: Option<bool>) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    let keep_open = keep_open.unwrap_or(false);

    // 上屏期间让钩子放行（我们自己的字符不能被自己吞掉）
    platform::hook::set_committing(true);
    // 直发字符，失败则走剪贴板
    let ok = if platform::send_unicode(&text) {
        true
    } else {
        app.clipboard().write_text(text.clone()).is_ok() && platform::send_paste()
    };
    std::thread::sleep(std::time::Duration::from_millis(20));
    platform::hook::set_committing(false);
    eprintln!("[commit] text={text:?} ok={ok} keep_open={keep_open}");

    if !keep_open {
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
        .invoke_handler(tauri::generate_handler![
            search,
            hide_panel,
            commit_text,
            resize_panel,
            set_composing,
            reposition_panel
        ])
        .setup(|app| {
            // 面板不抢焦点：目标应用保持前台，光标不消失
            if let Some(win) = app.get_webview_window("main") {
                match win.hwnd() {
                    Ok(hwnd) => platform::make_no_activate(hwnd.0 as isize),
                    Err(e) => eprintln!("取窗口句柄失败: {e}"),
                }
            }

            // 仅测试：允许模拟按键进钩子（正常使用必须关闭）
            if std::env::var("INKLINK_TEST_INJECTED").is_ok() {
                platform::hook::set_allow_injected(true);
                eprintln!("⚠️ 测试模式：接受注入按键");
            }

            // 键盘钩子：面板可见时采集按键，通过事件发给前端
            let (tx, rx) = std::sync::mpsc::channel::<String>();
            let hook_handle = app.handle().clone();
            std::thread::spawn(move || {
                for key in rx {
                    let _ = hook_handle.emit("key", key);
                }
            });
            match platform::hook::start(tx) {
                true => println!("键盘钩子已安装"),
                false => eprintln!("键盘钩子安装失败：面板可能收不到按键"),
            }

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
