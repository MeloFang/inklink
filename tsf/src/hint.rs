//! 联想条：上屏一个词之后，把「它后面常接什么」摆在光标下面。
//!
//! 和候选窗是**两套窗口**：候选窗挂在拼音组合串上（有拼音行、数字选字），
//! 联想条挂在**已经上屏的词**上 —— 那时没有组合串了，只有"接着写什么"。
//!
//! 所以这里不碰 TSF：只负责画、接点击、自己消失。点中的词由 lib.rs 插进文档，
//! 插完再问一次引擎，就形成了「健全 → 考核 → 激励体系」这样的链。
//!
//! 位置规则和候选窗一致：光标下方，放不下翻到上方，左右贴边收进工作区。

use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DT_LEFT, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, GetStockObject, GetTextExtentPoint32W, HDC,
    PAINTSTRUCT, PS_SOLID, ReleaseDC, RoundRect, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetForegroundWindow, HWND_TOPMOST, IsWindowVisible,
    KillTimer, MA_NOACTIVATE, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetTimer,
    SetWindowPos, ShowWindow, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_PAINT, WM_TIMER,
    WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const CLASS_NAME: &str = "inklink-hint";

/// 点中第几个联想词。和候选窗一样：函数指针 + 上下文。
pub type ClickHandler = unsafe extern "system" fn(ctx: usize, index: usize);

static CLICK_FN: AtomicUsize = AtomicUsize::new(0);
static CLICK_CTX: AtomicUsize = AtomicUsize::new(0);
static HWND_CELL: AtomicIsize = AtomicIsize::new(0);
static REGISTERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static TIPS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// 这一串联想是"打完哪个词"之后给的（点选时用它配对学习）。
static SOURCE: Mutex<Option<String>> = Mutex::new(None);
static HOVER: AtomicIsize = AtomicIsize::new(-1);

const PAD: i32 = 6;
const GAP: i32 = 4;
const ITEM_H: i32 = 28;
const RADIUS: i32 = 6;
/// 不点它就自己消失（毫秒）。太短来不及看，太长会一直挡着文档。
const LIFETIME_MS: u32 = 4000;
const TIMER_ID: usize = 0x11;

fn scale(v: i32) -> i32 {
    // 和候选窗同一套 DPI 规则：96 DPI 是基准
    let k = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForSystem() } as f32 / 96.0;
    (v as f32 * k).round() as i32
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 每个联想词的宽度（文字 + 内边距），和 `paint` 用同一份算法。
fn item_widths(hdc: HDC) -> Vec<i32> {
    let tips = TIPS.lock().unwrap_or_else(|p| p.into_inner()).clone();
    if tips.is_empty() {
        return Vec::new();
    }
    let font = make_font(scale(15));
    let old = unsafe { SelectObject(hdc, font) };
    let mut widths = Vec::with_capacity(tips.len());
    for t in &tips {
        let w = wide(t);
        let mut sz = SIZE { cx: 0, cy: 0 };
        unsafe { GetTextExtentPoint32W(hdc, w.as_ptr(), w.len() as i32 - 1, &mut sz) };
        widths.push(sz.cx + scale(20));
    }
    unsafe {
        SelectObject(hdc, old);
        DeleteObject(font);
    }
    widths
}

fn window_size(hdc: HDC) -> (i32, i32) {
    let widths = item_widths(hdc);
    if widths.is_empty() {
        return (0, 0);
    }
    let w: i32 = widths.iter().sum::<i32>() + GAP * (widths.len() as i32 - 1) + PAD * 2;
    (w, ITEM_H + PAD * 2)
}

fn make_font(px: i32) -> windows_sys::Win32::Graphics::Gdi::HFONT {
    let face = wide("Microsoft YaHei UI");
    unsafe {
        CreateFontW(
            -px,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            1, // DEFAULT_CHARSET
            0,
            0,
            0,
            0,
            face.as_ptr(),
        )
    }
}

/// 第 `index` 个联想词在窗口里的左边位置。
fn item_x(widths: &[i32], index: usize) -> i32 {
    PAD + widths.iter().take(index).sum::<i32>() + GAP * index as i32
}

/// 窗口内坐标 → 联想词下标。
fn hit_index(x: i32, y: i32) -> Option<usize> {
    let total = ITEM_H + PAD * 2;
    if y < PAD || y > total - PAD {
        return None;
    }
    let hdc = unsafe { GetDC(std::ptr::null_mut()) };
    if hdc.is_null() {
        return None;
    }
    let widths = item_widths(hdc);
    unsafe { ReleaseDC(std::ptr::null_mut(), hdc) };
    let mut left = PAD;
    for (i, w) in widths.iter().enumerate() {
        if x >= left && x < left + w {
            return Some(i);
        }
        left += w + GAP;
    }
    None
}

/// 联想条现在的词（测试与 lib.rs 用）。
pub fn source() -> Option<String> {
    SOURCE.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

pub fn current() -> Vec<String> {
    TIPS.lock().unwrap_or_else(|p| p.into_inner()).clone()
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
        let hdc = GetDC(std::ptr::null_mut());
        let (w, h) = window_size(hdc);
        ReleaseDC(std::ptr::null_mut(), hdc);
        if w <= 0 || h <= 0 {
            return;
        }
        let (x, y) = place(caret, w, h);
        SetWindowPos(hwnd as HWND, HWND_TOPMOST, x, y, w, h, SWP_NOACTIVATE);
        ShowWindow(hwnd as HWND, SW_SHOWNOACTIVATE);
        windows_sys::Win32::Graphics::Gdi::InvalidateRect(hwnd as HWND, std::ptr::null(), 0);
        KillTimer(hwnd as HWND, TIMER_ID);
        SetTimer(hwnd as HWND, TIMER_ID, LIFETIME_MS, None);
    }
    crate::log_line(&format!("hint: show {} tips", tips.len()));
}

/// 放不下就翻到光标上方；左右收进工作区。和候选窗一致的一句话版。
fn place(caret: (i32, i32, i32), w: i32, h: i32) -> (i32, i32) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    };
    let (cx, top, bottom) = caret;
    let mut left = 0;
    let mut right = 1920;
    let mut area_top = 0;
    let mut area_bottom = 1080;
    unsafe {
        let mon = MonitorFromPoint(
            POINT {
                x: cx,
                y: bottom,
            },
            MONITOR_DEFAULTTONEAREST,
        );
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
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
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

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let mut rc: RECT = unsafe { std::mem::zeroed() };
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc) };

    let dark = crate::config::snapshot().dark;
    let (bg, border, text, hover) = if dark {
        (0x0022_1E1E_u32, 0x0042_3A3A_u32, 0x00F1_EC_EC_u32, 0x0032_2A2A_u32)
    } else {
        (0x00FF_FFFF, 0x00E0_E0E0, 0x0022_2222, 0x00F2_EEFF)
    };

    unsafe {
        let brush = CreateSolidBrush(bg);
        FillRect(hdc, &rc, brush);
        DeleteObject(brush as *mut core::ffi::c_void);

        let pen = CreatePen(PS_SOLID, 1, border);
        let old_pen = SelectObject(hdc, pen);
        let null_brush = SelectObject(hdc, GetStockObject(5));
        RoundRect(hdc, 0, 0, rc.right, rc.bottom, RADIUS * 2, RADIUS * 2);
        SelectObject(hdc, null_brush);
        SelectObject(hdc, old_pen);
        DeleteObject(pen as *mut core::ffi::c_void);

        SetBkMode(hdc, TRANSPARENT as i32);
        let widths = item_widths(hdc);
        let font = make_font(scale(15));
        let old_font = SelectObject(hdc, font);
        let hov = HOVER.load(Ordering::SeqCst);
        let tips = TIPS.lock().unwrap_or_else(|p| p.into_inner()).clone();
        for (i, t) in tips.iter().enumerate() {
            let x = item_x(&widths, i);
            if hov == i as isize {
                let b = CreateSolidBrush(hover);
                let r = RECT {
                    left: x,
                    top: PAD,
                    right: x + widths[i],
                    bottom: PAD + ITEM_H,
                };
                FillRect(hdc, &r, b);
                DeleteObject(b as *mut core::ffi::c_void);
            }
            let mut tr = RECT {
                left: x,
                top: PAD,
                right: x + widths[i],
                bottom: PAD + ITEM_H,
            };
            SetTextColor(hdc, text);
            let w = wide(t);
            DrawTextW(
                hdc,
                w.as_ptr(),
                w.len() as i32 - 1,
                &mut tr,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER | windows_sys::Win32::Graphics::Gdi::DT_CENTER,
            );
        }
        SelectObject(hdc, old_font);
        DeleteObject(font);
    }
    unsafe { EndPaint(hwnd, &ps) };
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_PAINT => {
            unsafe { paint(hwnd) };
            0
        }
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

/// 鼠标移开时把悬停去掉（由 lib.rs 的定时器轮询调用）。
pub fn set_hover(index: isize) {
    if HOVER.swap(index, Ordering::SeqCst) != index {
        let hwnd = HWND_CELL.load(Ordering::SeqCst);
        if hwnd != 0 && unsafe { IsWindowVisible(hwnd as HWND) } != 0 {
            unsafe {
                windows_sys::Win32::Graphics::Gdi::InvalidateRect(
                    hwnd as HWND,
                    std::ptr::null(),
                    0,
                )
            };
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
