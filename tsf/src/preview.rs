//! AI 结果确认框（1.1.40）。
//!
//! 用户要求"三个 AI 动作完成后先弹一个框，点确定再替换"——这里就是那个框。
//!
//! ## 为什么是独立窗口、且不抢焦点
//!
//! - 不能阻塞在浮条的窗口过程里等用户点（那会把宿主的 UI 消息循环卡住），所以是
//!   **非模态**窗口：`show()` 建完就返回，用户的点击走自己的窗口过程。
//! - 复用浮条那套 `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST`：
//!   弹出时不打断用户正在打字的焦点，点了"确定"之后 AI 的编辑会话才排得上队。
//! - 不抢焦点就拿不到 `WM_KEYDOWN`，所以 Esc / 点外面用 50ms 的定时器轮询
//!   （和 ☰ 菜单同一个套路，见 ADR-0008）。
//!
//! ## 只显示、不负责业务
//!
//! 结果文本和"确定之后做什么"都不在这里：`ai.rs` 把待确认的结果放在 `PENDING`，
//! 这里只负责画和收点击，点确定时回调 `ai::confirm_pending()`。

use std::sync::Mutex;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DT_CENTER, DT_SINGLELINE, DT_VCENTER,
    DT_WORDBREAK, DeleteObject, DrawTextW, EndPaint, FillRect, GetStockObject, HGDIOBJ, NULL_BRUSH,
    PAINTSTRUCT, Rectangle, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE, VK_LBUTTON};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetCursorPos, GetWindowRect,
    IDC_ARROW, KillTimer, LoadCursorW, MA_NOACTIVATE, RegisterClassW, SPI_GETWORKAREA, SetTimer,
    ShowWindow, SystemParametersInfoW, WM_DESTROY, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE,
    WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const CLASS_NAME: &str = "inklink-ai-preview";
const TIMER_POLL: usize = 1;
const POLL_MS: u32 = 50;

/// 预览框最多显示这么多字；再多就截断（应用时用的是全文）。
const MAX_PREVIEW_CHARS: usize = 600;

const PAD: i32 = 14;
const BTN_W: i32 = 64;
const BTN_H: i32 = 28;
const WIDTH: i32 = 380;
const HEIGHT: i32 = 230;

struct Palette {
    bg: u32,
    border: u32,
    text: u32,
    dim: u32,
    accent: u32,
    btn: u32,
}

const LIGHT: Palette = Palette {
    bg: 0x00_FF_FF_FF,
    border: 0x00_D6_D6_D6,
    text: 0x00_20_20_20,
    dim: 0x00_8A_8A_8A,
    accent: 0x00_00_66_FF,
    btn: 0x00_F2_F2_F2,
};

const DARK: Palette = Palette {
    bg: 0x00_26_22_22,
    border: 0x00_46_41_41,
    text: 0x00_F1_EC_EC,
    dim: 0x00_97_8E_8E,
    accent: 0x00_3D_9E_FF,
    btn: 0x00_38_33_33,
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

static HWND_CELL: AtomicIsize = AtomicIsize::new(0);
/// 当前要显示的文字（也用来判断"窗口还活着"）。
static TEXT: Mutex<Option<String>> = Mutex::new(None);

/// 两个按钮的矩形（客户区坐标），纯函数好测。
fn buttons(w: i32, h: i32) -> (RECT, RECT) {
    let pad = scale(PAD);
    let bw = scale(BTN_W);
    let bh = scale(BTN_H);
    let bottom = h - pad;
    let cancel = RECT {
        left: w - pad - bw,
        top: bottom - bh,
        right: w - pad,
        bottom,
    };
    let ok = RECT {
        left: cancel.left - scale(8) - bw,
        top: cancel.top,
        right: cancel.left - scale(8),
        bottom: cancel.bottom,
    };
    (ok, cancel)
}

/// 正文区域：标题下面、按钮上面。
fn body_rect(w: i32, h: i32) -> RECT {
    let pad = scale(PAD);
    let title_h = scale(22);
    let (_, cancel) = buttons(w, h);
    RECT {
        left: pad,
        top: pad + title_h,
        right: w - pad,
        bottom: cancel.top - scale(6),
    }
}

/// 1 = 确定，2 = 取消，0 = 都不是。
fn hit(x: i32, y: i32, w: i32, h: i32) -> u8 {
    let (ok, cancel) = buttons(w, h);
    let inside = |r: &RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
    if inside(&ok) {
        1
    } else if inside(&cancel) {
        2
    } else {
        0
    }
}

fn make_font(px: i32, bold: i32) -> HGDIOBJ {
    let face = wide("Microsoft YaHei UI");
    unsafe {
        CreateFontW(
            -scale(px),
            0,
            0,
            0,
            bold,
            0,
            0,
            0,
            1,
            0,
            0,
            4, // ANTIALIASED，小字号彩色文字不能用 ClearType
            0,
            face.as_ptr(),
        ) as HGDIOBJ
    }
}

fn text_of(s: &str) -> String {
    let clipped: String = s.chars().take(MAX_PREVIEW_CHARS).collect();
    if s.chars().count() > MAX_PREVIEW_CHARS {
        format!("{clipped}…")
    } else {
        clipped
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

    let bg = unsafe { CreateSolidBrush(pal.bg) };
    unsafe { FillRect(hdc, &client, bg) };
    unsafe { DeleteObject(bg as HGDIOBJ) };

    // 1px 边框
    let brush = unsafe { GetStockObject(NULL_BRUSH) };
    let old_brush = unsafe { SelectObject(hdc, brush) };
    let pen = unsafe { CreatePen(0, 1, pal.border) };
    let old_pen = unsafe { SelectObject(hdc, pen as HGDIOBJ) };
    unsafe { Rectangle(hdc, 0, 0, w, h) };
    unsafe { SelectObject(hdc, old_pen) };
    unsafe { SelectObject(hdc, old_brush) };
    unsafe { DeleteObject(pen as HGDIOBJ) };

    unsafe { SetBkMode(hdc, TRANSPARENT as i32) };

    // 标题
    let title_font = make_font(13, 700);
    let old_font = unsafe { SelectObject(hdc, title_font) };
    unsafe { SetTextColor(hdc, pal.text) };
    let title = wide("AI 结果预览");
    let mut tr = RECT {
        left: scale(PAD),
        top: scale(PAD),
        right: w - scale(PAD),
        bottom: scale(PAD) + scale(22),
    };
    unsafe {
        DrawTextW(
            hdc,
            title.as_ptr(),
            (title.len() - 1) as i32,
            &mut tr,
            DT_SINGLELINE | DT_VCENTER,
        )
    };

    // 正文
    let body_font = make_font(13, 400);
    unsafe { SelectObject(hdc, body_font) };
    let text = TEXT
        .lock()
        .map(|g| g.clone().unwrap_or_default())
        .unwrap_or_default();
    let buf = wide(&text_of(&text));
    let mut br = body_rect(w, h);
    unsafe {
        DrawTextW(
            hdc,
            buf.as_ptr(),
            (buf.len() - 1) as i32,
            &mut br,
            DT_WORDBREAK,
        )
    };

    // 按钮
    let (ok, cancel) = buttons(w, h);
    for (r, label, primary) in [(&ok, "确定", true), (&cancel, "取消", false)] {
        let fill = unsafe { CreateSolidBrush(if primary { pal.accent } else { pal.btn }) };
        unsafe { FillRect(hdc, r, fill) };
        unsafe { DeleteObject(fill as HGDIOBJ) };
        let p = unsafe { CreatePen(0, 1, if primary { pal.accent } else { pal.border }) };
        let oldp = unsafe { SelectObject(hdc, p as HGDIOBJ) };
        let nb = unsafe { GetStockObject(NULL_BRUSH) };
        let oldb = unsafe { SelectObject(hdc, nb) };
        unsafe { Rectangle(hdc, r.left, r.top, r.right, r.bottom) };
        unsafe { SelectObject(hdc, oldb) };
        unsafe { SelectObject(hdc, oldp) };
        unsafe { DeleteObject(p as HGDIOBJ) };

        unsafe { SetTextColor(hdc, if primary { 0x00_FF_FF_FF } else { pal.text }) };
        let l = wide(label);
        let mut lr = *r;
        unsafe {
            DrawTextW(
                hdc,
                l.as_ptr(),
                (l.len() - 1) as i32,
                &mut lr,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            )
        };
    }

    // 提示
    let hint = wide("点击「确定」应用到文档；Esc 或点外面取消");
    unsafe { SetTextColor(hdc, pal.dim) };
    let mut hr = RECT {
        left: scale(PAD),
        top: cancel.bottom + scale(2),
        right: w - scale(PAD),
        bottom: cancel.bottom + scale(18),
    };
    unsafe {
        DrawTextW(
            hdc,
            hint.as_ptr(),
            (hint.len() - 1) as i32,
            &mut hr,
            DT_SINGLELINE,
        )
    };

    unsafe { SelectObject(hdc, old_font) };
    unsafe { DeleteObject(title_font) };
    unsafe { DeleteObject(body_font) };
    unsafe { EndPaint(hwnd, &mut ps) };
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

fn key_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
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
            match hit(x, y, client.right, client.bottom) {
                1 => {
                    unsafe { DestroyWindow(hwnd) };
                    crate::ai::confirm_pending();
                }
                2 => {
                    unsafe { DestroyWindow(hwnd) };
                    crate::ai::cancel_pending();
                }
                _ => {
                    unsafe { DestroyWindow(hwnd) };
                    crate::ai::cancel_pending();
                }
            }
            0
        }
        WM_KEYDOWN => {
            if wp as u16 == VK_ESCAPE as u16 {
                unsafe { DestroyWindow(hwnd) };
                crate::ai::cancel_pending();
            }
            0
        }
        WM_TIMER => {
            // 没焦点收不到 Esc，只能轮询；点外面也取消。
            if key_down(VK_ESCAPE) || (key_down(VK_LBUTTON) && !cursor_in(hwnd)) {
                unsafe { DestroyWindow(hwnd) };
                crate::ai::cancel_pending();
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

/// 弹出一个确认框。已有一个就关掉旧的（后来的结果覆盖前面的）。
pub fn show(text: &str) {
    ensure_class();
    let old = HWND_CELL.swap(0, Ordering::SeqCst);
    if old != 0 {
        unsafe { DestroyWindow(old as HWND) };
    }
    if let Ok(mut g) = TEXT.lock() {
        *g = Some(text.to_string());
    }

    let (w, h) = (scale(WIDTH), scale(HEIGHT));
    // 位置：优先贴光标下方，放不下就上方；再夹到工作区。
    let mut x = 200;
    let mut y = 200;
    if let Some((cx, _top, bottom)) = crate::ipc::caret_screen_rect() {
        x = cx;
        y = bottom + scale(6);
    }
    let mut area: RECT = unsafe { std::mem::zeroed() };
    unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut area as *mut RECT as *mut _, 0) };
    if y + h > area.bottom {
        y = (y - h - scale(30)).max(area.top);
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
        crate::log_line("preview: CreateWindowExW failed");
        // 窗口建不出来也不能丢结果：直接把待确认的结果应用掉
        crate::ai::confirm_pending();
        return;
    }
    HWND_CELL.store(hwnd as isize, Ordering::SeqCst);
    unsafe {
        ShowWindow(
            hwnd,
            windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE,
        );
        SetTimer(hwnd, TIMER_POLL, POLL_MS, None);
    }
    // 这里不跑消息循环：点击由宿主线程的消息泵送到 wndproc。
    crate::log_line(&format!("preview: shown at {x},{y} {w}x{h}"));
}

/// 当前预览框还开着吗（调试/测试用）。
#[allow(dead_code)]
pub fn is_open() -> bool {
    HWND_CELL.load(Ordering::SeqCst) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 按钮在右下角且不重叠() {
        let (ok, cancel) = buttons(scale(WIDTH), scale(HEIGHT));
        assert!(ok.right <= cancel.left, "确定在取消左边");
        assert!(cancel.right <= scale(WIDTH), "取消不出右边");
        assert_eq!(ok.top, cancel.top);
        assert_eq!(ok.bottom, cancel.bottom);
    }

    #[test]
    fn 点按钮命中测试() {
        let (w, h) = (scale(WIDTH), scale(HEIGHT));
        let (ok, cancel) = buttons(w, h);
        assert_eq!(
            hit((ok.left + ok.right) / 2, (ok.top + ok.bottom) / 2, w, h),
            1
        );
        assert_eq!(
            hit(
                (cancel.left + cancel.right) / 2,
                (cancel.top + cancel.bottom) / 2,
                w,
                h
            ),
            2
        );
        assert_eq!(hit(5, 5, w, h), 0, "正文区不命中按钮");
    }

    #[test]
    fn 正文区域在标题和按钮之间() {
        let (w, h) = (scale(WIDTH), scale(HEIGHT));
        let body = body_rect(w, h);
        let (_, cancel) = buttons(w, h);
        assert!(body.top > scale(PAD), "正文在标题下面");
        assert!(body.bottom <= cancel.top, "正文不压按钮");
    }

    #[test]
    fn 预览文本超长会截断() {
        let long = "字".repeat(MAX_PREVIEW_CHARS + 10);
        let shown = text_of(&long);
        assert_eq!(
            shown.chars().count(),
            MAX_PREVIEW_CHARS + 1,
            "截断后带省略号"
        );
        assert!(shown.ends_with('…'));
    }
}
