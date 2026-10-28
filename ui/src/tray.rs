//! 托盘图标 + 右键菜单。
//!
//! 菜单里的勾选状态**每次弹出前现读注册表**，不缓存 —— 这样用 Shift 在应用里切了中英，
//! 打开菜单看到的就是最新的。

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, LoadIconW, LoadImageW, PostMessageW,
    SetForegroundWindow, TrackPopupMenu, HICON, IDI_APPLICATION, IMAGE_ICON, LR_LOADFROMFILE,
    MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, TPM_BOTTOMALIGN, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_APP, WM_NULL,
};

pub const WM_TRAY: u32 = WM_APP + 1;

pub const CMD_TOGGLE_MODE: u32 = 1;
pub const CMD_TOGGLE_PUNCT: u32 = 2;
pub const CMD_TOGGLE_AUTOSTART: u32 = 3;
pub const CMD_SETTINGS: u32 = 4;
pub const CMD_ABOUT: u32 = 5;
pub const CMD_QUIT: u32 = 6;

const ICON_ID: u32 = 1;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 优先用 exe 旁边的 `inklink.ico`（安装后一定在），开发时（`target\release`）没有这个文件，
/// 就退回系统默认应用图标 —— 否则 `hIcon` 为 null，`Shell_NotifyIconW` 会失败，托盘上什么都不出现。
fn load_icon() -> HICON {
    let dir = crate::state::exe_dir();
    let path = wide(&format!("{dir}\\inklink.ico"));
    let h = unsafe {
        LoadImageW(
            std::ptr::null_mut(),
            path.as_ptr(),
            IMAGE_ICON,
            0,
            0,
            LR_LOADFROMFILE,
        )
    };
    if !h.is_null() {
        return h as HICON;
    }
    unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) as HICON }
}

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut d: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    d.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    d.hWnd = hwnd;
    d.uID = ICON_ID;
    d
}

pub fn add(hwnd: HWND) -> bool {
    let mut d = data(hwnd);
    d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    d.uCallbackMessage = WM_TRAY;
    d.hIcon = load_icon();
    let tip: Vec<u16> = "inklink 输入法".encode_utf16().collect();
    d.szTip[..tip.len()].copy_from_slice(&tip);
    unsafe { Shell_NotifyIconW(NIM_ADD, &d) != 0 }
}

pub fn remove(hwnd: HWND) {
    let d = data(hwnd);
    unsafe {
        Shell_NotifyIconW(NIM_DELETE, &d);
    }
}

/// 弹出右键菜单，返回用户点的命令 ID（0 = 点外面关掉了）。
pub fn popup(hwnd: HWND, chinese: bool, punct: bool, autostart: bool) -> u32 {
    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return 0;
        }
        let mode = wide(if chinese { "中文" } else { "英文" });
        let punct_label = wide(if punct { "中文标点" } else { "半角标点" });
        let auto = wide("开机自启");

        AppendMenuW(menu, MF_STRING, CMD_TOGGLE_MODE as usize, mode.as_ptr());
        AppendMenuW(menu, MF_STRING, CMD_TOGGLE_PUNCT as usize, punct_label.as_ptr());
        AppendMenuW(
            menu,
            MF_STRING | if autostart { MF_CHECKED } else { 0 },
            CMD_TOGGLE_AUTOSTART as usize,
            auto.as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        let settings = wide("设置…（还没做）");
        AppendMenuW(
            menu,
            MF_STRING | MF_GRAYED,
            CMD_SETTINGS as usize,
            settings.as_ptr(),
        );
        let about = wide(&format!("inklink {}", env!("CARGO_PKG_VERSION")));
        AppendMenuW(
            menu,
            MF_STRING | MF_GRAYED,
            CMD_ABOUT as usize,
            about.as_ptr(),
        );
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        let quit = wide("退出浮条（不影响打字）");
        AppendMenuW(menu, MF_STRING, CMD_QUIT as usize, quit.as_ptr());

        let mut pt = std::mem::zeroed();
        GetCursorPos(&mut pt);
        // 先抢前台，否则点菜单外面菜单不会消失（托盘菜单的老规矩）
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            0,
            hwnd,
            std::ptr::null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
        cmd as u32
    }
}

