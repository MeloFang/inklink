//! 候选窗：光标下方的无边框小窗口，拼音 + 网格候选。
//!
//! 布局（对照主流输入法）：
//! ```text
//! ┌──────────────────────────────┐
//! │ xiangmu                      │  ← 拼音行
//! │ 1.项目   2.橡木   3.羡慕      │
//! │ 4.项目管理 5.项目经理 …        │  ← 5 列网格
//! └──────────────────────────────┘
//! ```
//!
//! 约束：
//! - `WS_EX_NOACTIVATE`：不抢焦点，否则组合串会断、焦点会从应用跑掉
//! - 由 TSF 线程的消息循环驱动，**不另起线程**
//! - 只做显示，不做交互（数字键选择在 lib.rs 里处理）

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC,
    GetStockObject, GetTextExtentPoint32W, InvalidateRect, ReleaseDC, SelectObject, SetBkMode,
    SetTextColor, DEFAULT_GUI_FONT, DT_LEFT, DT_SINGLELINE, DT_VCENTER, PAINTSTRUCT, TRANSPARENT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, RegisterClassW, SetWindowPos, ShowWindow,
    CW_USEDEFAULT, HTTRANSPARENT, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE,
    SW_SHOWNOACTIVATE, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_PAINT, WNDCLASSW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

static HWND_CELL: AtomicIsize = AtomicIsize::new(0);
static REGISTERED: AtomicBool = AtomicBool::new(false);
static CANDIDATES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static PINYIN: Mutex<String> = Mutex::new(String::new());
static SELECTED: AtomicUsize = AtomicUsize::new(0);
static PAGE: AtomicUsize = AtomicUsize::new(1);
static TOTAL_PAGES: AtomicUsize = AtomicUsize::new(1);

const CLASS_NAME: &str = "inklink-candidate";

/// 网格列数
const COLS: usize = 5;
/// 内边距
const PAD: i32 = 12;
/// 拼音行高
const PINYIN_H: i32 = 26;
/// 候选行高
const ROW_H: i32 = 30;
/// 单元格最小宽度
const MIN_CELL_W: i32 = 90;

const BG_COLOR: u32 = 0x0022_1F1E; // #1e1f22 (COLORREF 是 BGR)
const PINYIN_COLOR: u32 = 0x00F6_823B; // #3b82f6
const TEXT_COLOR: u32 = 0x00F4_EFEC; // #eceff4
const NUM_COLOR: u32 = 0x0091_8880; // #808891
const SEL_BG: u32 = 0x00F6_823B; // 选中项背景

/// 显示候选窗。`x, y` 是屏幕坐标（左上角）。
pub fn show(
    pinyin: &str,
    candidates: &[String],
    sel: usize,
    page: usize,
    total_pages: usize,
    x: i32,
    y: i32,
) {
    unsafe {
        ensure_window();
        let hwnd = HWND_CELL.load(Ordering::SeqCst);
        if hwnd == 0 {
            return;
        }
        *CANDIDATES.lock().unwrap() = candidates.to_vec();
        *PINYIN.lock().unwrap() = pinyin.to_string();
        SELECTED.store(sel, Ordering::SeqCst);
        PAGE.store(page, Ordering::SeqCst);
        TOTAL_PAGES.store(total_pages, Ordering::SeqCst);

        let (w, h) = window_size(candidates);
        SetWindowPos(
            hwnd as HWND,
            std::ptr::null_mut(),
            x,
            y,
            w,
            h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        ShowWindow(hwnd as HWND, SW_SHOWNOACTIVATE);
        InvalidateRect(hwnd as HWND, std::ptr::null(), 1);
    }
}

/// 隐藏候选窗。
pub fn hide() {
    unsafe {
        let hwnd = HWND_CELL.load(Ordering::SeqCst);
        if hwnd != 0 {
            ShowWindow(hwnd as HWND, SW_HIDE);
        }
    }
}

/// 计算窗口尺寸：列宽取最长候选，行数按候选数。
fn window_size(candidates: &[String]) -> (i32, i32) {
    let rows = candidates.len().div_ceil(COLS).max(1) as i32;
    let cell_w = cell_width(candidates);
    let w = cell_w * COLS as i32 + PAD * 2;
    let h = PINYIN_H + rows * ROW_H + PAD * 2;
    (w.max(220), h)
}

fn cell_width(candidates: &[String]) -> i32 {
    unsafe {
        let hdc = GetDC(std::ptr::null_mut());
        if hdc.is_null() {
            return MIN_CELL_W;
        }
        let font = GetStockObject(DEFAULT_GUI_FONT as i32);
        let old = SelectObject(hdc, font);
        let mut max_w = MIN_CELL_W;
        for c in candidates {
            let text = format!("1.{c}");
            let wide: Vec<u16> = text.encode_utf16().collect();
            let mut size = SIZE { cx: 0, cy: 0 };
            GetTextExtentPoint32W(hdc, wide.as_ptr(), wide.len() as i32, &mut size);
            max_w = max_w.max(size.cx + 18);
        }
        SelectObject(hdc, old);
        ReleaseDC(std::ptr::null_mut(), hdc);
        max_w
    }
}

/// 注册窗口类（幂等）。
fn ensure_window() {
    unsafe {
        if !REGISTERED.swap(true, Ordering::SeqCst) {
            let name: Vec<u16> = CLASS_NAME.encode_utf16().chain(std::iter::once(0)).collect();
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: std::ptr::null_mut(),
                lpszClassName: name.as_ptr(),
                ..std::mem::zeroed()
            };
            let _ = RegisterClassW(&wc);
        }

        if HWND_CELL.load(Ordering::SeqCst) == 0 {
            let name: Vec<u16> = CLASS_NAME.encode_utf16().chain(std::iter::once(0)).collect();
            let hwnd = CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                name.as_ptr(),
                name.as_ptr(),
                WS_POPUP,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                300,
                120,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            HWND_CELL.store(hwnd as isize, Ordering::SeqCst);
        }
    }
}

/// 在指定位置画一段文字。
unsafe fn draw_text(
    hdc: windows_sys::Win32::Graphics::Gdi::HDC,
    x: i32,
    y: i32,
    text: &str,
    color: u32,
) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut rc = RECT {
        left: x,
        top: y,
        right: x + 600,
        bottom: y + ROW_H,
    };
    SetTextColor(hdc, color);
    DrawTextW(
        hdc,
        wide.as_ptr(),
        wide.len() as i32,
        &mut rc,
        DT_LEFT | DT_SINGLELINE | DT_VCENTER,
    );
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);

            let bg = CreateSolidBrush(BG_COLOR);
            FillRect(hdc, &rc, bg);
            DeleteObject(bg as *mut core::ffi::c_void);

            let font = GetStockObject(DEFAULT_GUI_FONT as i32);
            let old = SelectObject(hdc, font);
            SetBkMode(hdc, TRANSPARENT as i32);

            // 拼音行
            let pinyin = PINYIN.lock().unwrap().clone();
            draw_text(hdc, PAD, PAD, &pinyin, PINYIN_COLOR);

            // 候选网格
            let cands = CANDIDATES.lock().unwrap().clone();
            let cell_w = cell_width(&cands);
            let sel = SELECTED.load(Ordering::SeqCst);
            for (i, c) in cands.iter().enumerate() {
                let col = (i % COLS) as i32;
                let row = (i / COLS) as i32;
                let x = PAD + col * cell_w;
                let y = PAD + PINYIN_H + row * ROW_H;
                // 选中项高亮
                if i == sel {
                    let mut sel_rect = RECT {
                        left: x,
                        top: y + 2,
                        right: x + cell_w - 4,
                        bottom: y + ROW_H - 2,
                    };
                    let brush = CreateSolidBrush(SEL_BG);
                    FillRect(hdc, &mut sel_rect, brush);
                    DeleteObject(brush as *mut core::ffi::c_void);
                }
                let num_color = if i == sel { 0x00FF_FFFF } else { NUM_COLOR };
                let txt_color = if i == sel { 0x00FF_FFFF } else { TEXT_COLOR };
                draw_text(hdc, x + 6, y, &format!("{}.", i + 1), num_color);
                draw_text(hdc, x + 22, y, c, txt_color);
            }

            // 翻页指示
            let page = PAGE.load(Ordering::SeqCst);
            let total = TOTAL_PAGES.load(Ordering::SeqCst);
            if total > 1 {
                let indicator = format!("{page}/{total}");
                draw_text(hdc, rc.right - 56, rc.bottom - ROW_H - 2, &indicator, NUM_COLOR);
            }

            SelectObject(hdc, old);
            EndPaint(hwnd, &ps);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
