//! 浮条：宿主进程内的四格状态条（品牌标 | 中/英 | ，。 | ☰）。
//!
//! 为什么在 DLL 里画、而不是独立进程（用户拍板 m09409「走A」，见对话与 ADR）：
//! - 搜狗、微软拼音都把状态条画在**宿主进程里**（每个宿主各一份 `SoPY_Status` 窗口），
//!   没有"另一个程序常驻"。切到输入法 `Activate` 就出现，切走 `Deactivate` 就消失。
//! - 独立进程（方案 B）的问题：用的时候进程照样在、冷启动 100–300ms 延迟、
//!   空闲退出时机难定、多应用并发抢拉起会堆出多个实例、托盘反复加删像故障。
//!
//! ## 护栏（这个窗口直接跑在别人进程里，必须比独立 exe 更小心）
//!
//! - 纯 GDI 自绘，不起线程、不碰词库、不调 COM、不在 wndproc 里做 IPC；
//! - `tsf/Cargo.toml` 的 `[profile.release] panic = "abort"` 让 `catch_unwind` 失效，
//!   所以 wndproc 里**绝不能 panic**：不 unwrap/expect、不越界索引、只用常量尺寸；
//! - 点击先写注册表、再靠消息循环重绘；状态每次 `paint` 现读 `state::snapshot()`，
//!   不缓存 —— 用 Shift 在应用里切了中英，浮条下次重绘就是最新的；
//! - 命中测试 / 尺寸是纯函数，可单测。

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
    InvalidateRect, RoundRect, SelectObject, SetBkMode, SetTextColor, DT_CENTER, DT_SINGLELINE,
    DT_VCENTER, HBRUSH, HDC, HGDIOBJ, PAINTSTRUCT, TRANSPARENT,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DrawIconEx, GetClientRect, GetCursorPos, GetWindowRect,
    IsWindowVisible, LoadCursorW, LoadIconW, LoadImageW, MoveWindow, PostMessageW, RegisterClassW,
    ShowWindow, SystemParametersInfoW, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, DI_NORMAL, HICON,
    IDC_ARROW, IDI_APPLICATION, IMAGE_ICON, LR_LOADFROMFILE, MA_NOACTIVATE, SPI_GETWORKAREA,
    SW_HIDE, SW_SHOWNOACTIVATE, WNDCLASSW, WM_APP, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_POPUP,
};

const CLASS_NAME: &str = "inklink-bar";
/// 弹菜单走这条自投递消息，**不在鼠标消息里直接弹**：菜单要自己跑一个嵌套消息循环，
/// 直接在 `WM_LBUTTONUP` 里进会重入 wndproc，历史教训是崩在 user32。投到队列尾部再弹就干净。
const WM_OPEN_MENU: u32 = WM_APP + 20;
static MENU_OPEN: AtomicBool = AtomicBool::new(false);

static HWND_CELL: AtomicIsize = AtomicIsize::new(0);
static REGISTERED: AtomicBool = AtomicBool::new(false);
static HOVER: AtomicI32 = AtomicI32::new(-1);
static DRAG_ARMED: AtomicBool = AtomicBool::new(false);
static DRAGGING: AtomicBool = AtomicBool::new(false);
static FONT: AtomicIsize = AtomicIsize::new(0);

// 96 DPI 下的设计尺寸，实际按 DPI 缩放
const PAD: i32 = 3;
const ITEM_H: i32 = 24;
const RADIUS: i32 = 6;
const GAP: i32 = 2;
/// 第一格是品牌标（深蓝圆角方块 + 白色 i），点它等同点 ☰。
const W_BRAND: i32 = 26;
const W_MODE: i32 = 46;
const W_PUNCT: i32 = 46;
const W_MENU: i32 = 30;

// COLORREF 是 0x00BBGGRR
const C_ORANGE: u32 = 0x00_00_66_FF;
const C_BG: u32 = 0x00_FA_FA_FA;
const C_ITEM: u32 = 0x00_EC_EC_EC;
const C_ITEM_HOVER: u32 = 0x00_E0_E0_E0;
const C_TEXT: u32 = 0x00_20_20_20;
const C_WHITE: u32 = 0x00_FF_FF_FF;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn scale(v: i32) -> i32 {
    let dpi = unsafe { GetDpiForSystem() };
    let dpi = if dpi == 0 { 96 } else { dpi as i32 };
    (v * dpi + 48) / 96
}

/// 四个可点的地方：品牌标、中文/英文、中文标点/半角、菜单。纯函数，可单测。
fn item_rects() -> [RECT; 4] {
    let pad = scale(PAD);
    let gap = scale(GAP);
    let h = scale(ITEM_H);
    let widths = [
        scale(W_BRAND),
        scale(W_MODE),
        scale(W_PUNCT),
        scale(W_MENU),
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

fn window_size() -> (i32, i32) {
    let last = item_rects()[3];
    let pad = scale(PAD);
    (last.right + pad, last.bottom + pad)
}

fn hit_test(x: i32, y: i32) -> i32 {
    for (i, r) in item_rects().iter().enumerate() {
        if x >= r.left && x < r.right && y >= r.top && y < r.bottom {
            return i as i32;
        }
    }
    -1
}

fn invalidate(hwnd: HWND) {
    unsafe {
        InvalidateRect(hwnd, std::ptr::null(), 0);
    }
}

/// 浮条出现（`Activate` 时由 lib.rs 调，在宿主 UI 线程上，直接 ShowWindow 即可）。
pub fn show() {
    unsafe {
        ensure_window();
        let raw = HWND_CELL.load(Ordering::SeqCst);
        if raw == 0 {
            return;
        }
        let hwnd = raw as HWND;
        if IsWindowVisible(hwnd) == 0 {
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            invalidate(hwnd);
            crate::log_line("bar show");
        }
    }
}

/// 浮条消失（`Deactivate` 时调）。
pub fn hide() {
    unsafe {
        let raw = HWND_CELL.load(Ordering::SeqCst);
        if raw == 0 {
            return;
        }
        let hwnd = raw as HWND;
        if IsWindowVisible(hwnd) != 0 {
            ShowWindow(hwnd, SW_HIDE);
            crate::log_line("bar hide");
        }
    }
}

/// 重绘（Shift 在应用里切了中英之后，lib.rs 调这个让浮条跟一下）。
pub fn refresh() {
    unsafe {
        let raw = HWND_CELL.load(Ordering::SeqCst);
        if raw == 0 {
            return;
        }
        let hwnd = raw as HWND;
        if IsWindowVisible(hwnd) != 0 {
            invalidate(hwnd);
        }
    }
}

/// 注册窗口类 + 创建窗口（幂等）。位置只取一次：拖动之后窗口一直在，下次 show 不重读。
fn ensure_window() {
    unsafe {
        if !REGISTERED.swap(true, Ordering::SeqCst) {
            let name = wide(CLASS_NAME);
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: std::ptr::null_mut(),
                hIcon: std::ptr::null_mut(),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: name.as_ptr(),
            };
            let _ = RegisterClassW(&wc);
        }

        if HWND_CELL.load(Ordering::SeqCst) == 0 {
            let name = wide(CLASS_NAME);
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                name.as_ptr(),
                name.as_ptr(),
                WS_POPUP,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                10,
                10,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            if hwnd.is_null() {
                crate::log_line("bar: CreateWindowExW failed");
                return;
            }
            HWND_CELL.store(hwnd as isize, Ordering::SeqCst);
            let (w, h) = window_size();
            let (x, y) = crate::state::read_pos().unwrap_or_else(default_position);
            MoveWindow(hwnd, x, y, w, h, 0);
            crate::log_line(&format!("bar: created {w}x{h} at {x},{y}"));
        }
    }
}

fn default_position() -> (i32, i32) {
    let (w, h) = window_size();
    let mut area: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut area as *mut RECT as *mut _, 0);
    }
    // 右下角往上多让 44px：搜狗/微软的浮条也在右下角（本机实测默认位置会被
    // 搜狗的 `SoPY_Status` 完全盖住）。用户拖走一次就记住位置。
    (area.right - w - 24, area.bottom - h - 24 - 44)
}

/// 第一格里的品牌标。图标只加载一次（`LoadImageW` 会读盘），之后一直复用。
static BADGE: AtomicIsize = AtomicIsize::new(0);

fn load_badge() -> HICON {
    // 用 DLL 同目录下的 `inklink.ico`（安装器一定拷过去），没有就退回系统默认图标。
    let dll = crate::registry::dll_path();
    let dir = std::path::Path::new(&dll)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path = wide(&format!("{dir}\\inklink.ico"));
    let h = unsafe {
        LoadImageW(
            std::ptr::null_mut(),
            path.as_ptr(),
            IMAGE_ICON,
            0,
            0,
            LR_LOADFROMFILE,
        )
    };
    if !h.is_null() {
        return h as HICON;
    }
    unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) as HICON }
}

unsafe fn draw_badge(hdc: HDC, r: &RECT) {
    let mut h = BADGE.load(Ordering::Relaxed);
    if h == 0 {
        let icon = load_badge();
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
        h as HICON,
        side,
        side,
        0,
        std::ptr::null_mut(),
        DI_NORMAL,
    );
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
    // **必须补 NUL 再传 `len-1`**：空字符串时 `encode_utf16().collect()` 出来的是零长
    // `Vec`，`as_ptr()` 是悬垂指针（值恰好是 2 = align_of::<u16>()）。以前这里直接传
    // `buf.as_ptr()` + `len`，悬停第 0 格（品牌格用空串）时 `DrawTextW` 会去读 0x2，
    // 宿主进程就地 AV：`c0000005`，`ExceptionInformation[1] = 0x2`，
    // 报在 `user32.dll+0x24702`，从窗口回调逃出去就成了 `c000041d`。
    // 见 `C:\Users\Administrator\AppData\Local\CrashDumps\inklink-ui.exe.2004.dmp`。
    // **必须补 NUL 再传 `len-1`**：空字符串时 `encode_utf16().collect()` 出来的是零长
    // `Vec`，`as_ptr()` 是悬垂指针（值恰好是 2 = align_of::<u16>()）。以前这里直接传
    // `buf.as_ptr()` + `len`，悬停第 0 格（品牌格用空串）时 `DrawTextW` 会去读 0x2，
    // 宿主进程就地 AV：`c0000005`，`ExceptionInformation[1] = 0x2`，
    // 报在 `user32.dll+0x24702`，从窗口回调逃出去就成了 `c000041d`。
    // 见 `C:\Users\Administrator\AppData\Local\CrashDumps\inklink-ui.exe.2004.dmp`。
    let buf: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let mut tr = *r;
    DrawTextW(
        hdc,
        buf.as_ptr(),
        (buf.len() - 1) as i32,
        &mut tr,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
    );
    SelectObject(hdc, old_font);
}

fn get_font() -> HGDIOBJ {
    unsafe {
        let existing = FONT.load(Ordering::SeqCst);
        if existing != 0 {
            return existing as HGDIOBJ;
        }
        let face = wide("Microsoft YaHei UI");
        let f = CreateFontW(
            -scale(13),
            0,
            0,
            0,
            500,
            0,
            0,
            0,
            1, // DEFAULT_CHARSET
            0,
            0,
            5, // CLEARTYPE_QUALITY
            0,
            face.as_ptr(),
        );
        FONT.store(f as isize, Ordering::SeqCst);
        f as HGDIOBJ
    }
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(hwnd, &mut ps);
    let mut client: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut client);

    let bg = CreateSolidBrush(C_BG);
    FillRect(hdc, &client, bg);
    DeleteObject(bg as HGDIOBJ);

    let font = get_font();
    let rects = item_rects();
    let radius = scale(RADIUS);
    // 每次现读：Shift 在别的动作里切了中英，这里立刻反映；不缓存。
    let (chinese, punct) = crate::state::snapshot();
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

    EndPaint(hwnd, &ps);
}

fn toggle_mode(hwnd: HWND) {
    let on = !crate::state::chinese();
    crate::state::set_chinese(on);
    crate::log_line(&format!("bar toggle chinese -> {on}"));
    invalidate(hwnd);
}

fn toggle_punct(hwnd: HWND) {
    let on = !crate::state::punctuation();
    crate::state::set_punctuation(on);
    crate::log_line(&format!("bar toggle punct -> {on}"));
    invalidate(hwnd);
}

struct DragState {
    cursor: POINT,
    window: POINT,
    pressed: i32,
}

static DRAG: Mutex<Option<DragState>> = Mutex::new(None);

fn handle_menu(hwnd: HWND, cmd: u32) {
    match cmd {
        crate::menu::CMD_TOGGLE_MODE => toggle_mode(hwnd),
        crate::menu::CMD_TOGGLE_PUNCT => toggle_punct(hwnd),
        _ => {}
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_MOUSEMOVE => {
            let x = (lparam & 0xFFFF) as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let hit = hit_test(x, y);
            if hit != HOVER.load(Ordering::Relaxed) {
                HOVER.store(hit, Ordering::Relaxed);
                invalidate(hwnd);
            }
            if DRAG_ARMED.load(Ordering::Relaxed) {
                let mut pt: POINT = std::mem::zeroed();
                GetCursorPos(&mut pt);
                let (dx, dy) = DRAG
                    .lock()
                    .map(|d| d.as_ref().map(|s| (pt.x - s.cursor.x, pt.y - s.cursor.y)).unwrap_or((0, 0)))
                    .unwrap_or((0, 0));
                if !DRAGGING.load(Ordering::Relaxed) && (dx.abs() > 3 || dy.abs() > 3) {
                    DRAGGING.store(true, Ordering::Relaxed);
                    HOVER.store(-1, Ordering::Relaxed);
                }
                if DRAGGING.load(Ordering::Relaxed) {
                    // 尺寸必须**原样传回**：`MoveWindow` 的 nWidth/nHeight 是新尺寸，
                    // 传 0 会把窗口缩成 0x0（浮条"拖一下就不见了"）。
                    let (w, h) = window_size();
                    if let Some(d) = DRAG.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
                        MoveWindow(hwnd, d.window.x + dx, d.window.y + dy, w, h, 0);
                    }
                }
            }
            0
        }
        WM_LBUTTONDOWN => {
            let x = (lparam & 0xFFFF) as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let mut cursor: POINT = std::mem::zeroed();
            let mut wr: RECT = std::mem::zeroed();
            GetCursorPos(&mut cursor);
            GetWindowRect(hwnd, &mut wr);
            *DRAG.lock().unwrap_or_else(|p| p.into_inner()) = Some(DragState {
                cursor,
                window: POINT {
                    x: wr.left,
                    y: wr.top,
                },
                pressed: hit_test(x, y),
            });
            DRAG_ARMED.store(true, Ordering::Relaxed);
            DRAGGING.store(false, Ordering::Relaxed);
            SetCapture(hwnd);
            0
        }
        WM_LBUTTONUP => {
            ReleaseCapture();
            DRAG_ARMED.store(false, Ordering::Relaxed);
            let dragging = DRAGGING.load(Ordering::Relaxed);
            DRAGGING.store(false, Ordering::Relaxed);
            let pressed = DRAG
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .map(|d| d.pressed)
                .unwrap_or(-1);
            *DRAG.lock().unwrap_or_else(|p| p.into_inner()) = None;
            if dragging {
                let mut wr: RECT = std::mem::zeroed();
                GetWindowRect(hwnd, &mut wr);
                crate::state::write_pos(wr.left, wr.top);
            } else {
                match pressed {
                    // 品牌标：等同点 ☰，把菜单弹出来（投递到队列，不在这里直接弹）
                    0 | 3 => {
                        PostMessageW(hwnd, WM_OPEN_MENU, 0, 0);
                    }
                    1 => toggle_mode(hwnd),
                    2 => toggle_punct(hwnd),
                    _ => {}
                }
            }
            0
        }
        WM_OPEN_MENU => {
            // 菜单自己跑消息循环（会重入 wndproc），用 MENU_OPEN 挡掉重复弹出
            if !MENU_OPEN.swap(true, Ordering::SeqCst) {
                let mut wr: RECT = std::mem::zeroed();
                GetWindowRect(hwnd, &mut wr);
                let (chinese, punct) = crate::state::snapshot();
                let cmd = crate::menu::popup(hwnd, wr.left, wr.top, chinese, punct);
                MENU_OPEN.store(false, Ordering::SeqCst);
                if cmd != 0 {
                    crate::log_line(&format!("bar: menu choice {cmd}"));
                }
                handle_menu(hwnd, cmd);
            }
            0
        }
        // 注意：不要自己写 `WM_CLOSE => ...` 的分支 —— 之前漏了 import，Rust 把
        // 它当成了**新的绑定变量**（irrefutable pattern），于是每条消息都被当成
        // 关闭请求、窗口在 WM_NCCREATE 阶段就被 DestroyWindow 掉。DefWindowProcW
        // 对 WM_CLOSE 的默认处理就是 DestroyWindow，够了。
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 四格从左到右不重叠() {
        let rects = item_rects();
        for i in 1..4 {
            assert!(rects[i].left >= rects[i - 1].right, "第 {i} 格左边要 >= 上一格右边");
        }
    }

    #[test]
    fn 窗口尺寸等于最后一格右边加内边距() {
        let (w, h) = window_size();
        let last = item_rects()[3];
        let pad = scale(PAD);
        assert_eq!(w, last.right + pad);
        assert_eq!(h, last.bottom + pad);
    }

    #[test]
    fn 命中测试覆盖每一格() {
        let rects = item_rects();
        for (i, r) in rects.iter().enumerate() {
            let cx = r.left + 1;
            let cy = r.top + 1;
            assert_eq!(hit_test(cx, cy), i as i32, "第 {i} 格左上角内侧");
        }
        assert_eq!(hit_test(-5, -5), -1, "负坐标不该命中");
        let last = rects[3];
        assert_eq!(hit_test(last.right + 5, last.top + 1), -1, "越过最后一格不该命中");
    }

    #[test]
    fn 浮条能创建显示并绘制() {
        use windows_sys::Win32::Foundation::HWND;
        use windows_sys::Win32::Graphics::Gdi::UpdateWindow;
        use windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible;

        show();
        let raw = HWND_CELL.load(std::sync::atomic::Ordering::SeqCst);
        assert_ne!(raw, 0, "show() 之后应该有窗口句柄");
        let hwnd = raw as HWND;

        // 0..=3 每个格子的 hover 状态都画一遍：悬停会走「给该格画高亮底」的分支
        // （第 0 格那条会用**空字符串**调 draw_item），这条分支在宿主里 AV 崩过，必须覆盖。
        //
        // 注意：`UpdateWindow` 只在窗口**有无效区域**时才同步发 WM_PAINT。上一次
        // `BeginPaint` 已经把区域验证掉了，所以不先 `invalidate` 这里的循环是空转——
        // 当初就是这么漏掉这个崩溃的（测试全绿，宿主一悬停就死）。
        for h in -1..=3 {
            HOVER.store(h, std::sync::atomic::Ordering::Relaxed);
            invalidate(hwnd);
            unsafe { UpdateWindow(hwnd) };
        }
        HOVER.store(-1, std::sync::atomic::Ordering::Relaxed);

        assert_ne!(unsafe { IsWindowVisible(hwnd) }, 0, "show() 之后窗口应该可见");
        hide();
        assert_eq!(unsafe { IsWindowVisible(hwnd) }, 0, "hide() 之后窗口应该不可见");
    }
}
