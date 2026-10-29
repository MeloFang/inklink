//! inklink 常驻 UI（浮条 + 托盘）。
//!
//! 三条硬约束（见 `docs/adr/0007-加常驻UI进程与全局状态.md`）：
//! 1. **不碰输入路径**：这里只是一个读写注册表的窗口。被杀掉、崩掉，输入法照常工作。
//! 2. **不缓存状态**：每次重画、每次弹菜单都现读注册表，这样用 Shift 在应用里切了中英，
//!    浮条 400ms 内跟着变。
//! 3. **不画候选窗**：候选窗留在 DLL 里（它要知道光标在哪，且不能慢）。

#![windows_subsystem = "windows"]

mod menu;
mod state;
mod tray;

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicIsize, Ordering};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    InvalidateRect, RoundRect, SelectObject, SetBkMode, SetTextColor, DT_CENTER,
    DT_SINGLELINE, DT_VCENTER, HBRUSH, HDC, HGDIOBJ, PAINTSTRUCT, TRANSPARENT,
};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::System::SystemInformation::GetTickCount64;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    DrawIconEx, GetClientRect, GetCursorPos, GetMessageW, GetWindowRect, IsWindowVisible, KillTimer,
    LoadCursorW, MoveWindow, PostMessageW, PostQuitMessage, RegisterClassW, SetTimer, ShowWindow,
    SystemParametersInfoW, TranslateMessage, CHANGEFILTERSTRUCT, CS_HREDRAW, CS_VREDRAW, DI_NORMAL,
    IDC_ARROW, MSG, MSGFLT_ALLOW, SPI_GETWORKAREA, SW_HIDE, SW_SHOWNOACTIVATE, WNDCLASSW, WM_APP,
    WM_DESTROY, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONUP, WM_TIMER,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const WINDOW_CLASS: &str = "inklink-ui-bar";
const TIMER_POLL: usize = 1;
const POLL_MS: u32 = 400;
/// 收起浮条的延迟：连续在两个应用之间切换时 `Deactivate`/`Activate` 会成对到达，
/// 立刻隐藏会闪一下，所以等一会儿再收，期间又收到 SHOW 就取消。
const TIMER_HIDE: usize = 2;
const HIDE_DELAY_MS: u32 = 250;
/// 与 `tsf/src/ui_notify.rs` 里的同名常量必须一致。
const WM_UI_SHOW: u32 = WM_APP + 12;
const WM_UI_HIDE: u32 = WM_APP + 13;
/// 弹菜单走这条自投递消息，**不在鼠标消息里直接弹**。
/// 直接弹会在 `TrackPopupMenu` 的模态循环里重入 wndproc（此时还持着 SetCapture 的收尾），
/// 实测会崩在 user32 里（0xc0000005 @ user32!0x24702）。投递到队列尾部再弹就干净了。
const WM_OPEN_MENU: u32 = WM_APP + 20;
static MENU_OPEN: AtomicBool = AtomicBool::new(false);

static TRAY_ADDED: AtomicBool = AtomicBool::new(false);
static WRITE_OK: AtomicBool = AtomicBool::new(true);
static TRAY_TRIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

// 96 DPI 下的设计尺寸，实际按 DPI 缩放
const PAD: i32 = 3;
const ITEM_H: i32 = 24;
const RADIUS: i32 = 6;
const GAP: i32 = 2;
/// 第一格是品牌标（深蓝椭圆 + 白 ink），点它等同点 ☰。
const W_BRAND: i32 = 26;
const W_MODE: i32 = 46;
const W_PUNCT: i32 = 46;
const W_MENU: i32 = 30;

/// 点完浮条后这段时间内不收起：点一下会顺带让 IME 失活（宿主把焦点交给浮条），
/// 于是 250ms 后照常隐藏 —— 用户看到的就是"点一下就没了"。这里让它钉住一会儿。
const STICKY_MS: u64 = 1500;

// COLORREF 是 0x00BBGGRR
const C_ORANGE: u32 = 0x00_00_66_FF;
const C_BG: u32 = 0x00_FA_FA_FA;
const C_ITEM: u32 = 0x00_EC_EC_EC;
const C_ITEM_HOVER: u32 = 0x00_E0_E0_E0;
const C_TEXT: u32 = 0x00_20_20_20;
const C_WHITE: u32 = 0x00_FF_FF_FF;

static CHINESE: AtomicBool = AtomicBool::new(true);
static PUNCT: AtomicBool = AtomicBool::new(true);
static HOVER: AtomicI32 = AtomicI32::new(-1);
static DRAG_ARMED: AtomicBool = AtomicBool::new(false);
static DRAGGING: AtomicBool = AtomicBool::new(false);

/// 最近一次与浮条交互的时刻（`GetTickCount64`，毫秒）。用来推迟"自己消失"。
static LAST_TOUCH: AtomicI64 = AtomicI64::new(0);

fn touch() {
    LAST_TOUCH.store(unsafe { GetTickCount64() } as i64, Ordering::Relaxed);
}

fn recently_touched() -> bool {
    let t = LAST_TOUCH.load(Ordering::Relaxed);
    if t == 0 {
        return false;
    }
    let now = unsafe { GetTickCount64() } as i64;
    now.saturating_sub(t) < STICKY_MS as i64
}

/// 鼠标现在是否停在浮条上（`XP`/`YP` 都是屏幕坐标）。
fn cursor_inside(hwnd: HWND) -> bool {
    let mut pt: POINT = unsafe { std::mem::zeroed() };
    let mut wr: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        GetCursorPos(&mut pt);
        GetWindowRect(hwnd, &mut wr);
    }
    pt.x >= wr.left && pt.x < wr.right && pt.y >= wr.top && pt.y < wr.bottom
}

struct DragState {
    cursor: POINT,
    window: POINT,
    pressed: i32,
}

static mut DRAG: Option<DragState> = None;

/// 日志：GUI 程序没有控制台，出问题只能写文件。
///
/// 放 `%LOCALAPPDATA%\inklink\ui.log`（用户目录一定可写；exe 目录在 Program Files 下
/// 普通用户写不了）。**只写文件，不 spawn 子进程** —— 早期版本在这函数里调 `reg.exe`，
/// 结果每条日志都创建一次进程，在 wndproc 里直接把消息循环拖死。
pub(crate) fn log(msg: &str) {
    use std::io::Write;
    for path in log_paths() {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(f, "[{msg}]");
            return;
        }
    }
}

/// 装一个 panic 钩子，把恐慌信息写进日志。
///
/// GUI 程序没有控制台，`wndproc` 里一 panic 就直接变成 `0xc000041d`
/// （user32 捕获到从回调里逃出来的异常）——事件查看器只告诉你
/// "user32.dll 偏移 0x24702"，一个字的上下文都没有。有了这个钩子至少能看到
/// 恐慌发生在哪一行。
pub(crate) fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            (*s).to_string()
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string payload>".to_string()
        };
        log(&format!("PANIC at {loc}: {payload}"));
        let bt = std::backtrace::Backtrace::force_capture();
        for line in bt.to_string().lines().take(30) {
            log(&format!("PANIC bt {line}"));
        }
    }));
}

/// 依次尝试的日志落点：用户目录 → exe 目录 → `C:\Users\Public`。
/// 只要有一个能写就停（受限进程/被占用的目录都可能写不进去）。
fn log_paths() -> Vec<std::path::PathBuf> {
    let mut v = Vec::new();
    if let Ok(base) = std::env::var("LOCALAPPDATA") {
        let dir = std::path::PathBuf::from(base).join("inklink");
        let _ = std::fs::create_dir_all(&dir);
        v.push(dir.join("ui.log"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join("ui.log"));
        }
    }
    v.push(std::path::PathBuf::from("C:\\Users\\Public\\inklink-ui.log"));
    v
}
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn dpi_scale(hwnd: HWND, v: i32) -> i32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let dpi = if dpi == 0 { 96 } else { dpi as i32 };
    (v * dpi + 48) / 96
}

/// 四个可点的地方：品牌标、中文/英文、中文标点/半角、菜单。
fn item_rects(hwnd: HWND) -> [RECT; 4] {
    let pad = dpi_scale(hwnd, PAD);
    let gap = dpi_scale(hwnd, GAP);
    let h = dpi_scale(hwnd, ITEM_H);
    let widths = [
        dpi_scale(hwnd, W_BRAND),
        dpi_scale(hwnd, W_MODE),
        dpi_scale(hwnd, W_PUNCT),
        dpi_scale(hwnd, W_MENU),
    ];
    let mut rects = [RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    }; 4];
    let mut x = pad;
    for (i, w) in widths.iter().enumerate() {
        rects[i] = RECT {
            left: x,
            top: pad,
            right: x + w,
            bottom: pad + h,
        };
        x += w + gap;
    }
    rects
}

fn window_size(hwnd: HWND) -> (i32, i32) {
    let last = item_rects(hwnd)[3];
    let pad = dpi_scale(hwnd, PAD);
    (last.right + pad, last.bottom + pad)
}

fn hit_test(hwnd: HWND, x: i32, y: i32) -> i32 {
    for (i, r) in item_rects(hwnd).iter().enumerate() {
        if x >= r.left && x < r.right && y >= r.top && y < r.bottom {
            return i as i32;
        }
    }
    -1
}

unsafe fn draw_item(
    hdc: HDC,
    r: &RECT,
    text: &str,
    bg: u32,
    fg: u32,
    font: HGDIOBJ,
    radius: i32,
) {
    let brush: HBRUSH = CreateSolidBrush(bg);
    let old_brush = SelectObject(hdc, brush);
    RoundRect(hdc, r.left, r.top, r.right, r.bottom, radius * 2, radius * 2);
    SelectObject(hdc, old_brush);
    DeleteObject(brush as HGDIOBJ);

    let old_font = SelectObject(hdc, font);
    SetBkMode(hdc, TRANSPARENT as i32);
    SetTextColor(hdc, fg);
    let buf: Vec<u16> = text.encode_utf16().collect();
    let mut tr = *r;
    DrawTextW(
        hdc,
        buf.as_ptr(),
        buf.len() as i32,
        &mut tr,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );
    SelectObject(hdc, old_font);
}

/// 第一格里的品牌标。图标只加载一次（`LoadImageW` 会读盘），之后一直复用。
static BADGE: AtomicIsize = AtomicIsize::new(0);

unsafe fn draw_badge(hdc: HDC, r: &RECT) {
    let mut h = BADGE.load(Ordering::Relaxed);
    if h == 0 {
        // 第一格直接用产品图标（深蓝圆角方块 + 白色 i）：和任务栏、语言列表里
        // 那一枚是同一张图，24px 下也比透明底的字形清楚得多。
        let icon = tray::load_icon();
        if icon.is_null() {
            return;
        }
        h = icon as isize;
        BADGE.store(h, Ordering::Relaxed);
    }
    let side = (r.bottom - r.top).min(r.right - r.left);
    let x = r.left + (r.right - r.left - side) / 2;
    let y = r.top + (r.bottom - r.top - side) / 2;
    DrawIconEx(
        hdc,
        x,
        y,
        h as windows_sys::Win32::UI::WindowsAndMessaging::HICON,
        side,
        side,
        0,
        std::ptr::null_mut(),
        DI_NORMAL,
    );
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut client: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut client);

    let bg = CreateSolidBrush(C_BG);
    FillRect(hdc, &client, bg);
    DeleteObject(bg as HGDIOBJ);

    let dpi = GetDpiForWindow(hwnd).max(96) as i32;
    let font_size = -(dpi * 13 / 96);
    let face = wide("Microsoft YaHei UI");
    let font = CreateFontW(
        font_size,
        0,
        0,
        0,
        500,
        0,
        0,
        0,
        1,
        0,
        0,
        5,
        0,
        face.as_ptr(),
    );

    let rects = item_rects(hwnd);
    let radius = dpi_scale(hwnd, RADIUS);
    let chinese = CHINESE.load(Ordering::Relaxed);
    let punct = PUNCT.load(Ordering::Relaxed);
    let hover = HOVER.load(Ordering::Relaxed);

    // 第一格：品牌标（不带底色，直接画图标）
    if hover == 0 {
        draw_item(hdc, &rects[0], "", C_ITEM_HOVER, C_TEXT, font, radius);
    }
    draw_badge(hdc, &rects[0]);

    let mode_text = if chinese { "中" } else { "英" };
    let (mode_bg, mode_fg) = if chinese {
        (C_ORANGE, C_WHITE)
    } else if hover == 1 {
        (C_ITEM_HOVER, C_TEXT)
    } else {
        (C_ITEM, C_TEXT)
    };
    draw_item(hdc, &rects[1], mode_text, mode_bg, mode_fg, font, radius);

    let punct_text = if punct { "，。" } else { ",." };
    let punct_bg = if hover == 2 { C_ITEM_HOVER } else { C_ITEM };
    let punct_fg = if punct { C_ORANGE } else { C_TEXT };
    draw_item(hdc, &rects[2], punct_text, punct_bg, punct_fg, font, radius);

    let menu_bg = if hover == 3 { C_ITEM_HOVER } else { C_ITEM };
    draw_item(hdc, &rects[3], "☰", menu_bg, C_TEXT, font, radius);

    DeleteObject(font as HGDIOBJ);
    EndPaint(hwnd, &ps);
}

fn default_position(hwnd: HWND) -> (i32, i32) {
    let (w, h) = window_size(hwnd);
    let mut area: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            &mut area as *mut RECT as *mut _,
            0,
        );
    }
    // 右下角往上多让 44px：搜狗/微软的浮条也在右下角，正好压在这里（本机实测：
    // 默认位置被搜狗的 `SoPY_Status` 完全盖住，点都点不到）。用户拖走一次就记住位置。
    (area.right - w - 24, area.bottom - h - 24 - 44)
}


fn toggle_mode(hwnd: HWND) {
    let on = !CHINESE.load(Ordering::Relaxed);
    note_write(state::write_flag("Chinese", on));
    CHINESE.store(on, Ordering::Relaxed);
    unsafe {
        InvalidateRect(hwnd, std::ptr::null(), 0);
    }
}

fn toggle_punct(hwnd: HWND) {
    let on = !PUNCT.load(Ordering::Relaxed);
    note_write(state::write_flag("Punctuation", on));
    PUNCT.store(on, Ordering::Relaxed);
    unsafe {
        InvalidateRect(hwnd, std::ptr::null(), 0);
    }
}

/// 记一笔「注册表能不能写」。
///
/// 写不进去的时候（受限进程、被安全软件拦、注册表被锁），**不能再拿注册表回灌 UI** ——
/// 否则用户点一下、400ms 后自己变回去，看起来就是"点了没用"。这里一旦发现写失败，
/// 就以进程内状态为准，同步逻辑跳过读取。
fn note_write(rc: u32) {
    if rc != 0 && WRITE_OK.swap(false, Ordering::Relaxed) {
        log(&format!("registry write failed rc={rc} -> 以进程内状态为准"));
    }
}

fn sync_from_registry(hwnd: HWND) {
    if !WRITE_OK.load(Ordering::Relaxed) {
        return;
    }
    let chinese = state::read_flag("Chinese");
    let punct = state::read_flag("Punctuation");
    let changed = chinese != CHINESE.load(Ordering::Relaxed)
        || punct != PUNCT.load(Ordering::Relaxed);
    if changed {
        CHINESE.store(chinese, Ordering::Relaxed);
        PUNCT.store(punct, Ordering::Relaxed);
        unsafe {
            InvalidateRect(hwnd, std::ptr::null(), 0);
        }
    }
}

/// 轮询 DLL 写的显隐序号（UIPI 兜底通道）。
///
/// 用窗口消息通知是首选，但 `PostMessageW` 从普通权限进程发到管理员权限的窗口会被
/// UIPI 静默丢弃 —— 而浮条通常正是安装器以管理员身份拉起来的。注册表两边都够权限，
/// 所以 DLL 每次启停都 +1，这里 400ms 看一眼，谁更新就执行谁。
static LAST_SHOW_SEQ: AtomicI32 = AtomicI32::new(0);
static LAST_HIDE_SEQ: AtomicI32 = AtomicI32::new(0);

fn poll_activation(hwnd: HWND) {
    let show_seq = state::read_seq("ShowSeq");
    let hide_seq = state::read_seq("HideSeq");
    let show_changed = show_seq != LAST_SHOW_SEQ.load(Ordering::Relaxed);
    let hide_changed = hide_seq != LAST_HIDE_SEQ.load(Ordering::Relaxed);
    LAST_SHOW_SEQ.store(show_seq, Ordering::Relaxed);
    LAST_HIDE_SEQ.store(hide_seq, Ordering::Relaxed);
    if show_changed {
        unsafe {
            KillTimer(hwnd, TIMER_HIDE);
            if IsWindowVisible(hwnd) == 0 {
                ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                InvalidateRect(hwnd, std::ptr::null(), 0);
                log("ui show (poll)");
            }
        }
    } else if hide_changed {
        unsafe { SetTimer(hwnd, TIMER_HIDE, HIDE_DELAY_MS, None) };
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // 输入法被切到前台：出现（像搜狗那样，用的时候才有）
        WM_UI_SHOW => {
            unsafe { KillTimer(hwnd, TIMER_HIDE) };
            if unsafe { IsWindowVisible(hwnd) } == 0 {
                unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
                unsafe { InvalidateRect(hwnd, std::ptr::null(), 0) };
                log("ui show");
            }
            0
        }
        // 输入法被切走：延迟收起，避免 A→B 连续切换时闪一下
        WM_UI_HIDE => {
            unsafe { SetTimer(hwnd, TIMER_HIDE, HIDE_DELAY_MS, None) };
            0
        }
        WM_PAINT => {
            log("paint");
            paint(hwnd);
            0
        }
        WM_TIMER => {
            if wparam == TIMER_HIDE {
                unsafe {
                    KillTimer(hwnd, TIMER_HIDE);
                    // 刚点过、或者鼠标还停在浮条上就先不收起：点一下会让宿主失活、
                    // 我们随即收到 WM_UI_HIDE，用户看到的就是"点一下就没了"。
                    if recently_touched() || cursor_inside(hwnd) {
                        log("ui hide skipped (sticky)");
                    } else if IsWindowVisible(hwnd) != 0 {
                        ShowWindow(hwnd, SW_HIDE);
                        log("ui hide");
                    }
                }
            }
            if wparam == TIMER_POLL {
                // 托盘图标延迟到消息循环起来之后再加：`Shell_NotifyIconW` 会同步问一次
                // 任务栏，Explorer 忙的时候能把调用方卡住（本机实测过一次，卡在启动路径上
                // 就是整个浮条不出现）。这里失败就下个 tick 再试，最多试几次。
                if !TRAY_ADDED.load(Ordering::Relaxed) {
                    let tries = TRAY_TRIES.fetch_add(1, Ordering::Relaxed);
                    if tries < 5 {
                        if tray::add(hwnd) {
                            TRAY_ADDED.store(true, Ordering::Relaxed);
                            log("tray added");
                        } else if tries == 4 {
                            log("tray add gave up");
                        }
                    }
                }
                sync_from_registry(hwnd);
                poll_activation(hwnd);
            }
            0
        }
        WM_MOUSEMOVE => {
            let x = (lparam & 0xFFFF) as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let hit = hit_test(hwnd, x, y);
            if hit != HOVER.load(Ordering::Relaxed) {
                HOVER.store(hit, Ordering::Relaxed);
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
            if DRAG_ARMED.load(Ordering::Relaxed) {
                let mut pt: POINT = std::mem::zeroed();
                GetCursorPos(&mut pt);
                let (dx, dy) = match (&raw const DRAG).read() {
                    Some(d) => (pt.x - d.cursor.x, pt.y - d.cursor.y),
                    None => (0, 0),
                };
                if !DRAGGING.load(Ordering::Relaxed) && (dx.abs() > 3 || dy.abs() > 3) {
                    DRAGGING.store(true, Ordering::Relaxed);
                    HOVER.store(-1, Ordering::Relaxed);
                }
                if DRAGGING.load(Ordering::Relaxed) {
                    if let Some(d) = (&raw const DRAG).read() {
                        MoveWindow(hwnd, d.window.x + dx, d.window.y + dy, 0, 0, 0);
                    }
                }
            }
            0
        }
        WM_LBUTTONDOWN => {
            let x = (lparam & 0xFFFF) as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            log(&format!("LBUTTONDOWN x={x} y={y} hit={}", hit_test(hwnd, x, y)));
            touch();
            let mut cursor: POINT = std::mem::zeroed();
            let mut wr: RECT = std::mem::zeroed();
            GetCursorPos(&mut cursor);
            GetWindowRect(hwnd, &mut wr);
            (&raw mut DRAG).write(Some(DragState {
                cursor,
                window: POINT {
                    x: wr.left,
                    y: wr.top,
                },
                pressed: hit_test(hwnd, x, y),
            }));
            DRAG_ARMED.store(true, Ordering::Relaxed);
            DRAGGING.store(false, Ordering::Relaxed);
            SetCapture(hwnd);
            0
        }
        WM_LBUTTONUP => {
            log("LBUTTONUP");
            ReleaseCapture();
            DRAG_ARMED.store(false, Ordering::Relaxed);
            let dragging = DRAGGING.load(Ordering::Relaxed);
            DRAGGING.store(false, Ordering::Relaxed);
            let pressed = (&raw const DRAG).read().map(|d| d.pressed).unwrap_or(-1);
            (&raw mut DRAG).write(None);
            if dragging {
                let mut wr: RECT = std::mem::zeroed();
                GetWindowRect(hwnd, &mut wr);
                state::write_pos(wr.left, wr.top);
            } else {
                match pressed {
                    // 品牌标：等同点 ☰，把菜单弹出来（投递到队列，不在这里直接弹）
                    0 | 3 => {
                        unsafe {
                            PostMessageW(hwnd, WM_OPEN_MENU, 0, 0);
                        }
                    }
                    1 => toggle_mode(hwnd),
                    2 => toggle_punct(hwnd),
                    _ => {}
                }
            }
            0
        }
        WM_RBUTTONUP | tray::WM_TRAY => {
            // 托盘右键 / 浮条右键：同样投递到队列，理由见 WM_OPEN_MENU 的注释
            let event = lparam as u32;
            const WM_LBUTTONUP_CB: u32 = 0x0202;
            if msg == tray::WM_TRAY && event == WM_LBUTTONUP_CB {
                let visible = windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible(hwnd);
                ShowWindow(hwnd, if visible != 0 { 0 } else { SW_SHOWNOACTIVATE });
            } else if msg == WM_RBUTTONUP || event == 0x0205 {
                unsafe {
                    PostMessageW(hwnd, WM_OPEN_MENU, 0, 0);
                }
            }
            0
        }
        WM_OPEN_MENU => {
            // 菜单自己跑消息循环（会重入 wndproc），用 MENU_OPEN 挡掉重复弹出
            if !MENU_OPEN.swap(true, Ordering::SeqCst) {
                let mut wr: RECT = std::mem::zeroed();
                GetWindowRect(hwnd, &mut wr);
                let cmd = menu::popup(
                    hwnd,
                    wr.left,
                    wr.top,
                    CHINESE.load(Ordering::Relaxed),
                    PUNCT.load(Ordering::Relaxed),
                    state::autostart_enabled(),
                );
                MENU_OPEN.store(false, Ordering::SeqCst);
                if cmd != 0 {
                    log(&format!("menu choice {cmd}"));
                }
                handle_menu(hwnd, cmd);
            }
            0
        }
        WM_DESTROY => {
            log("wm_destroy");
            tray::remove(hwnd);
            PostQuitMessage(0);
            0
        }
        // 注意：不要自己写 `WM_CLOSE => ...` 的分支 —— 之前漏了 import，Rust 把
        // 它当成了**新的绑定变量**（irrefutable pattern），于是每条消息都被当成
        // 关闭请求、窗口在 WM_NCCREATE 阶段就被 DestroyWindow 掉，CreateWindowExW
        // 直接失败。DefWindowProcW 对 WM_CLOSE 的默认处理就是 DestroyWindow，够了。
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn handle_menu(hwnd: HWND, cmd: u32) {
    match cmd {
        menu::CMD_TOGGLE_MODE => toggle_mode(hwnd),
        menu::CMD_TOGGLE_PUNCT => toggle_punct(hwnd),
        menu::CMD_TOGGLE_AUTOSTART => {
            let on = !state::autostart_enabled();
            state::set_autostart(on);
        }
        menu::CMD_QUIT => unsafe {
            DestroyWindow(hwnd);
        },
        _ => {}
    }
}

fn main() {
    install_panic_hook();
    log("main enter");
    // 每显示器 DPI 感知：不声明的话 Windows 会把窗口按 125%/150% 拉伸（字发虚），
    // 而且我们拿到的坐标是逻辑坐标、截屏是物理像素，排查时对不上。
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    // startup marker

    if state::already_running() {
        log("already running -> exit");
        return;
    }

    unsafe {
        let instance = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
        let class_name = wide(WINDOW_CLASS);
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance,
            hIcon: std::ptr::null_mut(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class_name.as_ptr(),
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class_name.as_ptr(),
            wide("inklink").as_ptr(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            log("CreateWindowExW 失败");
            return;
        }
        log("window created");

        // UIPI：浮条通常是安装器（管理员）拉起来的，而打字的应用是普通权限。
        // 普通权限进程 `PostMessageW` 到高完整性窗口会被**静默丢弃**，表现就是
        // "切到输入法了但浮条从来不出现"。`WM_APP+12/13` 属于 `WM_USER` 以上，
        // 必须显式放开，否则这条消息永远收不到。
        for msg in [WM_UI_SHOW, WM_UI_HIDE] {
            let mut cf = CHANGEFILTERSTRUCT {
                cbSize: std::mem::size_of::<CHANGEFILTERSTRUCT>() as u32,
                ExtStatus: 0,
            };
            let ok = ChangeWindowMessageFilterEx(hwnd, msg, MSGFLT_ALLOW, &mut cf);
            log(&format!("msg filter {msg:#x} -> {ok}"));
        }

        let (w, h) = window_size(hwnd);
        log(&format!("size = {w}x{h}"));
        // 先把当前序号记下来：否则第一条 400ms 轮询就会把"历史上的某次激活"当成新事件，
        // 开机后浮条会自己冒出来（用户要的是"切到输入法才出现"）。
        LAST_SHOW_SEQ.store(state::read_seq("ShowSeq"), Ordering::Relaxed);
        LAST_HIDE_SEQ.store(state::read_seq("HideSeq"), Ordering::Relaxed);
        let (x, y) = state::read_pos().unwrap_or_else(|| default_position(hwnd));
        log(&format!("pos = {x},{y}"));
        MoveWindow(hwnd, x, y, w, h, 0);
        log(&format!("moved to {x},{y}"));

        CHINESE.store(state::read_flag("Chinese"), Ordering::Relaxed);
        PUNCT.store(state::read_flag("Punctuation"), Ordering::Relaxed);

        log("before show");
        // 启动时**不显示**：浮条要等输入法真正被切到前台（DLL 的 `Activate` 发消息）才出来。
        // 想立刻看一眼可以用托盘图标左键。
        log("start hidden (wait for IME activation)");
        // 托盘**不在启动路径上**做：`Shell_NotifyIconW` 会同步问一次任务栏，Explorer 忙的时候
        // 能把整个启动卡住（本机实测过一次）。放到定时器第一个 tick 里加，失败了下一个 tick 再试。
        SetTimer(hwnd, TIMER_POLL, POLL_MS, None);
        log("timer set");


        log("message loop enter");
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}
