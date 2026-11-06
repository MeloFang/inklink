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

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateRoundRectRgn, CreateSolidBrush, DT_LEFT,
    DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, GetStockObject,
    GetTextExtentPoint32W, InvalidateRect, PAINTSTRUCT, PS_SOLID, ReleaseDC, RoundRect,
    SelectObject, SetBkMode, SetTextColor, SetWindowRgn, TRANSPARENT,
};
use windows_sys::Win32::System::Threading::GetCurrentProcessId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, GetClientRect, GetForegroundWindow,
    GetWindowThreadProcessId, IsWindowVisible, MA_NOACTIVATE, RegisterClassW, SW_HIDE,
    HWND_TOPMOST, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetWindowPos, ShowWindow, WM_ERASEBKGND,
    WM_LBUTTONDOWN, WM_MOUSEACTIVATE, WM_PAINT, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP, WindowFromPoint,
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

/// 内边距（2026-11-06：14 -> 10，用户报"间距太大占屏幕"）
const PAD: i32 = 10;
/// 拼音行高
const PINYIN_H: i32 = 22;
/// 候选行高（34 -> 30，同上）
const ROW_H: i32 = 30;
/// 单元格最小宽度（96 -> 76：两个字一格的候选不再留出半格空白）
const MIN_CELL_W: i32 = 76;
/// 圆角半径
const CORNER_RADIUS: i32 = 8;

/// 候选窗现在的排版与主题。
///
/// 配置是**每次 `show()` 现读**的（见 `config.rs`），读到的结果存在这里，
/// 让同一次绘制里的 `window_size` / `hit_index` / `WM_PAINT` 用同一份 ——
/// 不这么锁，用户中途改配置就会"窗口尺寸按新列数、点击命中按旧列数"。
#[derive(Clone, Copy)]
struct Layout {
    cols: usize,
    dark: bool,
}

impl Layout {
    const DEFAULT: Layout = Layout {
        cols: 5,
        dark: false,
    };
}

static LAYOUT: Mutex<Layout> = Mutex::new(Layout::DEFAULT);

fn layout() -> Layout {
    *LAYOUT.lock().unwrap_or_else(|p| p.into_inner())
}

/// 一套配色。`COLORREF` 是 `0x00BBGGRR`，字面量写成 `0x00_BB_GG_RR` 方便核对。
struct Palette {
    bg: u32,
    pinyin: u32,
    text: u32,
    num: u32,
    sel: u32,
    border: u32,
}

/// 浅色（默认，对齐主流输入法）：白底、深灰字、橙色选中。
const PALETTE_LIGHT: Palette = Palette {
    bg: 0x00FF_FFFF,     // #ffffff
    pinyin: 0x0099_9999, // #999999
    text: 0x0022_2222,   // #222222
    num: 0x00AA_AAAA,    // #aaaaaa
    sel: 0x0000_66FF,    // #ff6600
    border: 0x00E0_E0E0, // #e0e0e0
};

/// 暗色：近黑底、亮灰字、同一族的亮橙选中（换成蓝色会破掉"选中=橙"的既有语感）。
const PALETTE_DARK: Palette = Palette {
    bg: 0x0022_1E1E,     // #1e1e22
    pinyin: 0x0097_8E8E, // #8e8e97
    text: 0x00F1_EC_EC,  // #ececf1
    num: 0x0078_6E6E,    // #6e6e78
    sel: 0x003D_9EFF,    // #ff9e3d
    border: 0x0042_3A3A, // #3a3a42
};

fn palette(dark: bool) -> &'static Palette {
    if dark { &PALETTE_DARK } else { &PALETTE_LIGHT }
}

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
        // 先锁住这次要用的排版/主题，再算尺寸 —— 否则同一帧里
        // `window_size` 与 `WM_PAINT` 可能读到两份不同的配置。
        let cfg = crate::config::snapshot();
        let layout = Layout {
            cols: cfg.cols,
            dark: cfg.dark,
        };
        *LAYOUT.lock().unwrap_or_else(|p| p.into_inner()) = layout;

        *CANDIDATES.lock().unwrap() = candidates.to_vec();
        *PINYIN.lock().unwrap() = pinyin.to_string();
        SELECTED.store(sel, Ordering::SeqCst);
        PAGE.store(page, Ordering::SeqCst);
        TOTAL_PAGES.store(total_pages, Ordering::SeqCst);

        let (w, h) = window_size(candidates, layout.cols);
        let (x, y) = place(caret, w, h);
        // HWND_TOPMOST 而不是 SWP_NOZORDER：窗口虽然是 TOPMOST 建的，但宿主自己的
        // 浮层（WPS 的 AI 面板、Office 的任务窗格）也是 TOPMOST 而且比我们后建，
        // 光"保持原样"会被它们压在下面 —— 用户打字时看不见候选（2026-11-06 实测）。
        // SWP_NOACTIVATE 保证抢层级不抢焦点，组合串不会断。
        SetWindowPos(
            hwnd as HWND,
            HWND_TOPMOST,
            x,
            y,
            w,
            h,
            SWP_NOACTIVATE,
        );
        // 圆角：用区域把窗口裁成圆角矩形
        let region = CreateRoundRectRgn(0, 0, w + 1, h + 1, CORNER_RADIUS, CORNER_RADIUS);
        SetWindowRgn(hwnd as HWND, region, 1);
        ShowWindow(hwnd as HWND, SW_SHOWNOACTIVATE);
        InvalidateRect(hwnd as HWND, std::ptr::null(), 1);
        // 排障用：位置/尺寸/可见性都记一行，用户报"候选窗不出来"时能直接看出是
        // 没算对位置、还是 ShowWindow 没生效。
        crate::log_line(&format!(
            "cand show {}x{} at {},{} caret=({},{},{}) n={} visible={}",
            w,
            h,
            x,
            y,
            caret.0,
            caret.1,
            caret.2,
            candidates.len(),
            IsWindowVisible(hwnd as HWND)
        ));
        // 排障第二行：窗口说自己可见、用户却看不见时，看这两条就够定位——
        // `under` 是不是自己（被别的窗口盖住会不同）、前台窗口属于哪个进程
        // （宿主不在前台时，弹窗再 topmost 也可能被全屏/独占窗口挡掉）。
        let mut fg_pid = 0u32;
        let fg = GetForegroundWindow();
        GetWindowThreadProcessId(fg, &mut fg_pid);
        let under = WindowFromPoint(POINT {
            x: x + w / 2,
            y: y + h / 2,
        });
        crate::log_line(&format!(
            "cand probe under={} self={} fg={} fgpid={} me={}",
            under as isize,
            hwnd as isize,
            fg as isize,
            fg_pid,
            GetCurrentProcessId()
        ));
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
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
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
        if w > 0 && h > 0 { (w, h) } else { (1280, 720) }
    }
}

/// 隐藏候选窗。
pub fn hide() {
    unsafe {
        let hwnd = HWND_CELL.load(Ordering::SeqCst);
        if hwnd != 0 {
            // 只在真的可见时才记一行，否则每次按键都刷一条
            if IsWindowVisible(hwnd as HWND) != 0 {
                crate::log_line("cand hide");
            }
            ShowWindow(hwnd as HWND, SW_HIDE);
        }
    }
}

/// 计算窗口尺寸：列宽取最长候选，行数按候选数。
fn window_size(candidates: &[String], cols: usize) -> (i32, i32) {
    let cols = cols.max(1);
    let rows = candidates.len().div_ceil(cols).max(1) as i32;
    // 实际用到的列数：一页只剩 2 个候选时，窗口不该还按 5 个格子撑开
    let used_cols = candidates.len().clamp(1, cols);
    let widths = col_widths(candidates, cols);
    let w = widths.iter().take(used_cols).sum::<i32>() + PAD * 2;
    let h = PINYIN_H + rows * ROW_H + PAD * 2;
    (w.max(180), h)
}

/// 窗口内坐标 → 候选下标（点在拼音行或空白处返回 None）。
///
/// 和 `WM_PAINT` 里的算式必须保持一致：整格都算可点（含序号那一小块），
/// 因为用户是"照着看见的那一格点"，不是照着文字的像素点。
fn hit_index(x: i32, y: i32, candidates: &[String], cols: usize) -> Option<usize> {
    let grid_top = PAD + PINYIN_H;
    if y < grid_top {
        return None;
    }
    let cols = cols.max(1);
    // 逐列宽度：从左往右量过去，落在哪一列就是哪一列
    let widths = col_widths(candidates, cols);
    let mut col = None;
    let mut left = PAD;
    for (i, w) in widths.iter().enumerate() {
        if x >= left && x < left + w {
            col = Some(i);
            break;
        }
        left += w;
    }
    let col = col?;
    let row = (y - grid_top) / ROW_H;
    if row < 0 {
        return None;
    }
    let idx = row as usize * cols + col;
    if idx < candidates.len() {
        Some(idx)
    } else {
        None
    }
}

fn col_widths(candidates: &[String], cols: usize) -> Vec<i32> {
    let cols = cols.max(1);
    let mut w = vec![MIN_CELL_W; cols];
    unsafe {
        let hdc = GetDC(std::ptr::null_mut());
        if hdc.is_null() {
            return w;
        }
        let font = get_font(&FONT_CAND, 17);
        let old = SelectObject(hdc, font);
        for (i, c) in candidates.iter().enumerate() {
            let col = i % cols;
            // 序号只画在选中行，但宽度按最长的那个算，免得选另一行时文字被挤
            let text = format!("{}.{c}", col + 1);
            let wide: Vec<u16> = text.encode_utf16().collect();
            let mut size = SIZE { cx: 0, cy: 0 };
            GetTextExtentPoint32W(hdc, wide.as_ptr(), wide.len() as i32, &mut size);
            w[col] = w[col].max(size.cx + 14);
        }
        SelectObject(hdc, old);
        ReleaseDC(std::ptr::null_mut(), hdc);
    }
    w
}

/// 第 `col` 列的左边界（窗口坐标，已含 PAD）。
fn col_x(widths: &[i32], col: usize) -> i32 {
    PAD + widths.iter().take(col).sum::<i32>()
}

/// 注册窗口类（幂等）。
fn ensure_window() {
    unsafe {
        if !REGISTERED.swap(true, Ordering::SeqCst) {
            let name: Vec<u16> = CLASS_NAME
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: std::ptr::null_mut(),
                lpszClassName: name.as_ptr(),
                ..std::mem::zeroed()
            };
            let _ = RegisterClassW(&wc);
        }

        if HWND_CELL.load(Ordering::SeqCst) == 0 {
            let name: Vec<u16> = CLASS_NAME
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
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

/// 把候选窗画到一块 DC 上（客户区 `client`）。
///
/// 从 `WM_PAINT` 里抽出来是为了能单测：测试给它一块内存位图，就能渲出
/// 浅色/暗色两版预览，不用部署到宿主里看。所有颜色/列数都从 `LAYOUT` 取，
/// 所以调用方必须先让 `show()` 或测试把 `LAYOUT` 设好。
unsafe fn paint_frame(hdc: windows_sys::Win32::Graphics::Gdi::HDC, client: &RECT) {
    let rc = *client;
    let layout = layout();
    let pal = palette(layout.dark);
    let cols = layout.cols.max(1);

    let bg = CreateSolidBrush(pal.bg);
    FillRect(hdc, &rc, bg);
    DeleteObject(bg as *mut core::ffi::c_void);

    // 圆角边框
    let pen = CreatePen(PS_SOLID, 1, pal.border);
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
    let pinyin = PINYIN.lock().unwrap_or_else(|p| p.into_inner()).clone();
    draw_text(hdc, PAD, PAD + 2, &pinyin, pal.pinyin, PINYIN_H);

    // 翻页指示（右上角，小灰字）
    let page = PAGE.load(Ordering::SeqCst);
    let total = TOTAL_PAGES.load(Ordering::SeqCst);
    if total > 1 {
        let indicator = format!("{page}/{total}");
        let iw = text_width(hdc, &indicator);
        draw_text(
            hdc,
            rc.right - PAD - iw,
            PAD + 2,
            &indicator,
            pal.num,
            PINYIN_H,
        );
    }

    // 候选网格（大号字，选中项用强调色）
    SelectObject(hdc, cand_font);
    let cands = CANDIDATES.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let widths = col_widths(&cands, cols);
    let sel = SELECTED.load(Ordering::SeqCst);
    // 只给「选中项所在的那一行」标数字 1~N。
    //
    // 展开成多行后如果从 1 标到 25，数字会和上下键/左右键的行列语义
    // 冲突（用户看不出「2」到底是这行的第 2 个还是整页第 2 个）。
    // 现在一行一标，上下换行、左右/数字选行内。
    let sel_row = sel / cols;
    for (i, c) in cands.iter().enumerate() {
        let col = i % cols;
        let row = (i / cols) as i32;
        let x = col_x(&widths, col);
        let y = PAD + PINYIN_H + row * ROW_H;
        if i / cols == sel_row {
            let color = if i == sel { pal.sel } else { pal.num };
            draw_text(hdc, x, y, &format!("{}.", col + 1), color, ROW_H);
        }
        // 非选中行不画数字，但文字仍从 x+22 起，保证上下行对齐
        let txt_color = if i == sel { pal.sel } else { pal.text };
        draw_text(hdc, x + 22, y, c, txt_color, ROW_H);
    }

    SelectObject(hdc, old);
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
            if let Some(idx) = hit_index(x, y, &cands, layout().cols) {
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
            paint_frame(hdc, &rc);
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
unsafe fn get_font(slot: &AtomicIsize, size: i32) -> windows_sys::Win32::Graphics::Gdi::HFONT {
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
    // 只有预览图那两条测试用到：渲一帧到内存位图。放在这里，cdylib 构建才不会报 unused。
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
        DeleteDC, HGDIOBJ,
    };

    const AREA: (i32, i32, i32, i32) = (0, 0, 1920, 1040);

    /// 渲染一帧候选窗，写 32 位 BMP（自上而下，BGRA）。
    ///
    /// 存在意义：暗色主题不用部署到宿主里就能看。用 `paint_frame` 画，和真实
    /// 窗口走同一条代码路径，所以预览里能看出对齐、配色、序号位置。
    fn write_frame(
        name: &str,
        cands: &[String],
        pinyin: &str,
        sel: usize,
        dark: bool,
        cols: usize,
    ) {
        unsafe {
            let (w, h) = window_size(cands, cols);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..std::mem::zeroed()
                },
                bmiColors: [std::mem::zeroed()],
            };
            let dc = CreateCompatibleDC(std::ptr::null_mut());
            assert!(!dc.is_null(), "CreateCompatibleDC 失败");
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let dib =
                CreateDIBSection(dc, &bmi, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0);
            assert!(!dib.is_null() && !bits.is_null(), "CreateDIBSection 失败");
            let old = SelectObject(dc, dib as HGDIOBJ);

            *CANDIDATES.lock().unwrap_or_else(|p| p.into_inner()) = cands.to_vec();
            *PINYIN.lock().unwrap_or_else(|p| p.into_inner()) = pinyin.to_string();
            SELECTED.store(sel, Ordering::SeqCst);
            PAGE.store(1, Ordering::SeqCst);
            TOTAL_PAGES.store(2, Ordering::SeqCst);
            *LAYOUT.lock().unwrap_or_else(|p| p.into_inner()) = Layout { cols, dark };

            let rc = RECT {
                left: 0,
                top: 0,
                right: w,
                bottom: h,
            };
            paint_frame(dc, &rc);

            let px = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
            let image = px.to_vec();
            SelectObject(dc, old);
            DeleteObject(dib as HGDIOBJ);
            DeleteDC(dc);

            let mut out: Vec<u8> = Vec::with_capacity(54 + image.len());
            out.extend_from_slice(b"BM");
            out.extend_from_slice(&((54 + image.len()) as u32).to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&54u32.to_le_bytes());
            out.extend_from_slice(&40u32.to_le_bytes());
            out.extend_from_slice(&w.to_le_bytes());
            out.extend_from_slice(&(-h).to_le_bytes());
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&32u16.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&(image.len() as u32).to_le_bytes());
            out.extend_from_slice(&2835i32.to_le_bytes());
            out.extend_from_slice(&2835i32.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&image);

            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(name);
            std::fs::write(&path, &out).expect("预览图写入失败");
        }
    }

    /// 浅色 / 暗色各渲一帧，落在 `tsf/target/cand-preview-light.bmp` / `-dark.bmp`。
    #[test]
    fn 渲染两套主题的预览图() {
        let cands: Vec<String> = vec![
            "项目".into(),
            "橡木".into(),
            "羡慕".into(),
            "项目管理".into(),
            "项目经理".into(),
            "香木".into(),
            "向慕".into(),
        ];
        for (dark, name) in [
            (false, "cand-preview-light.bmp"),
            (true, "cand-preview-dark.bmp"),
        ] {
            write_frame(name, &cands, "xiang'mu", 0, dark, 5);
        }
    }

    /// 两种主题的对比度都要够：正文和背景的亮度差至少 100（0~255）。
    #[test]
    fn 两套主题的字都看得清() {
        fn luma(c: u32) -> i32 {
            // COLORREF 0x00BBGGRR
            let b = (c & 0xFF) as i32;
            let g = ((c >> 8) & 0xFF) as i32;
            let r = ((c >> 16) & 0xFF) as i32;
            (r * 299 + g * 587 + b * 114) / 1000
        }
        for dark in [false, true] {
            let p = palette(dark);
            assert!(
                (luma(p.text) - luma(p.bg)).abs() >= 100,
                "dark={dark} 正文与背景对比度不够"
            );
            assert!(
                (luma(p.sel) - luma(p.bg)).abs() >= 60,
                "dark={dark} 选中色与背景分不开"
            );
            assert!(
                (luma(p.pinyin) - luma(p.bg)).abs() >= 40,
                "dark={dark} 拼音行太淡"
            );
        }
        // 浅色底亮、暗色底暗，别写反了
        assert!(luma(palette(false).bg) > 200);
        assert!(luma(palette(true).bg) < 60);
    }

    /// 鼠标点在格子里要落到正确的候选上
    #[test]
    fn 点哪一格就选哪一个() {
        let cands: Vec<String> = (0..7).map(|i| format!("词{i}")).collect();
        let cols = 5;
        // 拼音行不算
        assert_eq!(hit_index(10, 2, &cands, cols), None);
        // 第一行第一个（点在序号那一小块也算）
        assert_eq!(
            hit_index(PAD + 3, PAD + PINYIN_H + 5, &cands, cols),
            Some(0)
        );
        // 第一行第三个
        let cw = col_widths(&cands, cols)[0];
        assert_eq!(
            hit_index(PAD + cw * 2 + 3, PAD + PINYIN_H + 5, &cands, cols),
            Some(2)
        );
        // 第二行第一个 = 下标 5
        assert_eq!(
            hit_index(PAD + 3, PAD + PINYIN_H + ROW_H + 5, &cands, cols),
            Some(5)
        );
        // 第二行第三个是空的（只有 7 个）
        assert_eq!(
            hit_index(PAD + cw * 2 + 3, PAD + PINYIN_H + ROW_H + 5, &cands, cols),
            None
        );
    }

    /// 列数变了，命中测试要跟着变（P3：每页候选数可配置）
    #[test]
    fn 列数变化后命中测试跟着走() {
        let cands: Vec<String> = (0..7).map(|i| format!("词{i}")).collect();
        let cw = col_widths(&cands, 3)[0];
        // 3 列时，第 4 个候选在第 2 行第 1 格
        assert_eq!(
            hit_index(PAD + 3, PAD + PINYIN_H + ROW_H + 5, &cands, 3),
            Some(3)
        );
        // 3 列时第 2 行第 3 格是下标 5
        assert_eq!(
            hit_index(PAD + cw * 2 + 3, PAD + PINYIN_H + ROW_H + 5, &cands, 3),
            Some(5)
        );
        // 越界的列不算命中
        assert_eq!(
            hit_index(PAD + cw * 3 + 3, PAD + PINYIN_H + 5, &cands, 3),
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
