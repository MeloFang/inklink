//! inklink 设置 —— 一页原生小窗，只改 `HKCU\Software\inklink\Config` 那五个值。
//!
//! 为什么是这么小一个程序：现在可配的就五项（暗色、列数、行数、撤回窗口、学习开关），
//! 为五项做六页设置台是过度设计。等 P4/P6 把配置撑到几十项、需要分组和长列表时，
//! 再考虑换 webview 那套（见 PLAN.md 的 P2b）。
//!
//! 为什么用系统控件而不是自绘：浮条和候选窗自绘是因为它们要跟着主题变、还要在
//! 宿主进程里跑（不能引 UI 框架）；设置窗是独立进程、一天用一次，系统控件够用。
//! 代价是它不跟暗色 —— 这一点写进 README 比让用户猜好。
//!
//! 值的读写规则与 `tsf/src/config.rs` 必须一致，否则设置台和 DLL 会各说各话：
//! 都写 DWORD，`Dark`/`Learn` 用 0/1，列数 3–9、行数 1–9、撤回窗口 0–10000。

use std::cell::RefCell;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{GetStockObject, DEFAULT_GUI_FONT, HGDIOBJ};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_READ, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, GetWindowTextW,
    IsDialogMessageW, MessageBoxW, PostQuitMessage, RegisterClassW, SendMessageW, SetWindowTextW,
    ShowWindow, TranslateMessage, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, CW_USEDEFAULT,
    ES_NUMBER, MB_ICONERROR, MB_OK, MSG, SW_SHOW, WM_COMMAND, WM_CREATE, WM_DESTROY, WNDCLASSW,
    WS_BORDER, WS_CAPTION, WS_CHILD, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};

const KEY_PATH: &str = r"Software\inklink\Config";

const ID_COLS: usize = 101;
const ID_ROWS: usize = 102;
const ID_DARK: usize = 103;
const ID_LEARN: usize = 104;
const ID_UNDO: usize = 105;
const ID_SAVE: usize = 1001;
const ID_RESET: usize = 1002;
const ID_CLOSE: usize = 1003;

const COLS_RANGE: (u32, u32) = (3, 9);
const ROWS_RANGE: (u32, u32) = (1, 9);
const UNDO_RANGE: (u32, u32) = (0, 10_000);

/// 与 `tsf/src/config.rs` 的默认值保持一致：升级安装不能改变手感。
const DEF_COLS: u32 = 5;
const DEF_ROWS: u32 = 5;
const DEF_UNDO: u32 = 2000;

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui::default());
}

#[derive(Default)]
struct Ui {
    cols: HWND,
    rows: HWND,
    dark: HWND,
    learn: HWND,
    undo: HWND,
    status: HWND,
}

/// 把界面上填的值收成配置。返回 `Err` 就弹提示、不写注册表。
fn read_form(ui: &Ui) -> Result<Config, String> {
    let cols = parse_in(ui.cols, "每页候选的列数", COLS_RANGE)?;
    let rows = parse_in(ui.rows, "每页候选的行数", ROWS_RANGE)?;
    let undo = parse_in(ui.undo, "撤回窗口毫秒数", UNDO_RANGE)?;
    Ok(Config {
        dark: checked(ui.dark),
        learn: checked(ui.learn),
        cols,
        rows,
        undo_ms: undo,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Config {
    dark: bool,
    learn: bool,
    cols: u32,
    rows: u32,
    undo_ms: u32,
}

fn checked(h: HWND) -> bool {
    unsafe { SendMessageW(h, BM_GETCHECK as u32, 0, 0) == 1 }
}

fn text_of(h: HWND) -> String {
    let mut buf = [0u16; 32];
    let n = unsafe { GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32) };
    let n = if n < 0 { 0 } else { n as usize };
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}

/// 解析并夹到合法区间。空白直接当默认值（用户清空输入框是常见操作）。
fn parse_in(h: HWND, what: &str, range: (u32, u32)) -> Result<u32, String> {
    parse_clamped(&text_of(h), range)
        .ok_or_else(|| format!("{what} 要在 {} 到 {} 之间。", range.0, range.1))
}

/// 空白 → `None` 由调用方决定；这里只做"能不能解析成区间内的数"。
/// 抽成纯函数是为了能单测：UI 逻辑没法在这里跑。
fn parse_clamped(s: &str, range: (u32, u32)) -> Option<u32> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    let v: u32 = t.parse().ok()?;
    if v < range.0 || v > range.1 {
        return None;
    }
    Some(v)
}

// ── 注册表 ─────────────────────────────────────────────────────

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn open_key(access: u32) -> Option<HKEY> {
    let path = wide(KEY_PATH);
    let mut h: HKEY = std::ptr::null_mut();
    let rc = RegCreateKeyExW(
        HKEY_CURRENT_USER,
        path.as_ptr(),
        0,
        std::ptr::null_mut(),
        REG_OPTION_NON_VOLATILE,
        access,
        std::ptr::null_mut(),
        &mut h,
        std::ptr::null_mut(),
    );
    if rc == 0 {
        Some(h)
    } else {
        None
    }
}

unsafe fn read_dword(h: HKEY, name: &str) -> Option<u32> {
    let n = wide(name);
    let mut ty = 0u32;
    let mut val = 0u32;
    let mut cb = 4u32;
    let rc = RegQueryValueExW(
        h,
        n.as_ptr(),
        std::ptr::null_mut(),
        &mut ty,
        &mut val as *mut u32 as *mut u8,
        &mut cb,
    );
    if rc == 0 && ty == REG_DWORD {
        Some(val)
    } else {
        None
    }
}

unsafe fn write_dword(h: HKEY, name: &str, v: u32) -> bool {
    let n = wide(name);
    RegSetValueExW(
        h,
        n.as_ptr(),
        0,
        REG_DWORD,
        &v as *const u32 as *const u8,
        4,
    ) == 0
}

fn load_config() -> Config {
    unsafe {
        let Some(h) = open_key(KEY_READ) else {
            return Config {
                dark: false,
                learn: true,
                cols: DEF_COLS,
                rows: DEF_ROWS,
                undo_ms: DEF_UNDO,
            };
        };
        // 缺省语义必须和 DLL 一致：Dark 缺失=浅色（只有非 0 才算开），
        // Learn 缺失=开（只有明确写 0 才算关）。
        let dark = matches!(read_dword(h, "Dark"), Some(v) if v != 0);
        let learn = !matches!(read_dword(h, "Learn"), Some(0));
        let cols = read_dword(h, "Cols")
            .filter(|v| *v >= COLS_RANGE.0 && *v <= COLS_RANGE.1)
            .unwrap_or(DEF_COLS);
        let rows = read_dword(h, "Rows")
            .filter(|v| *v >= ROWS_RANGE.0 && *v <= ROWS_RANGE.1)
            .unwrap_or(DEF_ROWS);
        let undo = read_dword(h, "UndoMs")
            .filter(|v| *v <= UNDO_RANGE.1)
            .unwrap_or(DEF_UNDO);
        RegCloseKey(h);
        Config {
            dark,
            learn,
            cols,
            rows,
            undo_ms: undo,
        }
    }
}

fn save_config(c: Config) -> bool {
    unsafe {
        let Some(h) = open_key(KEY_READ | KEY_WRITE) else {
            return false;
        };
        let ok = write_dword(h, "Dark", if c.dark { 1 } else { 0 })
            && write_dword(h, "Learn", if c.learn { 1 } else { 0 })
            && write_dword(h, "Cols", c.cols)
            && write_dword(h, "Rows", c.rows)
            && write_dword(h, "UndoMs", c.undo_ms);
        RegCloseKey(h);
        ok
    }
}

// ── 窗口 ───────────────────────────────────────────────────────

fn wide_static(s: &str) -> &'static [u16] {
    // 控件文字要在窗口生命周期内一直有效；泄漏几个几十字节的 Vec 比管生命周期划算。
    Box::leak(wide(s).into_boxed_slice())
}

unsafe fn child(
    class: &str,
    text: &str,
    style: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    parent: HWND,
    id: usize,
) -> HWND {
    let cls = wide(class);
    CreateWindowExW(
        0,
        cls.as_ptr(),
        wide_static(text).as_ptr(),
        WS_CHILD | WS_VISIBLE | style,
        x,
        y,
        w,
        h,
        parent,
        id as *mut core::ffi::c_void,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    )
}

unsafe fn set_text(h: HWND, s: &str) {
    SetWindowTextW(h, wide_static(s).as_ptr());
}

unsafe fn build_ui(hwnd: HWND) {
    let font = GetStockObject(DEFAULT_GUI_FONT) as HGDIOBJ;
    let mut add = |h: HWND| {
        SendMessageW(
            h,
            0x0030, // WM_SETFONT
            font as usize,
            1,
        );
    };

    let label = |t: &str, x: i32, y: i32, w: i32| {
        let h = child("STATIC", t, 0, x, y, w, 18, hwnd, 0);
        add(h);
        h
    };
    let edit = |t: &str, x: i32, y: i32, w: i32, id: usize| {
        let h = child(
            "EDIT",
            t,
            WS_BORDER | WS_TABSTOP | ES_NUMBER as u32,
            x,
            y,
            w,
            22,
            hwnd,
            id,
        );
        SendMessageW(h, 0x0030, font as usize, 1);
        h
    };
    let check = |t: &str, x: i32, y: i32, w: i32, id: usize| {
        let h = child(
            "BUTTON",
            t,
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            x,
            y,
            w,
            20,
            hwnd,
            id,
        );
        add(h);
        h
    };
    let button = |t: &str, x: i32, y: i32, w: i32, id: usize| {
        let h = child("BUTTON", t, WS_TABSTOP, x, y, w, 26, hwnd, id);
        add(h);
        h
    };

    let c = load_config();

    label("每页候选", 16, 18, 80);
    let cols = edit(&c.cols.to_string(), 100, 14, 44, ID_COLS);
    label("列 ×", 152, 18, 34);
    let rows = edit(&c.rows.to_string(), 186, 14, 44, ID_ROWS);
    label("行", 238, 18, 24);

    let dark = check("暗色主题（候选窗、浮条、菜单一起变）", 16, 50, 320, ID_DARK);
    if c.dark {
        SendMessageW(dark, BM_SETCHECK as u32, 1, 0);
    }

    let learn = check("从我的输入里学词（关掉就完全不记）", 16, 78, 320, ID_LEARN);
    if c.learn {
        SendMessageW(learn, BM_SETCHECK as u32, 1, 0);
    }

    label("撤回窗口（毫秒，0 = 关掉撤回）", 16, 112, 240);
    let undo = edit(&c.undo_ms.to_string(), 16, 134, 80, ID_UNDO);

    let status = child(
        "STATIC",
        "改完点保存，下一次按键就生效，不用重启。",
        0,
        16,
        166,
        300,
        18,
        hwnd,
        0,
    );
    add(status);

    button("保存", 16, 196, 76, ID_SAVE);
    button("恢复默认", 100, 196, 90, ID_RESET);
    button("关闭", 198, 196, 76, ID_CLOSE);

    UI.with(|u| {
        let mut u = u.borrow_mut();
        u.cols = cols;
        u.rows = rows;
        u.dark = dark;
        u.learn = learn;
        u.undo = undo;
        u.status = status;
    });
}

fn on_save(hwnd: HWND) {
    let ui = UI.with(|u| u.borrow().to_snapshot());
    match read_form(&ui) {
        Ok(c) => {
            if save_config(c) {
                UI.with(|u| unsafe {
                    set_text(u.borrow().status, "已保存 —— 下一次按键生效。")
                });
            } else {
                unsafe {
                    let msg = wide("写注册表失败。\n\n可能是权限问题，试试以管理员身份运行。");
                    MessageBoxW(
                        hwnd,
                        msg.as_ptr(),
                        wide("inklink").as_ptr(),
                        MB_OK | MB_ICONERROR,
                    );
                }
            }
        }
        Err(e) => unsafe {
            let msg = wide(&e);
            MessageBoxW(
                hwnd,
                msg.as_ptr(),
                wide("inklink").as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        },
    }
}

impl Ui {
    fn to_snapshot(&self) -> Ui {
        Ui {
            cols: self.cols,
            rows: self.rows,
            dark: self.dark,
            learn: self.learn,
            undo: self.undo,
            status: self.status,
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            build_ui(hwnd);
            0
        }
        WM_COMMAND => {
            match (wp & 0xFFFF) as usize {
                ID_SAVE => on_save(hwnd),
                ID_RESET => {
                    let ui = UI.with(|u| u.borrow().to_snapshot());
                    set_text(ui.cols, &DEF_COLS.to_string());
                    set_text(ui.rows, &DEF_ROWS.to_string());
                    set_text(ui.undo, &DEF_UNDO.to_string());
                    SendMessageW(ui.dark, BM_SETCHECK as u32, 0, 0);
                    SendMessageW(ui.learn, BM_SETCHECK as u32, 1, 0);
                    set_text(ui.status, "已填回默认值 —— 还要点一次“保存”才写进去。");
                }
                ID_CLOSE => {
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn main() {
    unsafe {
        let cls = wide("inklink-settings");
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: std::ptr::null_mut(),
            hIcon: std::ptr::null_mut(),
            hCursor: windows_sys::Win32::UI::WindowsAndMessaging::LoadCursorW(
                std::ptr::null_mut(),
                windows_sys::Win32::UI::WindowsAndMessaging::IDC_ARROW,
            ),
            hbrBackground: (windows_sys::Win32::Graphics::Gdi::COLOR_WINDOW + 1)
                as *mut core::ffi::c_void,
            lpszMenuName: std::ptr::null(),
            lpszClassName: cls.as_ptr(),
        };
        if RegisterClassW(&wc) == 0 {
            return;
        }
        let hwnd = CreateWindowExW(
            0,
            cls.as_ptr(),
            wide_static("inklink 设置").as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            320,
            270,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if hwnd.is_null() {
            return;
        }
        ShowWindow(hwnd, SW_SHOW);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(hwnd, &msg) != 0 {
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 数字要在区间内才算数() {
        assert_eq!(parse_clamped("5", COLS_RANGE), Some(5));
        assert_eq!(parse_clamped("  5 ", COLS_RANGE), Some(5));
        assert_eq!(parse_clamped("3", COLS_RANGE), Some(3));
        assert_eq!(parse_clamped("9", COLS_RANGE), Some(9));
        assert_eq!(parse_clamped("2", COLS_RANGE), None);
        assert_eq!(parse_clamped("10", COLS_RANGE), None);
        assert_eq!(parse_clamped("", COLS_RANGE), None);
        assert_eq!(parse_clamped("abc", COLS_RANGE), None);
        assert_eq!(parse_clamped("5.5", COLS_RANGE), None);
        assert_eq!(parse_clamped("-1", COLS_RANGE), None);
    }

    #[test]
    fn 撤回窗口允许_0_表示关掉() {
        assert_eq!(parse_clamped("0", UNDO_RANGE), Some(0));
        assert_eq!(parse_clamped("2000", UNDO_RANGE), Some(2000));
        assert_eq!(parse_clamped("10000", UNDO_RANGE), Some(10000));
        assert_eq!(parse_clamped("10001", UNDO_RANGE), None);
    }
}
