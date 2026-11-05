//! AI 浮动面板（1.1.42）。
//!
//! 用户把浮条上三个 AI 格改成一个「AI」按钮：点开后弹出这个面板，里面才列
//! 翻译 / 改写 / 续写。面板会先读出当前选中的文字显示在里面，点一个动作让模型处理，
//! 结果也显示在面板里，最后 确定 / 取消 / 重新生成。
//!
//! ## 几条硬约束（都是踩过的）
//!
//! - **不抢焦点**：`WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST`。抢焦点就会
//!   让文档失去 TSF 上下文，读选区、写回都会静默失败。
//! - **非模态**：`show()` 建完就返回，点击走自己的窗口过程。阻塞在浮条的消息处理里
//!   会卡住宿主的 UI。
//! - **状态必须看得见**：没启用 AI、没选中文字、正在生成、失败原因，全部显示在面板里，
//!   不能只写日志（用户反馈"用不了、没提示"就是这里）。
//! - 不抢焦点就收不到键盘，Esc / 点外面用 50ms 定时器轮询。

use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DT_CENTER, DT_LEFT, DT_SINGLELINE,
    DT_VCENTER, DT_WORDBREAK, DeleteObject, DrawTextW, EndPaint, FillRect, GetStockObject, HGDIOBJ,
    NULL_BRUSH, PAINTSTRUCT, Rectangle, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE, VK_LBUTTON};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos, GetWindowRect,
    IDC_ARROW, KillTimer, LoadCursorW, MA_NOACTIVATE, RegisterClassW, SPI_GETWORKAREA,
    SW_SHOWNOACTIVATE, SetTimer, ShowWindow, SystemParametersInfoW, WM_DESTROY, WM_KEYDOWN,
    WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const CLASS_NAME: &str = "inklink-ai-panel";
const TIMER_POLL: usize = 1;
const POLL_MS: u32 = 50;

const WIDTH: i32 = 460;
const HEIGHT: i32 = 384;
const PAD: i32 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Disabled,
    Loading,
    NoSelection,
    Ready,
    Busy,
    Done,
    Error,
}

struct State {
    phase: Phase,
    source: String,
    result: String,
    /// 上一次用的动作（重新生成要用）。
    action: u8,
}

static STATE: Mutex<State> = Mutex::new(State {
    phase: Phase::Disabled,
    source: String::new(),
    result: String::new(),
    action: 0,
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
}

const LIGHT: Palette = Palette {
    bg: 0x00_FF_FF_FF,
    border: 0x00_D6_D6_D6,
    text: 0x00_1E_1E_1E,
    dim: 0x00_8A_8A_8A,
    accent: 0x00_00_66_FF,
    field: 0x00_F6_F6_F6,
    danger: 0x00_2B_2B_D6,
};

const DARK: Palette = Palette {
    bg: 0x00_26_22_22,
    border: 0x00_46_41_41,
    text: 0x00_F1_EC_EC,
    dim: 0x00_97_8E_8E,
    accent: 0x00_3D_9E_FF,
    field: 0x00_32_2D_2D,
    danger: 0x00_6A_6A_FF,
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

/// 面板里所有可点的矩形（客户区坐标）。纯函数，好测。
struct Layout {
    close: RECT,
    actions: [RECT; 3],
    confirm: RECT,
    cancel: RECT,
    regen: RECT,
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
    let bottom_row = h - pad - btn_h;
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

    Layout {
        close,
        actions,
        confirm,
        cancel,
        regen,
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
    Settings,
}

fn hit(x: i32, y: i32, w: i32, h: i32, phase: Phase) -> Hit {
    let l = layout(w, h);
    let inside = |r: &RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
    if inside(&l.close) {
        return Hit::Close;
    }
    if inside(&l.cancel) {
        return Hit::Cancel;
    }
    match phase {
        Phase::Disabled => {
            if inside(&l.settings) {
                return Hit::Settings;
            }
        }
        Phase::Ready | Phase::Done | Phase::Error => {
            for (i, r) in l.actions.iter().enumerate() {
                if inside(r) {
                    return Hit::Action(i as u8);
                }
            }
        }
        _ => {}
    }
    if phase == Phase::Done && inside(&l.confirm) {
        return Hit::Confirm;
    }
    if matches!(phase, Phase::Done | Phase::Error) && inside(&l.regen) {
        return Hit::Regenerate;
    }
    Hit::None
}

fn state_snapshot() -> (Phase, String, String, u8) {
    let g = STATE.lock().unwrap_or_else(|p| p.into_inner());
    (g.phase, g.source.clone(), g.result.clone(), g.action)
}

fn set_state(f: impl FnOnce(&mut State)) {
    if let Ok(mut g) = STATE.lock() {
        f(&mut g);
    }
    redraw();
}

fn redraw() {
    let h = HWND_CELL.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            windows_sys::Win32::Graphics::Gdi::InvalidateRect(h as HWND, std::ptr::null(), 0);
        }
    }
}

pub fn show_disabled() {
    set_state(|s| {
        s.phase = Phase::Disabled;
        s.source.clear();
        s.result.clear();
    });
    ensure_window();
}

pub fn show_loading() {
    set_state(|s| {
        s.phase = Phase::Loading;
        s.source.clear();
        s.result.clear();
    });
    ensure_window();
}

pub fn set_no_selection() {
    set_state(|s| {
        s.phase = Phase::NoSelection;
        s.source.clear();
    });
}

pub fn set_source(text: &str) {
    set_state(|s| {
        s.phase = Phase::Ready;
        s.source = text.to_string();
        s.result.clear();
    });
}

pub fn set_busy(action: u8) {
    set_state(|s| {
        s.phase = Phase::Busy;
        s.action = action;
        s.result.clear();
    });
}

pub fn set_result(text: &str) {
    set_state(|s| {
        s.phase = Phase::Done;
        s.result = text.to_string();
    });
}

pub fn set_error(msg: &str) {
    set_state(|s| {
        s.phase = Phase::Error;
        s.result = msg.to_string();
    });
}

/// 面板里当前显示的那段原文（发请求要用）。
pub fn current_source() -> Option<String> {
    let (phase, source, _, _) = state_snapshot();
    if matches!(phase, Phase::Ready | Phase::Done | Phase::Error) && !source.is_empty() {
        Some(source)
    } else {
        None
    }
}

pub fn last_action() -> Option<u8> {
    let (phase, _, _, action) = state_snapshot();
    if matches!(phase, Phase::Done | Phase::Error) {
        Some(action)
    } else {
        None
    }
}

/// 给 ai::confirm 用的（源文、结果、动作）。
pub fn result_for_apply() -> Option<(u8, String, String)> {
    let (phase, source, result, action) = state_snapshot();
    if phase != Phase::Done || source.is_empty() || result.is_empty() {
        return None;
    }
    Some((action, source, result))
}

pub fn hide() {
    let h = HWND_CELL.swap(0, Ordering::SeqCst);
    if h != 0 {
        unsafe { DestroyWindow(h as HWND) };
    }
}

fn ensure_window() {
    if HWND_CELL.load(Ordering::SeqCst) != 0 {
        redraw();
        return;
    }
    ensure_class();
    let (w, h) = (scale(WIDTH), scale(HEIGHT));
    let (mut x, mut y) = (300, 300);
    if let Some((cx, top, bottom)) = crate::ipc::caret_screen_rect() {
        x = cx;
        y = bottom + scale(8);
        if y + h > 0 {
            let _ = top;
        }
    }
    let mut area: RECT = unsafe { std::mem::zeroed() };
    unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut area as *mut RECT as *mut _, 0) };
    if y + h > area.bottom {
        y = (y - h - scale(40)).max(area.top);
    }
    if x + w > area.right {
        x = area.right - w - scale(8);
    }
    if x < area.left {
        x = area.left + scale(8);
    }

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
    unsafe {
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(hwnd, TIMER_POLL, POLL_MS, None);
    }
    crate::log_line(&format!("ai-panel: shown at {x},{y} {w}x{h}"));
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
        DT_LEFT | DT_WORDBREAK
    } else {
        DT_LEFT | DT_SINGLELINE | DT_VCENTER
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

unsafe fn paint(hwnd: HWND) {
    let mut ps: PAINTSTRUCT = unsafe { std::mem::zeroed() };
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let mut client: RECT = unsafe { std::mem::zeroed() };
    unsafe { GetClientRect(hwnd, &mut client) };
    let (w, h) = (client.right, client.bottom);
    let dark = crate::config::snapshot().dark;
    let pal = palette(dark);
    let (phase, source, result, _action) = state_snapshot();
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

    // 关闭 ×
    let close_font = make_font(15, 400);
    unsafe { text_block(hdc, &l.close, "×", pal.dim, close_font, false) };

    // 源文框
    let field = unsafe { CreateSolidBrush(pal.field) };
    unsafe { FillRect(hdc, &l.source_box, field) };
    unsafe { DeleteObject(field as HGDIOBJ) };
    let body = make_font(13, 400);
    let small = make_font(11, 400);
    let source_text = match phase {
        Phase::Disabled => "AI 未启用".to_string(),
        Phase::Loading => "正在读取选中文字…".to_string(),
        Phase::NoSelection => "先在文档里选中一段文字，再点「AI」".to_string(),
        _ => {
            if source.is_empty() {
                "（没有选中文字）".to_string()
            } else {
                source.clone()
            }
        }
    };
    let inner = RECT {
        left: l.source_box.left + scale(10),
        top: l.source_box.top + scale(8),
        right: l.source_box.right - scale(10),
        bottom: l.source_box.bottom - scale(8),
    };
    unsafe {
        text_block(
            hdc,
            &inner,
            &source_text,
            if phase == Phase::NoSelection || phase == Phase::Disabled {
                pal.dim
            } else {
                pal.text
            },
            body,
            true,
        )
    };

    // 动作 / 设置按钮
    if phase == Phase::Disabled {
        unsafe { button(hdc, &l.settings, "打开设置", true, true, pal, body) };
    } else {
        let labels = ["翻译", "改写", "续写"];
        let enabled = matches!(phase, Phase::Ready | Phase::Done | Phase::Error);
        for (i, label) in labels.iter().enumerate() {
            let active = enabled && phase != Phase::Busy;
            unsafe { button(hdc, &l.actions[i], label, false, active, pal, body) };
        }
    }

    // 结果框
    let (result_text, fg) = match phase {
        Phase::Disabled => (
            "设置里勾选「启用 AI」并填写接口地址，然后回到这里试。".to_string(),
            pal.dim,
        ),
        Phase::Loading => ("…".to_string(), pal.dim),
        Phase::NoSelection => ("选中文字后，面板会自动读取。".to_string(), pal.dim),
        Phase::Ready => ("选择上面一种处理方式。".to_string(), pal.dim),
        Phase::Busy => ("生成中…".to_string(), pal.dim),
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
    let confirm_on = phase == Phase::Done;
    unsafe { button(hdc, &l.confirm, "确定", true, confirm_on, pal, body) };
    unsafe { button(hdc, &l.cancel, "取消", false, true, pal, body) };
    let regen_on = matches!(phase, Phase::Done | Phase::Error);
    unsafe { button(hdc, &l.regen, "重新生成", false, regen_on, pal, body) };

    // 提示行
    let hint = "选中的文字会替换成结果；取消/Esc 不动文档";
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
            pal.dim,
            small,
            false,
        )
    };

    unsafe { DeleteObject(title) };
    unsafe { DeleteObject(close_font) };
    unsafe { DeleteObject(body) };
    unsafe { DeleteObject(small) };
    unsafe { EndPaint(hwnd, &mut ps) };
}

fn key_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

fn cursor_in(hwnd: HWND) -> bool {
    let mut pt = POINT { x: 0, y: 0 };
    let mut r: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        GetCursorPos(&mut pt);
        GetWindowRect(hwnd, &mut r);
    }
    pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
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
                Hit::Close | Hit::Cancel => crate::ai::cancel_panel(),
                Hit::Action(i) => crate::ai::send(i),
                Hit::Confirm => crate::ai::confirm(),
                Hit::Regenerate => crate::ai::regenerate(),
                Hit::Settings => crate::ai::open_settings(),
                Hit::None => {}
            }
            0
        }
        WM_KEYDOWN => {
            if wp as u16 == VK_ESCAPE as u16 {
                crate::ai::cancel_panel();
            }
            0
        }
        WM_TIMER => {
            if key_down(VK_ESCAPE) || (key_down(VK_LBUTTON) && !cursor_in(hwnd)) {
                crate::ai::cancel_panel();
            }
            0
        }
        WM_DESTROY => {
            unsafe { KillTimer(hwnd, TIMER_POLL) };
            HWND_CELL.store(0, Ordering::SeqCst);
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn ensure_class() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
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
    fn 底部按钮顺序是重新生成_取消_确定() {
        let l = layout(scale(WIDTH), scale(HEIGHT));
        assert!(l.regen.right <= l.cancel.left);
        assert!(l.cancel.right <= l.confirm.left);
        assert!(l.confirm.right <= scale(WIDTH));
    }

    #[test]
    fn 命中测试按状态区分() {
        let (w, h) = (scale(WIDTH), scale(HEIGHT));
        let l = layout(w, h);
        let c = |r: &RECT| ((r.left + r.right) / 2, (r.top + r.bottom) / 2);

        let (x, y) = c(&l.actions[0]);
        assert_eq!(hit(x, y, w, h, Phase::Ready), Hit::Action(0));
        assert_eq!(hit(x, y, w, h, Phase::Busy), Hit::None, "生成中动作不可点");
        // 未启用时第一格位置被「打开设置」占了，用第二格验"动作不可点"
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

        let (x, y) = c(&l.settings);
        assert_eq!(hit(x, y, w, h, Phase::Disabled), Hit::Settings);
    }
}
