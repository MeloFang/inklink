//! 自绘的弹出菜单。
//!
//! 为什么不用系统的 `TrackPopupMenu`：
//!
//! 浮条是 `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST` 的窗口，
//! `SetForegroundWindow` 在它身上**必然失败**（实测返回 0）。而 `TrackPopupMenu`
//! 要求宿主窗口是前台，否则菜单要么画不出来、要么点不掉，函数会一直卡在它自己的
//! 模态循环里 —— 表现为"点一下浮条就变成一条白带、整个浮条不再响应"，
//! 甚至崩在 user32（`0xc000041d`：异常从回调里逃出去）。
//!
//! 所以菜单自己画：一个独立的小 popup 窗口 + 自己跑一个消息循环
//! （选中一项 / 点外面 / Esc 都销毁窗口 → 循环退出）。外观跟浮条同一套配色字体，
//! 命中测试是几个纯函数，好测。
//!
//! ## 与 `ui/src/menu.rs` 的差别（这是跑在宿主进程里的版本）
//!
//! - 去掉「开机自启」「退出浮条」两项：进程内浮条没有这两个概念（不常驻、不能退）；
//! - 循环里 **`PeekMessageW` 只取菜单窗口自己的消息**（按 hwnd 过滤），绝不碰宿主
//!   的 `WM_QUIT`，也**没有 `std::process::exit`**（那会杀掉宿主进程）。

use std::sync::atomic::{AtomicI32, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Mutex, Once};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreatePen, CreateSolidBrush, DT_LEFT, DT_SINGLELINE, DT_VCENTER,
    DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, GetStockObject, GetTextExtentPoint32W,
    HGDIOBJ, InvalidateRect, LineTo, MoveToEx, NULL_BRUSH, PAINTSTRUCT, Rectangle, ReleaseDC,
    SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_DOWN, VK_ESCAPE, VK_LBUTTON, VK_RETURN, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    GetClientRect, GetCursorPos, GetWindowRect, IDC_ARROW, LoadCursorW, MA_NOACTIVATE, MSG,
    PM_REMOVE, PeekMessageW, RegisterClassW, SPI_GETWORKAREA, SW_SHOWNOACTIVATE, ShowWindow,
    SystemParametersInfoW, TranslateMessage, WM_CAPTURECHANGED, WM_DESTROY, WM_KEYDOWN,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

pub const CMD_TOGGLE_MODE: u32 = 1;
pub const CMD_TOGGLE_PUNCT: u32 = 2;
/// 亮/暗主题（1.1.40 从浮条挪进菜单）。
pub const CMD_TOGGLE_THEME: u32 = 3;
pub const CMD_SETTINGS: u32 = 4;
pub const CMD_ABOUT: u32 = 5;
/// P4b 的三项 AI 命令。编号和 `ai::ACTION_*` 分开：命令是"菜单项身份"，
/// 动作是"协议里的一个字节"，中间由 `ai::action_for_cmd` 翻译。
pub const CMD_AI_TRANSLATE: u32 = 10;
pub const CMD_AI_REWRITE: u32 = 11;
pub const CMD_AI_CONTINUE: u32 = 12;

const CLASS_NAME: &str = "inklink-menu";

/// 菜单弹出循环把它选中的命令放在这里。
static CHOSEN: AtomicU32 = AtomicU32::new(0);
/// 菜单窗口还活着就是非 0；销毁时置 0，弹出循环据此退出。
static MENU_HWND: AtomicIsize = AtomicIsize::new(0);
static HOVER: AtomicI32 = AtomicI32::new(-1);
static CLASS_REGISTERED: Once = Once::new();
static ITEMS: Mutex<Vec<Item>> = Mutex::new(Vec::new());

// 与浮条同一套配色（COLORREF 是 0x00BBGGRR）。浅色/暗色只换这张表。
struct Palette {
    bg: u32,
    border: u32,
    hover: u32,
    text: u32,
    dim: u32,
    /// 勾选标记的颜色（浅色用橙、暗色用琥珀，和候选窗选中色同族）
    accent: u32,
}

const MENU_LIGHT: Palette = Palette {
    bg: 0x00_FF_FF_FF,
    border: 0x00_D6_D6_D6,
    hover: 0x00_F2_EC_E4,
    text: 0x00_20_20_20,
    dim: 0x00_A8_A8_A8,
    accent: 0x00_00_66_FF,
};

const MENU_DARK: Palette = Palette {
    bg: 0x00_2A_26_26,
    border: 0x00_46_41_41,
    hover: 0x00_38_33_33,
    text: 0x00_F1_EC_EC,
    dim: 0x00_97_8E_8E,
    accent: 0x00_3D_9E_FF,
};

fn palette(dark: bool) -> &'static Palette {
    if dark { &MENU_DARK } else { &MENU_LIGHT }
}

// 96 DPI 下的设计尺寸
const ROW_H: i32 = 26;
const SEP_H: i32 = 9;
const PAD_X: i32 = 10;
const PAD_Y: i32 = 5;
const CHECK_W: i32 = 20;
const MIN_W: i32 = 132;

#[derive(Clone)]
struct Item {
    id: u32,
    label: String,
    checked: bool,
    enabled: bool,
    separator: bool,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn scale(v: i32) -> i32 {
    let dpi = unsafe { GetDpiForSystem() };
    let dpi = if dpi == 0 { 96 } else { dpi as i32 };
    (v * dpi + 48) / 96
}

fn build_items(chinese: bool, punct: bool, ai: bool, dark: bool) -> Vec<Item> {
    let sep = || Item {
        id: 0,
        label: String::new(),
        checked: false,
        enabled: false,
        separator: true,
    };
    // AI 那三项只在"设置里启用 AI"之后才点得动（铁律第三条：默认不联网）。
    // 没启用时灰着，并补一行说明——否则用户只会觉得"点了没反应"。
    let ai_item = |id: u32, label: &str| Item {
        id,
        label: label.to_string(),
        checked: false,
        enabled: ai,
        separator: false,
    };
    let mut items = vec![
        Item {
            id: CMD_TOGGLE_MODE,
            label: if chinese {
                "切换为英文"
            } else {
                "切换为中文"
            }
            .to_string(),
            checked: chinese,
            enabled: true,
            separator: false,
        },
        Item {
            id: CMD_TOGGLE_PUNCT,
            label: "中文标点".to_string(),
            checked: punct,
            enabled: true,
            separator: false,
        },
        Item {
            id: CMD_TOGGLE_THEME,
            label: "暗色主题".to_string(),
            checked: dark,
            enabled: true,
            separator: false,
        },
        sep(),
        ai_item(CMD_AI_TRANSLATE, "翻译选中文字"),
        ai_item(CMD_AI_REWRITE, "改写选中文字"),
        ai_item(CMD_AI_CONTINUE, "续写选中文字"),
    ];
    if !ai {
        items.push(Item {
            id: 0,
            label: "（先到设置里启用 AI）".to_string(),
            checked: false,
            enabled: false,
            separator: false,
        });
    }
    items.push(sep());
    items.push(Item {
        id: CMD_SETTINGS,
        // 1.1.30 起真的有设置窗了（inklink-settings.exe，装在同一个目录）。
        label: "设置…".to_string(),
        checked: false,
        enabled: true,
        separator: false,
    });
    items.push(Item {
        id: CMD_ABOUT,
        label: format!("inklink {}", env!("CARGO_PKG_VERSION")),
        checked: false,
        enabled: false,
        separator: false,
    });
    items
}

fn snapshot() -> Vec<Item> {
    ITEMS.lock().map(|v| v.clone()).unwrap_or_default()
}

fn row_height(it: &Item) -> i32 {
    if it.separator {
        scale(SEP_H)
    } else {
        scale(ROW_H)
    }
}

/// y 落在第几项；落在分隔线或空白上返回 None。
fn row_at(items: &[Item], y: i32) -> Option<usize> {
    let mut top = scale(PAD_Y);
    for (i, it) in items.iter().enumerate() {
        let h = row_height(it);
        if y >= top && y < top + h {
            return if it.separator { None } else { Some(i) };
        }
        top += h;
    }
    None
}

fn make_font() -> HGDIOBJ {
    let face = wide("Microsoft YaHei UI");
    unsafe {
        CreateFontW(
            -scale(13),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            face.as_ptr(),
        ) as HGDIOBJ
    }
}

fn text_width(text: &str) -> i32 {
    let w = wide(text);
    unsafe {
        let hdc = GetDC(std::ptr::null_mut());
        let font = make_font();
        let old = SelectObject(hdc, font);
        let mut sz = SIZE { cx: 0, cy: 0 };
        GetTextExtentPoint32W(hdc, w.as_ptr(), (w.len() - 1) as i32, &mut sz);
        SelectObject(hdc, old);
        DeleteObject(font);
        ReleaseDC(std::ptr::null_mut(), hdc);
        sz.cx
    }
}

fn menu_size(items: &[Item]) -> (i32, i32) {
    let mut width = scale(MIN_W);
    let mut height = scale(PAD_Y) * 2;
    for it in items {
        height += row_height(it);
        if !it.separator {
            let need = text_width(&it.label) + scale(PAD_X) * 2 + scale(CHECK_W);
            if need > width {
                width = need;
            }
        }
    }
    (width, height)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut client: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut client);

            let pal = palette(crate::config::snapshot().dark);
            let bg = CreateSolidBrush(pal.bg);
            FillRect(hdc, &client, bg);
            DeleteObject(bg as HGDIOBJ);

            // 1px 边框
            let brush = GetStockObject(NULL_BRUSH);
            let old_brush = SelectObject(hdc, brush);
            let border = CreatePen(0, 1, pal.border);
            let old_pen = SelectObject(hdc, border as HGDIOBJ);
            Rectangle(hdc, 0, 0, client.right, client.bottom);
            SelectObject(hdc, old_pen);
            SelectObject(hdc, old_brush);
            DeleteObject(border as HGDIOBJ);

            let font = make_font();
            let old_font = SelectObject(hdc, font);
            SetBkMode(hdc, TRANSPARENT as i32);

            let list = snapshot();
            let hover = HOVER.load(Ordering::Relaxed);
            let mut top = scale(PAD_Y);
            for (i, it) in list.iter().enumerate() {
                let h = row_height(it);
                if it.separator {
                    let y = top + h / 2;
                    let pen = CreatePen(0, 1, pal.border);
                    let old = SelectObject(hdc, pen as HGDIOBJ);
                    MoveToEx(hdc, scale(PAD_X), y, std::ptr::null_mut());
                    LineTo(hdc, client.right - scale(PAD_X), y);
                    SelectObject(hdc, old);
                    DeleteObject(pen as HGDIOBJ);
                } else {
                    if i as i32 == hover {
                        let hb = CreateSolidBrush(pal.hover);
                        let r = RECT {
                            left: scale(3),
                            top,
                            right: client.right - scale(3),
                            bottom: top + h,
                        };
                        FillRect(hdc, &r, hb);
                        DeleteObject(hb as HGDIOBJ);
                    }
                    if it.checked {
                        SetTextColor(hdc, pal.accent);
                        let mark = wide("✓");
                        let mut r = RECT {
                            left: scale(PAD_X) - scale(2),
                            top,
                            right: scale(PAD_X) + scale(CHECK_W),
                            bottom: top + h,
                        };
                        DrawTextW(
                            hdc,
                            mark.as_ptr(),
                            (mark.len() - 1) as i32,
                            &mut r,
                            DT_LEFT | DT_SINGLELINE | DT_VCENTER,
                        );
                    }
                    SetTextColor(hdc, if it.enabled { pal.text } else { pal.dim });
                    let label = wide(&it.label);
                    let mut r = RECT {
                        left: scale(PAD_X) + scale(CHECK_W),
                        top,
                        right: client.right - scale(PAD_X),
                        bottom: top + h,
                    };
                    DrawTextW(
                        hdc,
                        label.as_ptr(),
                        (label.len() - 1) as i32,
                        &mut r,
                        DT_LEFT | DT_SINGLELINE | DT_VCENTER,
                    );
                }
                top += h;
            }

            SelectObject(hdc, old_font);
            DeleteObject(font);
            EndPaint(hwnd, &ps);
            0
        }
        WM_MOUSEMOVE => {
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let list = snapshot();
            let hit = row_at(&list, y).map(|i| i as i32).unwrap_or(-1);
            if HOVER.swap(hit, Ordering::Relaxed) != hit {
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
            0
        }
        WM_LBUTTONUP => {
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let list = snapshot();
            if let Some(i) = row_at(&list, y) {
                let it = &list[i];
                if it.enabled {
                    CHOSEN.store(it.id, Ordering::SeqCst);
                }
            }
            DestroyWindow(hwnd);
            0
        }
        WM_RBUTTONUP => {
            DestroyWindow(hwnd);
            0
        }
        WM_CAPTURECHANGED => 0,
        WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
            // 坐标落在客户区外就是"点外面"
            let x = (lparam & 0xFFFF) as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let mut client: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut client);
            if x < 0 || y < 0 || x >= client.right || y >= client.bottom {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_KEYDOWN => {
            let list = snapshot();
            let key = wparam as u16;
            if key == VK_ESCAPE {
                DestroyWindow(hwnd);
            } else if key == VK_UP || key == VK_DOWN {
                let step: i32 = if key == VK_UP { -1 } else { 1 };
                let mut idx = HOVER.load(Ordering::Relaxed);
                for _ in 0..list.len() {
                    idx += step;
                    if idx < 0 {
                        idx = list.len() as i32 - 1;
                    }
                    if idx >= list.len() as i32 {
                        idx = 0;
                    }
                    let it = &list[idx as usize];
                    if !it.separator && it.enabled {
                        break;
                    }
                }
                HOVER.store(idx, Ordering::Relaxed);
                InvalidateRect(hwnd, std::ptr::null(), 0);
            } else if key == VK_RETURN {
                let idx = HOVER.load(Ordering::Relaxed);
                if idx >= 0 && (idx as usize) < list.len() {
                    let it = &list[idx as usize];
                    if it.enabled && !it.separator {
                        CHOSEN.store(it.id, Ordering::SeqCst);
                    }
                }
                DestroyWindow(hwnd);
            }
            0
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_DESTROY => {
            MENU_HWND.store(0, Ordering::SeqCst);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn ensure_class() {
    CLASS_REGISTERED.call_once(|| unsafe {
        let name = wide(CLASS_NAME);
        let mut wc: WNDCLASSW = std::mem::zeroed();
        wc.lpfnWndProc = Some(wndproc);
        wc.hInstance = GetModuleHandleW(std::ptr::null());
        wc.hCursor = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
        wc.hbrBackground = std::ptr::null_mut();
        wc.lpszClassName = name.as_ptr();
        wc.style = CS_HREDRAW | CS_VREDRAW;
        RegisterClassW(&wc);
    });
}

/// 弹出菜单并阻塞到用户选完。返回命令 ID（0 = 点外面/按 Esc）。
pub fn popup(owner: HWND, x: i32, y: i32, chinese: bool, punct: bool, ai: bool, dark: bool) -> u32 {
    ensure_class();
    let list = build_items(chinese, punct, ai, dark);
    if let Ok(mut slot) = ITEMS.lock() {
        *slot = list.clone();
    }
    let (w, h) = menu_size(&list);

    let mut area: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut area as *mut RECT as *mut _, 0);
    }
    // 默认长在浮条上方；上面放不下就放下面；左右贴边时收回来
    let mut px = x;
    let mut py = y - h;
    if py < area.top {
        py = y;
    }
    if px + w > area.right {
        px = area.right - w - 4;
    }
    if px < area.left {
        px = area.left + 4;
    }

    CHOSEN.store(0, Ordering::SeqCst);
    HOVER.store(-1, Ordering::Relaxed);
    let name = wide(CLASS_NAME);
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            name.as_ptr(),
            std::ptr::null(),
            WS_POPUP,
            px,
            py,
            w,
            h,
            owner,
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    if hwnd.is_null() {
        crate::log_line("menu: CreateWindowExW failed");
        return 0;
    }
    MENU_HWND.store(hwnd as isize, Ordering::SeqCst);
    crate::log_line(&format!("menu: created {hwnd:p} at {px},{py} {w}x{h}"));
    unsafe {
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }

    // 自己跑一个"非阻塞"的模态循环。
    //
    // 关键（跑在宿主进程里）：`PeekMessageW` 只取**菜单窗口自己的消息**（按 hwnd
    // 过滤），不碰宿主的 `WM_QUIT`，也绝无 `std::process::exit`。悬停高亮和"点外面 /
    // Esc"用 `GetAsyncKeyState` + 光标位置轮询（`SetCapture` 在 `WS_EX_NOACTIVATE`
    // 窗口上收不到点击，试过不可靠）。
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        let mut prev_lbutton = key_down(VK_LBUTTON);
        let mut prev_escape = key_down(VK_ESCAPE);
        loop {
            while PeekMessageW(&mut msg, hwnd, 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if MENU_HWND.load(Ordering::SeqCst) == 0 {
                break;
            }

            // 悬停高亮也跟着光标走（比等 WM_MOUSEMOVE 稳）
            let inside = cursor_in(hwnd);
            let list = snapshot();
            let hit = if inside {
                row_at(&list, cursor_client_y(hwnd))
                    .map(|i| i as i32)
                    .unwrap_or(-1)
            } else {
                -1
            };
            if HOVER.swap(hit, Ordering::Relaxed) != hit {
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }

            let now_down = key_down(VK_LBUTTON);
            let now_esc = key_down(VK_ESCAPE);
            if now_esc && !prev_escape {
                DestroyWindow(hwnd);
                break;
            }
            if now_down && !prev_lbutton && !inside {
                // 在菜单外面按下左键：关掉菜单。菜单里面的点击走 WM_LBUTTONUP，
                // 这样"按下拖动、松开才选中"的菜单手感才正常。
                DestroyWindow(hwnd);
                break;
            }
            prev_lbutton = now_down;
            prev_escape = now_esc;
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let live = MENU_HWND.load(Ordering::SeqCst) as HWND;
        if !live.is_null() {
            DestroyWindow(live);
        }
        MENU_HWND.store(0, Ordering::SeqCst);
    }
    let cmd = CHOSEN.load(Ordering::SeqCst);
    crate::log_line(&format!("menu: closed chosen={cmd}"));
    cmd
}

/// 这个键此刻是不是按着（全局状态，不看消息队列）。
fn key_down(vk: u16) -> bool {
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

/// 光标是不是在菜单窗口里。
fn cursor_in(hwnd: HWND) -> bool {
    let mut pt = POINT { x: 0, y: 0 };
    let mut r: RECT = unsafe { std::mem::zeroed() };
    unsafe {
        GetCursorPos(&mut pt);
        GetWindowRect(hwnd, &mut r);
    }
    pt.x >= r.left && pt.x < r.right && pt.y >= r.top && pt.y < r.bottom
}

/// 光标在菜单客户区里的 y（不在菜单里就返回 -1）。
fn cursor_client_y(hwnd: HWND) -> i32 {
    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        GetCursorPos(&mut pt);
        windows_sys::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
    }
    pt.y
}

/// 菜单窗口句柄（调试用；0 表示没开）。
#[allow(dead_code)]
pub fn menu_hwnd() -> isize {
    MENU_HWND.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<Item> {
        build_items(true, true, true, false)
    }

    #[test]
    fn rows_plus_padding_fill_the_menu_height() {
        let list = items();
        let (_, h) = menu_size(&list);
        let rows: i32 = list.iter().map(row_height).sum();
        assert_eq!(
            rows + scale(PAD_Y) * 2,
            h,
            "行高 + 上下内边距必须正好等于菜单高度"
        );
    }

    #[test]
    fn hit_test_maps_each_row() {
        let list = items();
        let mut top = scale(PAD_Y);
        for (idx, it) in list.iter().enumerate() {
            let h = row_height(it);
            if !it.separator {
                assert_eq!(row_at(&list, top), Some(idx), "第 {idx} 行上边界");
                assert_eq!(row_at(&list, top + h - 1), Some(idx), "第 {idx} 行下边界");
            }
            top += h;
        }
        assert_eq!(row_at(&list, top), None, "越过底部不该命中");
        assert_eq!(row_at(&list, -5), None, "负坐标不该命中");
        assert_eq!(row_at(&list, scale(PAD_Y) - 1), None, "顶部内边距不该命中");
    }

    #[test]
    fn separators_are_not_rows() {
        let list = items();
        let mut top = scale(PAD_Y);
        for it in &list {
            let h = row_height(it);
            if it.separator {
                assert_eq!(row_at(&list, top), None, "分隔线不该能被点中");
            }
            top += h;
        }
    }

    #[test]
    fn disabled_items_are_still_rows() {
        // 灰掉的「版本号」在命中测试里仍然是"行"，只是点击时不派发命令。
        // 设置项从 1.1.30 起可点了，所以这里改用版本号那行来验。
        let list = items();
        let (_, _) = menu_size(&list);
        let about = list.iter().position(|it| it.id == CMD_ABOUT).unwrap();
        assert!(!list[about].enabled);
        let settings = list.iter().position(|it| it.id == CMD_SETTINGS).unwrap();
        assert!(list[settings].enabled, "设置项现在应该能点");
        let mut top = scale(PAD_Y);
        for it in list.iter().take(about) {
            top += row_height(it);
        }
        assert_eq!(row_at(&list, top + 1), Some(about));
    }

    #[test]
    fn mode_label_follows_state() {
        let cn = build_items(true, true, true, false);
        let en = build_items(false, true, true, false);
        assert_eq!(cn[0].label, "切换为英文");
        assert_eq!(en[0].label, "切换为中文");
        assert!(cn[0].checked, "中文模式下第一项要打勾");
        assert!(!en[0].checked, "英文模式下第一项不打勾");
        assert!(cn[1].checked);
        let off = build_items(true, false, true, false);
        assert!(!off[1].checked);
    }

    #[test]
    fn menu_width_fits_longest_label() {
        let list = items();
        let (w, _) = menu_size(&list);
        assert!(w >= scale(MIN_W), "菜单不能比最小宽度还窄");
        assert!(w < scale(400), "菜单不该宽得离谱：{w}");
    }

    #[test]
    fn no_quit_row() {
        let list = items();
        assert!(
            list.iter().all(|it| it.id != 6),
            "进程内菜单没有「退出浮条」项"
        );
    }

    #[test]
    fn 三个_ai_菜单项按启用状态变灰() {
        let on = build_items(true, true, true, false);
        for id in [CMD_AI_TRANSLATE, CMD_AI_REWRITE, CMD_AI_CONTINUE] {
            let it = on.iter().find(|it| it.id == id).expect("AI 项要在菜单里");
            assert!(it.enabled, "启用 AI 时该项可点");
            assert!(!it.checked, "AI 项不打勾");
            assert!(!it.separator);
        }
        // 顺序：翻译 → 改写 → 续写（动作编号也是 0/1/2）
        let order: Vec<u32> = on
            .iter()
            .filter(|it| crate::ai::action_for_cmd(it.id).is_some())
            .map(|it| it.id)
            .collect();
        assert_eq!(
            order,
            vec![CMD_AI_TRANSLATE, CMD_AI_REWRITE, CMD_AI_CONTINUE]
        );

        let off = build_items(true, true, false, false);
        for id in [CMD_AI_TRANSLATE, CMD_AI_REWRITE, CMD_AI_CONTINUE] {
            let it = off.iter().find(|it| it.id == id).expect("AI 项要在菜单里");
            assert!(!it.enabled, "没启用 AI 时该项要灰掉");
        }
        assert!(
            off.len() == on.len() + 1,
            "没启用时要多一行「先到设置里启用 AI」的说明"
        );
        assert!(off.iter().any(|it| it.label.contains("启用 AI")));
    }

    #[test]
    fn ai_说明行也是灰的且点不中() {
        let off = build_items(true, true, false, false);
        let hint = off
            .iter()
            .position(|it| it.label.contains("启用 AI"))
            .unwrap();
        assert!(!off[hint].enabled);
        assert!(!off[hint].separator, "说明行是普通灰行，不是分隔线");
        // 它能被 row_at 命中（只是点击不派发命令），菜单高度里也要占一格
        let mut top = scale(PAD_Y);
        for it in off.iter().take(hint) {
            top += row_height(it);
        }
        assert_eq!(row_at(&off, top + 1), Some(hint));
    }
}
