//! 托盘图标 + 右键菜单。
//!
//! 菜单里的勾选状态**每次弹出前现读注册表**，不缓存 —— 这样用 Shift 在应用里切了中英，
//! 打开菜单看到的就是最新的。

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    LoadIconW, LoadImageW, HICON, IDI_APPLICATION, IMAGE_ICON, LR_LOADFROMFILE, WM_APP,
};

pub const WM_TRAY: u32 = WM_APP + 1;


const ICON_ID: u32 = 1;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 优先用 exe 旁边的 `inklink.ico`（安装后一定在），开发时（`target\release`）没有这个文件，
/// 就退回系统默认应用图标 —— 否则 `hIcon` 为 null，`Shell_NotifyIconW` 会失败，托盘上什么都不出现。
pub fn load_icon() -> HICON {
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
