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
    BeginPaint, CreatePen, CreateRoundRectRgn, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint,
    FillRect, GetDC, GetStockObject, GetTextExtentPoint32W, InvalidateRect, ReleaseDC, RoundRect,
    CreateFontW, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, DT_LEFT, DT_SINGLELINE,
    DT_VCENTER, PAINTSTRUCT, PS_SOLID, TRANSPARENT,
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
static FONT_CAND: AtomicIsize = AtomicIsize::new(0);
static FONT_PINYIN: AtomicIsize = AtomicIsize::new(0);

const CLASS_NAME: &str = "inklink-candidate";

/// 网格列数
const COLS: usize = 5;
/// 内边距
const PAD: i32 = 14;
/// 拼音行高
const PINYIN_H: i32 = 24;
/// 候选行高
const ROW_H: i32 = 34;
/// 单元格最小宽度
const MIN_CELL_W: i32 = 96;

// 浅色主题（对齐主流输入法）
const BG_COLOR: u32 = 0x00FF_FFFF; // #ffffff 白底
const PINYIN_COLOR: u32 = 0x0099_9999; // 灰
const TEXT_COLOR: u32 = 0x0022_2222; // #222222 深灰
const NUM_COLOR: u32 = 0x00AA_AAAA; // 浅灰序号
const SEL_COLOR: u32 = 0x0000_66FF; // #ff6600 橙色高亮（BGR）
const BORDER_COLOR: u32 = 0x00E0_E0E0; // 浅灰边框
const CORNER_RADIUS: i32 = 8; // 圆角半径

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
        // 圆角：用区域把窗口裁成圆角矩形
        let region = CreateRoundRectRgn(0, 0, w + 1, h + 1, CORNER_RADIUS, CORNER_RADIUS);
        SetWindowRgn(hwnd as HWND, region, 1);
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
        let font = get_font(&FONT_CAND, 17);
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
    height: i32,
) {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut rc = RECT {
        left: x,
        top: y,
        right: x + 600,
        bottom: y + height,
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

            // 圆角边框
            let pen = CreatePen(PS_SOLID, 1, BORDER_COLOR);
            let old_pen = SelectObject(hdc, pen);
            let null_brush = SelectObject(hdc, GetStockObject(5 /* NULL_BRUSH */));
            RoundRect(
                hdc,
                0,
                0,
                rc.right,
                rc.bottom,
                CORNER_RADIUS * 2,
                CORNER_RADIUS * 2,
            );
            SelectObject(hdc, null_brush);
            SelectObject(hdc, old_pen);
            DeleteObject(pen as *mut core::ffi::c_void);

            let cand_font = get_font(&FONT_CAND, 17);
            let pinyin_font = get_font(&FONT_PINYIN, 14);
            SetBkMode(hdc, TRANSPARENT as i32);

            // 拼音行（小号灰字）
            let old = SelectObject(hdc, pinyin_font);
            let pinyin = PINYIN.lock().unwrap().clone();
            draw_text(hdc, PAD, PAD + 2, &pinyin, PINYIN_COLOR, PINYIN_H);

            // 翻页指示（右上角，小灰字）
            let page = PAGE.load(Ordering::SeqCst);
            let total = TOTAL_PAGES.load(Ordering::SeqCst);
            if total > 1 {
                let indicator = format!("{page}/{total}");
                let iw = text_width(hdc, &indicator);
                draw_text(hdc, rc.right - PAD - iw, PAD + 2, &indicator, NUM_COLOR, PINYIN_H);
            }

            // 候选网格（大号深色字，选中橙色）
            SelectObject(hdc, cand_font);
            let cands = CANDIDATES.lock().unwrap().clone();
            let cell_w = cell_width(&cands);
            let sel = SELECTED.load(Ordering::SeqCst);
            // 只给「选中项所在的那一行」标数字 1~5。
            //
            // 展开成 5×5 后如果从 1 标到 25，数字会和上下键/左右键的行列语义
            // 冲突（用户看不出「2」到底是这行的第 2 个还是整页第 2 个）。
            // 现在一行一标，上下换行、左右/数字选行内。
            let sel_row = sel / COLS;
            for (i, c) in cands.iter().enumerate() {
                let col = (i % COLS) as i32;
                let row = (i / COLS) as i32;
                let x = PAD + col * cell_w;
                let y = PAD + PINYIN_H + row * ROW_H;
                if i / COLS == sel_row {
                    let color = if i == sel { SEL_COLOR } else { NUM_COLOR };
                    draw_text(hdc, x, y, &format!("{}.", col + 1), color, ROW_H);
                }
                // 非选中行不画数字，但文字仍从 x+22 起，保证上下行对齐
                let txt_color = if i == sel { SEL_COLOR } else { TEXT_COLOR };
                draw_text(hdc, x + 22, y, c, txt_color, ROW_H);
            }

            SelectObject(hdc, old);
            EndPaint(hwnd, &ps);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 测量一段文字的宽度。
unsafe fn text_width(hdc: windows_sys::Win32::Graphics::Gdi::HDC, text: &str) -> i32 {
    let wide: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE { cx: 0, cy: 0 };
    GetTextExtentPoint32W(hdc, wide.as_ptr(), wide.len() as i32, &mut size);
    size.cx
}

/// 懒创建字体并缓存。
unsafe fn get_font(
    slot: &AtomicIsize,
    size: i32,
) -> windows_sys::Win32::Graphics::Gdi::HFONT {
    let existing = slot.load(Ordering::SeqCst);
    if existing != 0 {
        return existing as _;
    }
    let face: Vec<u16> = "Microsoft YaHei UI"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let f = CreateFontW(
        -size,
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
        5, // CLEARTYPE_QUALITY
        0,
        face.as_ptr(),
    );
    slot.store(f as isize, Ordering::SeqCst);
    f
}
