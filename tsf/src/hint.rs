//! 联想条：上屏一个词之后，把「它后面常接什么」摆在光标下面。
//!
//! 和候选窗是**两套窗口**：候选窗挂在拼音组合串上（有拼音行、数字选字），
//! 联想条挂在**已经上屏的词**上 —— 那时没有组合串了，只有"接着写什么"。
//!
//! 外观和 AI 面板同一套做法：分层窗口 + `UpdateLayeredWindow`，圆角 + 渐变描边 + 外发光
//! （复用 `ai_panel::finish`），所以看起来和浮条/AI 面板是一个家族。
//!
//! 交互：每个词前面带 1/2/3，**按数字键直接选**（和候选窗一致），也可以鼠标点。

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, CreateFontW, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, GetDC, GetTextExtentPoint32W, ReleaseDC, SelectObject, SetBkMode, TRANSPARENT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetForegroundWindow, HWND_TOPMOST, IsWindowVisible, KillTimer,
    MA_NOACTIVATE, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetTimer,
    SetWindowPos, ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WM_ERASEBKGND, WM_LBUTTONDOWN,
    WM_MOUSEACTIVATE, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};

use crate::bar::{Canvas, coverage, round_rect_sdf};

const CLASS_NAME: &str = "inklink-hint";

/// 点中第几个联想词。和候选窗一样：函数指针 + 上下文。
pub type ClickHandler = unsafe extern "system" fn(ctx: usize, index: usize);

static CLICK_FN: AtomicUsize = AtomicUsize::new(0);
static CLICK_CTX: AtomicUsize = AtomicUsize::new(0);
static HWND_CELL: AtomicIsize = AtomicIsize::new(0);
static REGISTERED: AtomicBool = AtomicBool::new(false);
static TIPS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static SOURCE: Mutex<Option<String>> = Mutex::new(None);
static HOVER: AtomicIsize = AtomicIsize::new(-1);

const PAD: i32 = 6;
/// 本体外面留的发光/阴影边
const GLOW: i32 = 9;
const GAP: i32 = 4;
const ITEM_H: i32 = 28;
const RADIUS: i32 = 8;
/// 不点它就自己消失（毫秒）
const LIFETIME_MS: u32 = 4000;
const TIMER_ID: usize = 0x11;

fn scale(v: i32) -> i32 {
    let k = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForSystem() } as f32 / 96.0;
    (v as f32 * k).round() as i32
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn make_font(px: i32, weight: i32) -> windows_sys::Win32::Graphics::Gdi::HFONT {
    let face = wide("Microsoft YaHei UI");
    unsafe { CreateFontW(-px, 0, 0, 0, weight, 0, 0, 0, 1, 0, 0, 0, 0, face.as_ptr()) }
}

/// 每个联想词的文字（带序号）与宽度。
fn items() -> Vec<(String, i32)> {
    let tips = TIPS.lock().unwrap_or_else(|p| p.into_inner()).clone();
    if tips.is_empty() {
        return Vec::new();
    }
    let hdc = unsafe { GetDC(std::ptr::null_mut()) };
    if hdc.is_null() {
        return Vec::new();
    }
    let font = make_font(scale(15), 400);
    let old = unsafe { SelectObject(hdc, font) };
    let mut out = Vec::with_capacity(tips.len());
    for (i, t) in tips.iter().enumerate() {
        let label = format!("{}. {t}", i + 1);
        let w = wide(&label);
        let mut sz = SIZE { cx: 0, cy: 0 };
        unsafe { GetTextExtentPoint32W(hdc, w.as_ptr(), w.len() as i32 - 1, &mut sz) };
        out.push((label, sz.cx + scale(18)));
    }
    unsafe {
        SelectObject(hdc, old);
        DeleteObject(font);
        ReleaseDC(std::ptr::null_mut(), hdc);
    }
    out
}

/// 窗口尺寸（含发光边）。
fn outer_size() -> (i32, i32) {
    let it = items();
    if it.is_empty() {
        return (0, 0);
    }
    let w: i32 =
        it.iter().map(|(_, w)| *w).sum::<i32>() + GAP * (it.len() as i32 - 1) + PAD * 2 + GLOW * 2;
    (w, ITEM_H + PAD * 2 + GLOW * 2)
}

/// 第 `index` 个词的左边（窗口坐标，已含发光边）。
fn item_x(widths: &[i32], index: usize) -> i32 {
    GLOW + PAD + widths.iter().take(index).sum::<i32>() + GAP * index as i32
}

/// 窗口坐标 → 联想词下标。
fn hit_index(x: i32, y: i32) -> Option<usize> {
    let it = items();
    if it.is_empty() {
        return None;
    }
    let top = GLOW + PAD;
    if y < top || y > top + ITEM_H {
        return None;
    }
    let widths: Vec<i32> = it.iter().map(|(_, w)| *w).collect();
    let mut left = GLOW + PAD;
    for (i, w) in widths.iter().enumerate() {
        if x >= left && x < left + w {
            return Some(i);
        }
        left += w + GAP;
    }
    None
}

pub fn current() -> Vec<String> {
    TIPS.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

pub fn source() -> Option<String> {
    SOURCE.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

pub fn is_visible() -> bool {
    let hwnd = HWND_CELL.load(Ordering::SeqCst);
    hwnd != 0 && unsafe { IsWindowVisible(hwnd as HWND) } != 0
}

pub fn set_click_handler(f: ClickHandler, ctx: usize) {
    CLICK_CTX.store(ctx, Ordering::SeqCst);
    CLICK_FN.store(f as usize, Ordering::SeqCst);
}

pub fn clear_click_handler() {
    CLICK_FN.store(0, Ordering::SeqCst);
    CLICK_CTX.store(0, Ordering::SeqCst);
}

pub fn hide() {
    let hwnd = HWND_CELL.load(Ordering::SeqCst);
    if hwnd != 0 {
        unsafe {
            KillTimer(hwnd as HWND, TIMER_ID);
            ShowWindow(hwnd as HWND, SW_HIDE);
        }
    }
    // 词表必须跟着清掉：数字键的选择判断看的是"现在有没有联想词"，
    // 只隐藏窗口会让按键把上一次联想的词插进文档（2026-11-08 用户实测）。
    TIPS.lock().unwrap_or_else(|p| p.into_inner()).clear();
    *SOURCE.lock().unwrap_or_else(|p| p.into_inner()) = None;
    HOVER.store(-1, Ordering::SeqCst);
}

/// 光标下面弹一条联想。`caret` 是 `(x, 顶边, 底边)`，和候选窗同一套坐标。
pub fn show(tips: &[String], caret: (i32, i32, i32), from: &str) {
    if tips.is_empty() {
        hide();
        return;
    }
    *TIPS.lock().unwrap_or_else(|p| p.into_inner()) = tips.to_vec();
    *SOURCE.lock().unwrap_or_else(|p| p.into_inner()) = Some(from.to_string());
    HOVER.store(-1, Ordering::SeqCst);

    unsafe {
        ensure_window();
        let hwnd = HWND_CELL.load(Ordering::SeqCst);
        if hwnd == 0 {
            return;
        }
        let (w, h) = outer_size();
        if w <= 0 || h <= 0 {
            return;
        }
        let (x, y) = place(caret, w, h);
        SetWindowPos(hwnd as HWND, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
        if !present(hwnd as HWND) {
            return;
        }
        ShowWindow(hwnd as HWND, SW_SHOWNOACTIVATE);
        KillTimer(hwnd as HWND, TIMER_ID);
        SetTimer(hwnd as HWND, TIMER_ID, LIFETIME_MS, None);
    }
    crate::log_line(&format!("hint: show {} tips", tips.len()));
}

/// 分层窗口的绘制：圆角底、每个词一块药丸、渐变描边和外发光交给 `finish`。
fn present(hwnd: HWND) -> bool {
    let (w, h) = outer_size();
    let Some(mut c) = (unsafe { Canvas::new(w, h) }) else {
        return false;
    };
    let dark = crate::config::snapshot().dark;
    let it = items();
    if it.is_empty() {
        return false;
    }
    let widths: Vec<i32> = it.iter().map(|(_, ww)| *ww).collect();
    let k = scale(1) as f32;
    let hov = HOVER.load(Ordering::SeqCst);

    // 底色：整条一块圆角卡片
    {
        let px = unsafe { c.pixels() };
        let (x0, y0) = (GLOW as f32, GLOW as f32);
        let (x1, y1) = ((w - GLOW) as f32, (h - GLOW) as f32);
        let (bg, radius) = if dark {
            ((0x22u32, 0x1Eu32, 0x1Eu32), (RADIUS as f32) * k)
        } else {
            ((0xFFu32, 0xFFu32, 0xFFu32), (RADIUS as f32) * k)
        };
        let inner = (radius - 1.5 * k).max(1.0);
        for y in 0..h {
            for x in 0..w {
                let d = round_rect_sdf(x as f32 + 0.5, y as f32 + 0.5, x0, y0, x1, y1, radius);
                let a = coverage(d);
                if a <= 0.0 {
                    continue;
                }
                // 悬停的药丸
                let mut hot = 0.0f32;
                if hov >= 0 {
                    let i = hov as usize;
                    if i < widths.len() {
                        let lx = item_x(&widths, i) as f32;
                        let sub = round_rect_sdf(
                            x as f32 + 0.5,
                            y as f32 + 0.5,
                            lx,
                            (GLOW + PAD) as f32,
                            lx + widths[i] as f32,
                            (GLOW + PAD + ITEM_H) as f32,
                            (ITEM_H as f32 / 2.0) * k,
                        );
                        hot = coverage(sub) * 0.9;
                    }
                }
                let (hr, hg, hb) = if dark {
                    (0x3Au32, 0x32u32, 0x32u32)
                } else {
                    (0xF2u32, 0xEEu32, 0xFFu32)
                };
                let r = bg.0 as f32 * (1.0 - hot) + hr as f32 * hot;
                let g = bg.1 as f32 * (1.0 - hot) + hg as f32 * hot;
                let b = bg.2 as f32 * (1.0 - hot) + hb as f32 * hot;
                let ai = (a * 255.0 + 0.5) as u32;
                if let Some(slot) = px.get_mut((y * w + x) as usize) {
                    *slot = (ai << 24) | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
                }
            }
        }
    }

    // 文字（GDI 画在预乘 alpha 之前，`finish` 之后会按 alpha 处理）
    let hdc = c.dc;
    let font = make_font(scale(15), 400);
    unsafe {
        SetBkMode(hdc, TRANSPARENT as i32);
        let old = SelectObject(hdc, font);
        for (i, (label, _)) in it.iter().enumerate() {
            let x = item_x(&widths, i);
            let mut rc = RECT {
                left: x,
                top: GLOW + PAD,
                right: x + widths[i],
                bottom: GLOW + PAD + ITEM_H,
            };
            let t = wide(label);
            let fg = if i as isize == hov {
                if dark { 0x00F1_EC_EC } else { 0x0022_2222 }
            } else if dark {
                0x00C8_C2_C2
            } else {
                0x0044_4444
            };
            windows_sys::Win32::Graphics::Gdi::SetTextColor(hdc, fg);
            windows_sys::Win32::Graphics::Gdi::DrawTextW(
                hdc,
                t.as_ptr(),
                t.len() as i32 - 1,
                &mut rc,
                DT_CENTER | DT_SINGLELINE | DT_VCENTER,
            );
        }
        SelectObject(hdc, old);
        DeleteObject(font);
    }

    // 描边 + 外发光（和 AI 面板同一套：粉→紫→蓝渐变 + 柔和阴影）
    {
        let px = unsafe { c.pixels() };
        crate::ai_panel::finish(px, w, h, GLOW, (RADIUS as f32) * k, 2.0 * k, 0.55);
    }

    let size = SIZE { cx: w, cy: h };
    let src = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    unsafe {
        UpdateLayeredWindow(
            hwnd,
            std::ptr::null_mut(),
            std::ptr::null(),
            &size,
            c.dc,
            &src,
            0,
            &blend,
            ULW_ALPHA,
        ) != 0
    }
}

/// 放不下就翻到光标上方；左右收进工作区。
fn place(caret: (i32, i32, i32), w: i32, h: i32) -> (i32, i32) {
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    };
    let (cx, top, bottom) = caret;
    let mut left = 0;
    let mut right = 1920;
    let mut area_top = 0;
    let mut area_bottom = 1080;
    unsafe {
        let mon = MonitorFromPoint(POINT { x: cx, y: bottom }, MONITOR_DEFAULTTONEAREST);
        if !mon.is_null() {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(mon, &mut mi) != 0 {
                left = mi.rcWork.left;
                right = mi.rcWork.right;
                area_top = mi.rcWork.top;
                area_bottom = mi.rcWork.bottom;
            }
        }
    }
    let mut y = bottom + scale(4);
    if y + h > area_bottom {
        y = top - scale(4) - h;
    }
    if y < area_top {
        y = area_top;
    }
    let mut x = cx;
    if x + w > right {
        x = right - w;
    }
    x = x.max(left);
    (x, y)
}

fn ensure_window() {
    unsafe {
        if !REGISTERED.swap(true, Ordering::SeqCst) {
            let name = wide(CLASS_NAME);
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: std::ptr::null_mut(),
                lpszClassName: name.as_ptr(),
                ..std::mem::zeroed()
            };
            let _ = RegisterClassW(&wc);
        }
        if HWND_CELL.load(Ordering::SeqCst) == 0 {
            let name = wide(CLASS_NAME);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                name.as_ptr(),
                name.as_ptr(),
                WS_POPUP,
                0,
                0,
                200,
                60,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            HWND_CELL.store(hwnd as isize, Ordering::SeqCst);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_TIMER => {
            hide();
            0
        }
        WM_LBUTTONDOWN => {
            let x = (lparam as i16) as i32;
            let y = ((lparam >> 16) as i16) as i32;
            if let Some(idx) = hit_index(x, y) {
                let f = CLICK_FN.load(Ordering::SeqCst);
                let ctx = CLICK_CTX.load(Ordering::SeqCst);
                hide();
                if f != 0 {
                    let handler: ClickHandler = unsafe { std::mem::transmute(f) };
                    unsafe { handler(ctx, idx) };
                }
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 用户按了左右键：在联想词之间移动选中项。返回 true 表示这次按键被吃掉了。
///
/// 第一次按就"进到"联想条里（`FOCUSED`），之后空格才是"确认选中项"。
/// 鼠标移开时把悬停去掉（由 lib.rs 的定时器轮询调用）。
pub fn set_hover(index: isize) {
    if HOVER.swap(index, Ordering::SeqCst) != index {
        let hwnd = HWND_CELL.load(Ordering::SeqCst);
        if hwnd != 0 && unsafe { IsWindowVisible(hwnd as HWND) } != 0 {
            unsafe { present(hwnd as HWND) };
        }
    }
}

/// 前台窗口是不是自己所在的那个进程（宿主切走了就该收起来）。
pub fn foreground_is_self() -> bool {
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_null() {
        return false;
    }
    let mut pid = 0u32;
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(fg, &mut pid) };
    pid == unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessId() }
}
