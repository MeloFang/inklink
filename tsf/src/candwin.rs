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
//! - 只做显示 + 鼠标点击选词；上屏动作通过回调交给 `lib.rs`（编辑会话必须在那边发）

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
    CW_USEDEFAULT, MA_NOACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOWNOACTIVATE,
    WM_ERASEBKGND, WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_PAINT, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
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

/// 鼠标点了候选之后要干什么：`(上下文指针, 候选在整页里的下标)`。
///
/// 上屏必须走 TSF 编辑会话，那套东西在 `lib.rs` 里，所以窗口过程只负责
/// "算出点的是第几个"然后回调出去，不自己动文档。
pub type ClickHandler = unsafe extern "system" fn(ctx: usize, index: usize);

static CLICK_FN: AtomicUsize = AtomicUsize::new(0);
static CLICK_CTX: AtomicUsize = AtomicUsize::new(0);

/// 装/换点击回调（`Activate` 时装、`Deactivate` 时拆）。
pub fn set_click_handler(f: ClickHandler, ctx: usize) {
    CLICK_CTX.store(ctx, Ordering::SeqCst);
    CLICK_FN.store(f as usize, Ordering::SeqCst);
}

pub fn clear_click_handler() {
    CLICK_FN.store(0, Ordering::SeqCst);
    CLICK_CTX.store(0, Ordering::SeqCst);
}

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

/// 显示候选窗。`caret` 是光标的三段坐标 `(x, 顶边 y, 底边 y)`（屏幕坐标）。
///
/// 位置在这里算，不交给调用方，因为"放不下就往上翻"要同时知道窗口尺寸和屏幕边界。
pub fn show(
    pinyin: &str,
    candidates: &[String],
    sel: usize,
    page: usize,
    total_pages: usize,
    caret: (i32, i32, i32),
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
        let (x, y) = place(caret, w, h);
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

/// 光标到候选窗的间距（搜狗、微软拼音都在 4~8 像素之间）
const GAP: i32 = 6;
/// 候选窗距离屏幕边缘的最小留白
const EDGE: i32 = 4;

/// 算出候选窗左上角：默认在光标下方，**下面放不下就翻到光标上方**。
///
/// 这一条是用户拿搜狗对比出来的：光标靠近任务栏时，候选窗如果硬贴着往下画，
/// 一半会在屏幕外，等于不能选字。翻上去的基准是光标**顶边**，不是底边——
/// 用底边算会把光标所在那一行盖住。
fn place(caret: (i32, i32, i32), w: i32, h: i32) -> (i32, i32) {
    let (caret_x, _, caret_bottom) = caret;
    let area = work_area(caret_x, caret_bottom);
    place_in(area, caret, w, h)
}

/// `place` 的纯计算部分（可用区由调用方给，便于单测）。
fn place_in(area: (i32, i32, i32, i32), caret: (i32, i32, i32), w: i32, h: i32) -> (i32, i32) {
    let (left, top, right, bottom) = area;
    let (caret_x, caret_top, caret_bottom) = caret;

    // 竖直：先试下方，放不下改上方，上方也放不下就贴着可用区顶部
    // （贴顶之后窗口底边 = top + h，只要 h ≤ 可用区高度就一定在区域内）
    let mut y = caret_bottom + GAP;
    if y + h > bottom {
        y = caret_top - GAP - h;
    }
    if y < top {
        y = top;
    }

    // 水平：以光标为基准，右边放不下就左移
    let mut x = caret_x + EDGE;
    if x + w > right {
        x = right - w - EDGE;
    }
    x = x.max(left);

    (x, y)
}

/// 该点所在显示器的可用区域（已经扣掉任务栏）。取不到就退到整个主屏。
fn work_area(x: i32, y: i32) -> (i32, i32, i32, i32) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    unsafe {
        let pt = POINT { x, y };
        let monitor = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        if !monitor.is_null() {
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut info) != 0 {
                let r = info.rcWork;
                return (r.left, r.top, r.right, r.bottom);
            }
        }
        // 兜底：主屏物理尺寸（不管任务栏，总比画到屏幕外强）
        let (sw, sh) = screen_size();
        (0, 0, sw, sh)
    }
}

/// 主屏尺寸（拿不到就给个保守值）。
fn screen_size() -> (i32, i32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    unsafe {
        let w = GetSystemMetrics(SM_CXSCREEN);
        let h = GetSystemMetrics(SM_CYSCREEN);
        if w > 0 && h > 0 {
            (w, h)
        } else {
            (1280, 720)
        }
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

/// 窗口内坐标 → 候选下标（点在拼音行或空白处返回 None）。
///
/// 和 `WM_PAINT` 里的算式必须保持一致：整格都算可点（含序号那一小块），
/// 因为用户是"照着看见的那一格点"，不是照着文字的像素点。
fn hit_index(x: i32, y: i32, candidates: &[String]) -> Option<usize> {
    let grid_top = PAD + PINYIN_H;
    if y < grid_top {
        return None;
    }
    let cell_w = cell_width(candidates);
    let col = (x - PAD) / cell_w;
    let row = (y - grid_top) / ROW_H;
    if col < 0 || col >= COLS as i32 || row < 0 {
        return None;
    }
    let idx = row as usize * COLS + col as usize;
    if idx < candidates.len() {
        Some(idx)
    } else {
        None
    }
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
        // 鼠标点候选：算出点的是第几个，收起窗口，把上屏交给 lib.rs。
        //
        // 以前这里是 `WM_NCHITTEST -> HTTRANSPARENT`（点击穿透到下面的应用），
        // 结果是"点候选"等于"在文档里点了一下"：组合串被打断，拼音原样留在文档里。
        WM_LBUTTONDOWN => {
            let x = (lparam as i16) as i32;
            let y = ((lparam >> 16) as i16) as i32;
            let cands = CANDIDATES.lock().unwrap_or_else(|p| p.into_inner()).clone();
            if let Some(idx) = hit_index(x, y, &cands) {
                let stored = CLICK_FN.load(Ordering::SeqCst);
                let ctx = CLICK_CTX.load(Ordering::SeqCst);
                hide();
                if stored != 0 {
                    // SAFETY: 指针只在 `Activate`..`Deactivate` 之间有效，
                    // 而候选窗只在这段时间里可见，能点到就说明它还在。
                    let handler: ClickHandler = std::mem::transmute::<usize, ClickHandler>(stored);
                    handler(ctx, idx);
                }
            }
            0
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: (i32, i32, i32, i32) = (0, 0, 1920, 1040);

    /// 鼠标点在格子里要落到正确的候选上
    #[test]
    fn 点哪一格就选哪一个() {
        let cands: Vec<String> = (0..7).map(|i| format!("词{i}")).collect();
        // 拼音行不算
        assert_eq!(hit_index(10, 2, &cands), None);
        // 第一行第一个（点在序号那一小块也算）
        assert_eq!(hit_index(PAD + 3, PAD + PINYIN_H + 5, &cands), Some(0));
        // 第一行第三个
        let cw = cell_width(&cands);
        assert_eq!(hit_index(PAD + cw * 2 + 3, PAD + PINYIN_H + 5, &cands), Some(2));
        // 第二行第一个 = 下标 5
        assert_eq!(
            hit_index(PAD + 3, PAD + PINYIN_H + ROW_H + 5, &cands),
            Some(5)
        );
        // 第二行第三个是空的（只有 7 个）
        assert_eq!(
            hit_index(PAD + cw * 2 + 3, PAD + PINYIN_H + ROW_H + 5, &cands),
            None
        );
    }

    /// 下面有地方就贴在光标下面
    #[test]
    fn 光标下方有地方就往下放() {
        let (x, y) = place_in(AREA, (400, 300, 320), 500, 180);
        assert_eq!(x, 404);
        assert_eq!(y, 320 + GAP);
    }

    /// 贴着任务栏时翻到光标上方，且不盖住光标所在那一行
    #[test]
    fn 下面放不下就翻到上方() {
        let caret = (400, 960, 980);
        let (_, y) = place_in(AREA, caret, 500, 180);
        assert_eq!(y, caret.1 - GAP - 180);
        assert!(y + 180 <= caret.1);
        assert!(y >= 0);
    }

    /// 右边放不下就往左挪，别出屏幕
    #[test]
    fn 右边放不下就往左挪() {
        let (x, _) = place_in(AREA, (1880, 300, 320), 500, 180);
        assert_eq!(x, 1920 - 500 - EDGE);
        assert!(x + 500 <= 1920);
    }

    /// 上下都放不下就贴顶，整个窗口仍要留在可用区里
    #[test]
    fn 上下都放不下就贴顶() {
        let area = (0, 0, 1920, 200);
        let (_, y) = place_in(area, (400, 120, 140), 500, 180);
        assert_eq!(y, 0);
        assert!(y + 180 <= 200);
    }

    /// 窗口比可用区还高：贴顶，不能是负数（剩下的部分只能被裁掉）
    #[test]
    fn 窗口比可用区还高时贴顶() {
        let area = (0, 0, 1920, 200);
        let (_, y) = place_in(area, (400, 10, 30), 500, 1200);
        assert_eq!(y, 0);
    }
}
