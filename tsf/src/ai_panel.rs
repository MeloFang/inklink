//! AI 浮动面板（1.1.42 起；1.1.43 改成常驻）。
//!
//! 浮条上的「AI」按钮打开它：面板一直读当前选中的文字显示在里面，点一个动作让模型处理，
//! 结果也显示在面板里，最后 确定 / 取消 / 重新生成 / 复制结果。
//!
//! ## 1.1.43 改了什么（用户反馈）
//!
//! - **常驻**：只有右上角 × 能关。以前点文档任何地方、按 Esc 面板就关了 —— 而用户恰恰
//!   要先开面板、再去文档里选字，一点文档面板就没了，"选中的进不去 AI 助手"。
//! - **实时读选区**：开着的时候每 400ms 读一次选区（`ai::poll_selection`），
//!   用户在文档里选什么就显示什么。
//! - **屏幕中下方**：放在当前显示器工作区的底部正中，不再跟着光标跑。
//! - **跟着宿主程序**：本程序不在前台（Alt+Tab 走了）或这条线程的输入法被切走时先收起，
//!   回来再出现（状态都还在）。和浮条一个规矩，不会在别的程序上面盖一块用不了的面板。
//! - 开着的时候浮条外圈有一圈转动的彩光（`bar::set_glow`）。
//!
//! ## 几条硬约束（都是踩过的）
//!
//! - **不抢焦点**：`WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST`。抢焦点就会
//!   让文档失去 TSF 上下文，读选区、写回都会静默失败。
//! - **非模态**：`show()` 建完就返回，点击走自己的窗口过程。
//! - **状态必须看得见**：没启用、没选中、生成中、失败原因、写没写进去，全部显示在面板里。
//! - **轮询不刷屏**：选区没变就不重画，否则 400ms 闪一次。

use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT,
    DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, DeleteObject, DrawTextW, EndPaint, FillRect,
    GetMonitorInfoW, GetStockObject, HGDIOBJ, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow, NULL_BRUSH, PAINTSTRUCT, Rectangle, SelectObject, SetBkMode, SetTextColor,
    TRANSPARENT,
};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetForegroundWindow,
    GetWindowThreadProcessId, IDC_ARROW, IsWindowVisible, KillTimer, LoadCursorW, MA_NOACTIVATE,
    PostMessageW, RegisterClassW, SPI_GETWORKAREA, SW_HIDE, SW_SHOWNOACTIVATE, SetTimer,
    ShowWindow, SystemParametersInfoW, WM_CLOSE, WM_DESTROY, WM_LBUTTONUP, WM_MOUSEACTIVATE,
    WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const CLASS_NAME: &str = "inklink-ai-panel";
const TIMER_POLL: usize = 1;
/// 定时器周期。每一拍检查"该不该显示"，每 `SELECTION_EVERY` 拍读一次选区。
const POLL_MS: u32 = 100;
/// 400ms 读一次选区：选完字半秒内面板就有，又不至于每拍都排编辑会话。
const SELECTION_EVERY: u32 = 4;
static TICKS: AtomicU32 = AtomicU32::new(0);

const WIDTH: i32 = 460;
const HEIGHT: i32 = 384;
const PAD: i32 = 16;
/// 面板离工作区底边的距离（任务栏上方留一点空）。
const BOTTOM_MARGIN: i32 = 24;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Disabled,
    Loading,
    NoSelection,
    /// 选区超过 `MAX_INPUT_CHARS`
    TooLong,
    Ready,
    Busy,
    Done,
    /// 点了确定，等编辑会话写回
    Applying,
    Error,
}

struct State {
    phase: Phase,
    source: String,
    result: String,
    /// 上一次用的动作（重新生成要用）。
    action: u8,
    /// 写回之后的一句话：`(写进去了吗, 说明)`。
    note: Option<(bool, String)>,
}

static STATE: Mutex<State> = Mutex::new(State {
    phase: Phase::Disabled,
    source: String::new(),
    result: String::new(),
    action: 0,
    note: None,
});
static HWND_CELL: AtomicIsize = AtomicIsize::new(0);

struct Palette {
    bg: u32,
    border: u32,
    text: u32,
    dim: u32,
    accent: u32,
    field: u32,
    danger: u32,
    ok: u32,
}

const LIGHT: Palette = Palette {
    bg: 0x00_FF_FF_FF,
    border: 0x00_D6_D6_D6,
    text: 0x00_1E_1E_1E,
    dim: 0x00_8A_8A_8A,
    accent: 0x00_F6_5C_8B, // #8b5cf6 紫，和浮条的彩光一个主色
    field: 0x00_F6_F6_F6,
    danger: 0x00_2B_2B_D6,
    ok: 0x004A_A316,
};

const DARK: Palette = Palette {
    bg: 0x00_26_22_22,
    border: 0x00_46_41_41,
    text: 0x00_F1_EC_EC,
    dim: 0x00_97_8E_8E,
    accent: 0x00_FA_8B_A7, // #a78bfa 亮一档的紫，暗底上更清楚
    field: 0x00_32_2D_2D,
    danger: 0x00_6A_6A_FF,
    ok: 0x00_80_DE_4A,
};

fn palette(dark: bool) -> &'static Palette {
    if dark { &DARK } else { &LIGHT }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn scale(v: i32) -> i32 {
    let dpi = unsafe { GetDpiForSystem() };
    let dpi = if dpi == 0 { 96 } else { dpi as i32 };
    (v * dpi + 48) / 96
}

fn make_font(px: i32, weight: i32) -> HGDIOBJ {
    let face = wide("Microsoft YaHei UI");
    unsafe {
        CreateFontW(
            -scale(px),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            1,
            0,
            0,
            4, // 灰度抗锯齿：小字号彩色字不能用 ClearType
            0,
            face.as_ptr(),
        ) as HGDIOBJ
    }
}

/// 面板放在哪：工作区（已扣任务栏）底部正中，离底边 `margin`。纯函数，好测。
///
/// 面板比工作区还宽/高时贴左/贴顶，不让它跑出屏幕。
fn bottom_center(work: &RECT, w: i32, h: i32, margin: i32) -> (i32, i32) {
    let x = work.left + ((work.right - work.left) - w) / 2;
    let y = work.bottom - h - margin;
    (x.max(work.left), y.max(work.top))
}

/// 面板里所有可点的矩形（客户区坐标）。纯函数，好测。
struct Layout {
    close: RECT,
    actions: [RECT; 3],
    confirm: RECT,
    cancel: RECT,
    regen: RECT,
    copy: RECT,
    settings: RECT,
    source_box: RECT,
    result_box: RECT,
}

fn layout(w: i32, h: i32) -> Layout {
    let pad = scale(PAD);
    let btn_h = scale(30);
    let gap = scale(10);

    let close = RECT {
        left: w - pad - scale(22),
        top: pad - scale(2),
        right: w - pad,
        bottom: pad + scale(20),
    };

    let top = pad + scale(26);
    let source_box = RECT {
        left: pad,
        top,
        right: w - pad,
        bottom: top + scale(66),
    };

    let ay = source_box.bottom + scale(12);
    let bw = (w - pad * 2 - gap * 2) / 3;
    let mut actions = [RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    }; 3];
    for (i, r) in actions.iter_mut().enumerate() {
        let left = pad + (bw + gap) * i as i32;
        *r = RECT {
            left,
            top: ay,
            right: left + bw,
            bottom: ay + btn_h,
        };
    }

    let settings = RECT {
        left: pad,
        top: ay,
        right: pad + scale(110),
        bottom: ay + btn_h,
    };

    let ry = ay + btn_h + scale(12);
    let bottom_row = h - pad - btn_h - scale(18);
    let result_box = RECT {
        left: pad,
        top: ry,
        right: w - pad,
        bottom: bottom_row - scale(12),
    };

    let confirm = RECT {
        left: w - pad - scale(76),
        top: bottom_row,
        right: w - pad,
        bottom: bottom_row + btn_h,
    };
    let cancel = RECT {
        left: confirm.left - gap - scale(76),
        top: bottom_row,
        right: confirm.left - gap,
        bottom: bottom_row + btn_h,
    };
    let regen = RECT {
        left: cancel.left - gap - scale(96),
        top: bottom_row,
        right: cancel.left - gap,
        bottom: bottom_row + btn_h,
    };
    let copy = RECT {
        left: pad,
        top: bottom_row,
        right: pad + scale(96),
        bottom: bottom_row + btn_h,
    };

    Layout {
        close,
        actions,
        confirm,
        cancel,
        regen,
        copy,
        settings,
        source_box,
        result_box,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    None,
    Close,
    Action(u8),
    Confirm,
    Cancel,
    Regenerate,
    Copy,
    Settings,
}

/// 哪些阶段能点翻译/改写/续写。
fn actions_enabled(phase: Phase) -> bool {
    matches!(phase, Phase::Ready | Phase::Done | Phase::Error)
}

fn hit(x: i32, y: i32, w: i32, h: i32, phase: Phase) -> Hit {
    let l = layout(w, h);
    let inside = |r: &RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
    if inside(&l.close) {
        return Hit::Close;
    }
    if phase == Phase::Disabled {
        return if inside(&l.settings) {
            Hit::Settings
        } else {
            Hit::None
        };
    }
    if actions_enabled(phase) {
        for (i, r) in l.actions.iter().enumerate() {
            if inside(r) {
                return Hit::Action(i as u8);
            }
        }
    }
    if matches!(phase, Phase::Done | Phase::Error) && inside(&l.cancel) {
        return Hit::Cancel;
    }
    if phase == Phase::Done && inside(&l.confirm) {
        return Hit::Confirm;
    }
    if phase == Phase::Done && inside(&l.copy) {
        return Hit::Copy;
    }
    if matches!(phase, Phase::Done | Phase::Error) && inside(&l.regen) {
        return Hit::Regenerate;
    }
    Hit::None
}

fn state_snapshot() -> (Phase, String, String, u8, Option<(bool, String)>) {
    let g = STATE.lock().unwrap_or_else(|p| p.into_inner());
    (
        g.phase,
        g.source.clone(),
        g.result.clone(),
        g.action,
        g.note.clone(),
    )
}

/// 改状态；`f` 返回 false 表示"没变"，就不重画（轮询每 400ms 一次，不能每次都闪）。
fn update(f: impl FnOnce(&mut State) -> bool) {
    let changed = {
        let mut g = STATE.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut g)
    };
    if changed {
        redraw();
    }
}

fn redraw() {
    let h = HWND_CELL.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            windows_sys::Win32::Graphics::Gdi::InvalidateRect(h as HWND, std::ptr::null(), 0);
        }
    }
}

/// 选区轮询的结果落到哪个阶段（纯函数，好测）。
///
/// 只有"等选区"的几个阶段才跟着选区变；生成中、出了结果、正在写回时，选区变了也不动 ——
/// 否则用户点确定之前手一抖点到别处，结果就被冲掉了。
fn follow_selection(phase: Phase) -> bool {
    matches!(
        phase,
        Phase::Loading | Phase::NoSelection | Phase::TooLong | Phase::Ready
    )
}

pub fn show_disabled() {
    update(|s| {
        s.phase = Phase::Disabled;
        s.source.clear();
        s.result.clear();
        s.note = None;
        true
    });
    ensure_window();
}

pub fn show_loading() {
    update(|s| {
        s.phase = Phase::Loading;
        s.source.clear();
        s.result.clear();
        s.note = None;
        true
    });
    ensure_window();
}

/// 面板开着、而且现在要跟着选区变（`ai::poll_selection` 据此决定读不读）。
pub fn wants_selection() -> bool {
    is_open() && follow_selection(state_snapshot().0)
}

pub fn set_no_selection() {
    update(|s| {
        if !follow_selection(s.phase) || (s.phase == Phase::NoSelection && s.source.is_empty()) {
            return false;
        }
        s.phase = Phase::NoSelection;
        s.source.clear();
        true
    });
}

pub fn set_too_long(chars: usize, max: usize) {
    update(|s| {
        if !follow_selection(s.phase) {
            return false;
        }
        let msg = format!("选中了 {chars} 字，超过 {max} 字的上限。选短一点再试。");
        if s.phase == Phase::TooLong && s.source == msg {
            return false;
        }
        s.phase = Phase::TooLong;
        s.source = msg;
        true
    });
}

pub fn set_source(text: &str) {
    update(|s| {
        if !follow_selection(s.phase) || (s.phase == Phase::Ready && s.source == text) {
            return false;
        }
        s.phase = Phase::Ready;
        s.source = text.to_string();
        s.result.clear();
        // 选了新的一段，上一次"已写入"的提示就过时了
        s.note = None;
        true
    });
}

pub fn set_busy(action: u8) {
    update(|s| {
        s.phase = Phase::Busy;
        s.action = action;
        s.result.clear();
        s.note = None;
        true
    });
}

pub fn set_result(text: &str) {
    update(|s| {
        s.phase = Phase::Done;
        s.result = text.to_string();
        s.note = None;
        true
    });
}

pub fn set_error(msg: &str) {
    update(|s| {
        s.phase = Phase::Error;
        s.result = msg.to_string();
        true
    });
}

pub fn set_applying() {
    update(|s| {
        s.phase = Phase::Applying;
        s.note = None;
        true
    });
}

/// 写回的结果。写进去了 → 回到"等选区"（文档里的选区已经没了）；
/// 没写进去 → 留在结果页，用户可以重选原文再点确定，或者复制结果。
pub fn set_applied(outcome: Result<&str, &str>) {
    update(|s| {
        match outcome {
            Ok(msg) => {
                s.phase = Phase::NoSelection;
                s.source.clear();
                s.result.clear();
                s.note = Some((true, msg.to_string()));
            }
            Err(msg) => {
                s.phase = Phase::Done;
                s.note = Some((false, msg.to_string()));
            }
        }
        true
    });
}

/// 「取消」：丢掉结果，回到"选中文字 → 选动作"。面板不关。
pub fn discard_result() {
    update(|s| {
        s.result.clear();
        s.note = None;
        s.phase = if s.source.is_empty() {
            Phase::NoSelection
        } else {
            Phase::Ready
        };
        true
    });
}

/// 面板里当前显示的那段原文（发请求要用）。
pub fn current_source() -> Option<String> {
    let (phase, source, ..) = state_snapshot();
    if actions_enabled(phase) && !source.is_empty() {
        Some(source)
    } else {
        None
    }
}

pub fn last_action() -> Option<u8> {
    let (phase, _, _, action, _) = state_snapshot();
    if matches!(phase, Phase::Done | Phase::Error) {
        Some(action)
    } else {
        None
    }
}

/// 给 ai::confirm 用的（动作、原文、结果）。
pub fn result_for_apply() -> Option<(u8, String, String)> {
    let (phase, source, result, action, _) = state_snapshot();
    if phase != Phase::Done || source.is_empty() || result.is_empty() {
        return None;
    }
    Some((action, source, result))
}

pub fn is_open() -> bool {
    HWND_CELL.load(Ordering::SeqCst) != 0
}

/// 关掉面板（只有 × 和浮条上再点一次「AI」会走到这里）。
///
/// 窗口只能由建它的线程销毁：别的线程调过来就投一个 `WM_CLOSE` 让它自己关。
pub fn hide() {
    let h = HWND_CELL.load(Ordering::SeqCst);
    if h == 0 {
        return;
    }
    let owner = unsafe { GetWindowThreadProcessId(h as HWND, std::ptr::null_mut()) };
    if owner == unsafe { GetCurrentThreadId() } {
        unsafe { DestroyWindow(h as HWND) };
    } else {
        unsafe { PostMessageW(h as HWND, WM_CLOSE, 0, 0) };
    }
}

/// 当前前台窗口所在显示器的工作区；取不到退回主显示器工作区。
fn work_area() -> RECT {
    let mut area: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        let mon = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTONEAREST);
        let mut mi: MONITORINFO = std::mem::zeroed();
        mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if !mon.is_null() && GetMonitorInfoW(mon, &mut mi) != 0 {
            return mi.rcWork;
        }
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut area as *mut RECT as *mut _, 0);
    }
    area
}

fn ensure_window() {
    if is_open() {
        redraw();
        return;
    }
    ensure_class();
    let (w, h) = (scale(WIDTH), scale(HEIGHT));
    let (x, y) = bottom_center(&work_area(), w, h, scale(BOTTOM_MARGIN));

    let name = wide(CLASS_NAME);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            name.as_ptr(),
            name.as_ptr(),
            WS_POPUP,
            x,
            y,
            w,
            h,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        crate::log_line("ai-panel: CreateWindowExW failed");
        return;
    }
    HWND_CELL.store(hwnd as isize, Ordering::SeqCst);
    TICKS.store(0, Ordering::Relaxed);
    unsafe {
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(hwnd, TIMER_POLL, POLL_MS, None);
    }
    crate::bar::set_glow(true);
    crate::log_line(&format!("ai-panel: shown at {x},{y} {w}x{h}"));
}

/// 面板该不该露出来：宿主在前台、而且这条线程的输入法还开着。
fn should_be_visible() -> bool {
    crate::bar::foreground_is_self() && crate::ai::has_service()
}

/// 定时器一拍：按宿主前后台收起/放出，隔几拍读一次选区。
fn on_tick(hwnd: HWND) {
    let want = should_be_visible();
    let visible = unsafe { IsWindowVisible(hwnd) } != 0;
    if want && !visible {
        unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
    } else if !want && visible {
        unsafe { ShowWindow(hwnd, SW_HIDE) };
    }
    if !want {
        return;
    }
    let n = TICKS.fetch_add(1, Ordering::Relaxed);
    if n.is_multiple_of(SELECTION_EVERY) {
        crate::ai::poll_selection();
    }
}

/// 把结果写进剪贴板（用户点「复制结果」时）。宿主不让输入法改字时的退路。
fn copy_to_clipboard(text: &str) -> bool {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
    };
    use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

    let buf = wide(text);
    let bytes = buf.len() * 2;
    let h = HWND_CELL.load(Ordering::SeqCst) as HWND;
    unsafe {
        if OpenClipboard(h) == 0 {
            return false;
        }
        EmptyClipboard();
        let mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
        let mut ok = false;
        if !mem.is_null() {
            let dst = GlobalLock(mem) as *mut u16;
            if !dst.is_null() {
                std::ptr::copy_nonoverlapping(buf.as_ptr(), dst, buf.len());
                GlobalUnlock(mem);
                // 成功后内存归剪贴板所有，不能再释放
                ok = !SetClipboardData(CF_UNICODETEXT as u32, mem).is_null();
            }
        }
        CloseClipboard();
        ok
    }
}

unsafe fn text_block(
    hdc: windows_sys::Win32::Graphics::Gdi::HDC,
    r: &RECT,
    text: &str,
    fg: u32,
    font: HGDIOBJ,
    wrap: bool,
) {
    if text.is_empty() {
        return;
    }
    let old = unsafe { SelectObject(hdc, font) };
    unsafe { SetTextColor(hdc, fg) };
    let buf = wide(text);
    let mut rr = *r;
    let flags = if wrap {
        DT_LEFT | DT_WORDBREAK | DT_END_ELLIPSIS
    } else {
        DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS
    };
    unsafe { DrawTextW(hdc, buf.as_ptr(), (buf.len() - 1) as i32, &mut rr, flags) };
    unsafe { SelectObject(hdc, old) };
}

unsafe fn button(
    hdc: windows_sys::Win32::Graphics::Gdi::HDC,
    r: &RECT,
    label: &str,
    primary: bool,
    enabled: bool,
    pal: &Palette,
    font: HGDIOBJ,
) {
    let (fill, border, fg) = if !enabled {
        (pal.field, pal.border, pal.dim)
    } else if primary {
        (pal.accent, pal.accent, 0x00_FF_FF_FF)
    } else {
        (pal.field, pal.border, pal.text)
    };
    let b = unsafe { CreateSolidBrush(fill) };
    unsafe { FillRect(hdc, r, b) };
    unsafe { DeleteObject(b as HGDIOBJ) };
    let p = unsafe { CreatePen(0, 1, border) };
    let oldp = unsafe { SelectObject(hdc, p as HGDIOBJ) };
    let nb = unsafe { GetStockObject(NULL_BRUSH) };
    let oldb = unsafe { SelectObject(hdc, nb) };
    unsafe { Rectangle(hdc, r.left, r.top, r.right, r.bottom) };
    unsafe { SelectObject(hdc, oldb) };
    unsafe { SelectObject(hdc, oldp) };
    unsafe { DeleteObject(p as HGDIOBJ) };

    let old = unsafe { SelectObject(hdc, font) };
    unsafe { SetTextColor(hdc, fg) };
    let buf = wide(label);
    let mut rr = *r;
    unsafe {
        DrawTextW(
            hdc,
            buf.as_ptr(),
            (buf.len() - 1) as i32,
            &mut rr,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        )
    };
    unsafe { SelectObject(hdc, old) };
}

/// 原文框里那句话。纯函数：哪个阶段说什么，测试里钉住。
fn source_line(phase: Phase, source: &str) -> &str {
    match phase {
        Phase::Disabled => "AI 未启用",
        Phase::Loading => "正在读取选中文字…",
        Phase::NoSelection => "在文档里选中一段文字，会自动出现在这里",
        _ if source.is_empty() => "（没有选中文字）",
        _ => source,
    }
}

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let mut client: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetClientRect(hwnd, &mut client) };
    let (w, h) = (client.right, client.bottom);
    let dark = crate::config::snapshot().dark;
    let pal = palette(dark);
    let (phase, source, result, _action, note) = state_snapshot();
    let l = layout(w, h);

    let bg = unsafe { CreateSolidBrush(pal.bg) };
    unsafe { FillRect(hdc, &client, bg) };
    unsafe { DeleteObject(bg as HGDIOBJ) };
    let p = unsafe { CreatePen(0, 1, pal.border) };
    let oldp = unsafe { SelectObject(hdc, p as HGDIOBJ) };
    let nb = unsafe { GetStockObject(NULL_BRUSH) };
    let oldb = unsafe { SelectObject(hdc, nb) };
    unsafe { Rectangle(hdc, 0, 0, w, h) };
    unsafe { SelectObject(hdc, oldb) };
    unsafe { SelectObject(hdc, oldp) };
    unsafe { DeleteObject(p as HGDIOBJ) };

    unsafe { SetBkMode(hdc, TRANSPARENT as i32) };

    let title = make_font(15, 700);
    unsafe {
        text_block(
            hdc,
            &RECT {
                left: scale(PAD),
                top: scale(PAD),
                right: w - scale(PAD) - scale(30),
                bottom: scale(PAD) + scale(24),
            },
            "AI 助手",
            pal.text,
            title,
            false,
        )
    };

    // 关闭 ×（唯一能关掉面板的地方）
    let close_font = make_font(15, 400);
    unsafe { text_block(hdc, &l.close, "×", pal.text, close_font, false) };

    // 原文框
    let field = unsafe { CreateSolidBrush(pal.field) };
    unsafe { FillRect(hdc, &l.source_box, field) };
    unsafe { DeleteObject(field as HGDIOBJ) };
    let body = make_font(13, 400);
    let small = make_font(11, 400);
    let inner = RECT {
        left: l.source_box.left + scale(10),
        top: l.source_box.top + scale(8),
        right: l.source_box.right - scale(10),
        bottom: l.source_box.bottom - scale(8),
    };
    let src_fg = match phase {
        Phase::NoSelection | Phase::Disabled | Phase::Loading => pal.dim,
        Phase::TooLong => pal.danger,
        _ => pal.text,
    };
    unsafe { text_block(hdc, &inner, source_line(phase, &source), src_fg, body, true) };

    // 动作 / 设置按钮
    if phase == Phase::Disabled {
        unsafe { button(hdc, &l.settings, "打开设置", true, true, pal, body) };
    } else {
        let labels = ["翻译", "改写", "续写"];
        let enabled = actions_enabled(phase);
        for (i, label) in labels.iter().enumerate() {
            unsafe { button(hdc, &l.actions[i], label, false, enabled, pal, body) };
        }
    }

    // 结果框
    let (result_text, fg) = match phase {
        Phase::Disabled => (
            "设置里勾选「启用 AI」并填写接口地址，然后回到这里试。".to_string(),
            pal.dim,
        ),
        Phase::Loading => ("…".to_string(), pal.dim),
        Phase::NoSelection | Phase::TooLong | Phase::Ready => match &note {
            Some((ok, msg)) => (msg.clone(), if *ok { pal.ok } else { pal.danger }),
            None if phase == Phase::Ready => ("选择上面一种处理方式。".to_string(), pal.dim),
            None => ("选中文字后，点翻译 / 改写 / 续写。".to_string(), pal.dim),
        },
        Phase::Busy => ("生成中…（大模型第一次加载要十几秒）".to_string(), pal.dim),
        Phase::Applying => ("正在写入文档…".to_string(), pal.dim),
        Phase::Done => (result.clone(), pal.text),
        Phase::Error => (format!("失败：{result}"), pal.danger),
    };
    let field2 = unsafe { CreateSolidBrush(pal.field) };
    unsafe { FillRect(hdc, &l.result_box, field2) };
    unsafe { DeleteObject(field2 as HGDIOBJ) };
    let inner2 = RECT {
        left: l.result_box.left + scale(10),
        top: l.result_box.top + scale(8),
        right: l.result_box.right - scale(10),
        bottom: l.result_box.bottom - scale(8),
    };
    unsafe { text_block(hdc, &inner2, &result_text, fg, body, true) };

    // 底部按钮
    let done = phase == Phase::Done;
    let has_result = matches!(phase, Phase::Done | Phase::Error);
    unsafe { button(hdc, &l.copy, "复制结果", false, done, pal, body) };
    unsafe { button(hdc, &l.regen, "重新生成", false, has_result, pal, body) };
    unsafe { button(hdc, &l.cancel, "取消", false, has_result, pal, body) };
    unsafe { button(hdc, &l.confirm, "确定", true, done, pal, body) };

    // 提示行：结果页上写回失败的原因优先
    let (hint, hint_fg) = match (&note, phase) {
        (Some((false, msg)), Phase::Done) => (msg.as_str(), pal.danger),
        _ => (
            "确定 = 用结果替换选中的文字；取消只丢掉结果。关面板点右上角 ×",
            pal.dim,
        ),
    };
    unsafe {
        text_block(
            hdc,
            &RECT {
                left: scale(PAD),
                top: h - scale(PAD) - scale(14),
                right: w - scale(PAD),
                bottom: h - scale(PAD),
            },
            hint,
            hint_fg,
            small,
            false,
        )
    };

    unsafe { DeleteObject(title) };
    unsafe { DeleteObject(close_font) };
    unsafe { DeleteObject(body) };
    unsafe { DeleteObject(small) };
    unsafe { EndPaint(hwnd, &ps) };
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            unsafe { paint(hwnd) };
            0
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_LBUTTONUP => {
            let x = (lp & 0xFFFF) as i16 as i32;
            let y = ((lp >> 16) & 0xFFFF) as i16 as i32;
            let mut client: RECT = unsafe { std::mem::zeroed() };
            unsafe { GetClientRect(hwnd, &mut client) };
            let (phase, ..) = state_snapshot();
            match hit(x, y, client.right, client.bottom, phase) {
                Hit::Close => crate::ai::close_panel(),
                Hit::Cancel => crate::ai::cancel_result(),
                Hit::Action(i) => crate::ai::send(i),
                Hit::Confirm => crate::ai::confirm(),
                Hit::Regenerate => crate::ai::regenerate(),
                Hit::Copy => {
                    let (_, _, result, _, _) = state_snapshot();
                    let ok = copy_to_clipboard(&result);
                    crate::log_line(&format!("ai-panel: copy result -> {ok}"));
                    update(|s| {
                        s.note = Some(if ok {
                            (true, "结果已复制，可以粘贴到任何地方。".to_string())
                        } else {
                            (
                                false,
                                "复制失败：剪贴板被别的程序占着，再点一次。".to_string(),
                            )
                        });
                        true
                    });
                }
                Hit::Settings => crate::ai::open_settings(),
                Hit::None => {}
            }
            0
        }
        WM_TIMER => {
            if wp == TIMER_POLL {
                on_tick(hwnd);
            }
            0
        }
        WM_DESTROY => {
            unsafe { KillTimer(hwnd, TIMER_POLL) };
            HWND_CELL.store(0, Ordering::SeqCst);
            crate::bar::set_glow(false);
            crate::log_line("ai-panel: closed");
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn ensure_class() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let name = wide(CLASS_NAME);
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: std::ptr::null_mut(),
            hIcon: std::ptr::null_mut(),
            hCursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: name.as_ptr(),
        };
        let _ = unsafe { RegisterClassW(&wc) };
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 三颗动作按钮等宽不重叠() {
        let l = layout(scale(WIDTH), scale(HEIGHT));
        assert!(l.actions[0].right < l.actions[1].left);
        assert!(l.actions[1].right < l.actions[2].left);
        assert_eq!(l.actions[0].top, l.actions[2].top);
    }

    #[test]
    fn 底部按钮顺序是复制_重新生成_取消_确定() {
        let l = layout(scale(WIDTH), scale(HEIGHT));
        assert!(l.copy.right <= l.regen.left);
        assert!(l.regen.right <= l.cancel.left);
        assert!(l.cancel.right <= l.confirm.left);
        assert!(l.confirm.right <= scale(WIDTH));
        assert!(l.result_box.bottom <= l.confirm.top, "结果框不能压住按钮");
    }

    #[test]
    fn 命中测试按状态区分() {
        let (w, h) = (scale(WIDTH), scale(HEIGHT));
        let l = layout(w, h);
        let c = |r: &RECT| ((r.left + r.right) / 2, (r.top + r.bottom) / 2);

        let (x, y) = c(&l.actions[0]);
        assert_eq!(hit(x, y, w, h, Phase::Ready), Hit::Action(0));
        assert_eq!(hit(x, y, w, h, Phase::Busy), Hit::None, "生成中动作不可点");
        assert_eq!(
            hit(x, y, w, h, Phase::Applying),
            Hit::None,
            "写回中动作不可点"
        );
        let (sx, sy) = c(&l.actions[1]);
        assert_eq!(
            hit(sx, sy, w, h, Phase::Disabled),
            Hit::None,
            "未启用时动作不可点"
        );

        let (x, y) = c(&l.confirm);
        assert_eq!(hit(x, y, w, h, Phase::Done), Hit::Confirm);
        assert_eq!(
            hit(x, y, w, h, Phase::Ready),
            Hit::None,
            "没结果时确定不可点"
        );

        let (x, y) = c(&l.copy);
        assert_eq!(hit(x, y, w, h, Phase::Done), Hit::Copy);
        assert_eq!(
            hit(x, y, w, h, Phase::Error),
            Hit::None,
            "失败时没东西可复制"
        );

        let (x, y) = c(&l.settings);
        assert_eq!(hit(x, y, w, h, Phase::Disabled), Hit::Settings);
    }

    #[test]
    fn 关闭按钮在任何状态都能点() {
        let (w, h) = (scale(WIDTH), scale(HEIGHT));
        let l = layout(w, h);
        let (x, y) = (
            (l.close.left + l.close.right) / 2,
            (l.close.top + l.close.bottom) / 2,
        );
        for phase in [
            Phase::Disabled,
            Phase::Loading,
            Phase::NoSelection,
            Phase::TooLong,
            Phase::Ready,
            Phase::Busy,
            Phase::Done,
            Phase::Applying,
            Phase::Error,
        ] {
            assert_eq!(hit(x, y, w, h, phase), Hit::Close, "{phase:?}");
        }
    }

    #[test]
    fn 取消只在有结果时可点而且不等于关闭() {
        let (w, h) = (scale(WIDTH), scale(HEIGHT));
        let l = layout(w, h);
        let (x, y) = (
            (l.cancel.left + l.cancel.right) / 2,
            (l.cancel.top + l.cancel.bottom) / 2,
        );
        assert_eq!(hit(x, y, w, h, Phase::Done), Hit::Cancel);
        assert_eq!(hit(x, y, w, h, Phase::Error), Hit::Cancel);
        assert_eq!(hit(x, y, w, h, Phase::Ready), Hit::None);
    }

    #[test]
    fn 面板放在工作区底部正中() {
        // 1920×1040 的工作区（任务栏 40px），面板 460×384，离底 24
        let work = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        assert_eq!(bottom_center(&work, 460, 384, 24), (730, 632));
        // 副屏在左边（负坐标）
        let left = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
        };
        assert_eq!(bottom_center(&left, 460, 384, 24), (-1190, 672));
        // 工作区比面板还小：贴左上，不跑出屏幕
        let tiny = RECT {
            left: 100,
            top: 50,
            right: 400,
            bottom: 300,
        };
        assert_eq!(bottom_center(&tiny, 460, 384, 24), (100, 50));
    }

    #[test]
    fn 只有等选区的阶段才跟着选区变() {
        for p in [
            Phase::Loading,
            Phase::NoSelection,
            Phase::TooLong,
            Phase::Ready,
        ] {
            assert!(follow_selection(p), "{p:?}");
        }
        // 生成中 / 出了结果 / 写回中：选区变了也不能把结果冲掉
        for p in [
            Phase::Busy,
            Phase::Done,
            Phase::Applying,
            Phase::Error,
            Phase::Disabled,
        ] {
            assert!(!follow_selection(p), "{p:?}");
        }
    }

    #[test]
    fn 没选中时原文框告诉用户去选() {
        assert!(source_line(Phase::NoSelection, "").contains("选中"));
        assert_eq!(source_line(Phase::Ready, "hello"), "hello");
        assert_eq!(source_line(Phase::Ready, ""), "（没有选中文字）");
    }
}
