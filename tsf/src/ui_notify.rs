//! 浮条显隐通知。
//!
//! TSF 在我们这个输入法被**切到前台**时调 `Activate`/`ActivateEx`，切走时调 `Deactivate`。
//! 就在这两处给常驻的 `inklink-ui.exe` 发一条窗口消息，让它像搜狗那样「用的时候出现、
//! 不用的时候收起来」，而不是一直挂在屏幕右下角。
//!
//! 为什么用窗口消息而不是管道：只需要单向、偶发、无返回值的通知，`PostMessageW` 是异步的，
//! 不会因为对端忙而卡住应用的 UI 线程（铁律 2：同步路径 50ms 封顶）。
//! 对端不在（用户退出了浮条）就什么都不做 —— 浮条只是入口，不影响打字。

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_APP};

/// 与 `ui/src/main.rs` 里的同名常量必须一致。
pub const WM_INKLINK_SHOW: u32 = WM_APP + 12;
pub const WM_INKLINK_HIDE: u32 = WM_APP + 13;

const UI_CLASS: &str = "inklink-ui-bar";

fn post(msg: u32) {
    let class: Vec<u16> = UI_CLASS.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let hwnd: HWND = FindWindowW(class.as_ptr(), std::ptr::null());
        if !hwnd.is_null() {
            let _ = PostMessageW(hwnd, msg, 0, 0);
        }
    }
}

/// 输入法被切到前台：让浮条出现。
pub fn show() {
    post(WM_INKLINK_SHOW);
}

/// 输入法被切走：让浮条收起（浮条那边延迟一点，避免连续切换时闪）。
pub fn hide() {
    post(WM_INKLINK_HIDE);
}
