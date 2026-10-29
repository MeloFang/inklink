//! 浮条显隐通知。
//!
//! TSF 在我们这个输入法被**切到前台**时调 `Activate`/`ActivateEx`，切走时调 `Deactivate`。
//! 就在这两处通知常驻的 `inklink-ui.exe`，让它像搜狗那样「用的时候出现、不用的时候收起来」，
//! 而不是一直挂在屏幕右下角。
//!
//! 两条通道，缺一不可：
//!
//! 1. **窗口消息**（首选，零延迟）：单向、偶发、无返回值的通知，`PostMessageW` 是异步的，
//!    不会因为对端忙而卡住应用的 UI 线程（铁律 2：同步路径 50ms 封顶）。对端不在就什么都不做。
//! 2. **注册表序号**（兜底）：UIPI 会**静默丢弃**低完整性进程发给高完整性窗口的
//!    `WM_USER` 以上的消息。浮条通常由安装器以管理员身份启动，而打字的应用是普通权限，
//!    于是消息永远到不了 —— 表现就是"切到输入法了但浮条从来不出现"。
//!    所以这里再往 `HKCU\Software\inklink\UI` 写一个自增序号，浮条 400ms 轮询一次；
//!    注册表读写两边权限都够，不经过 UIPI。

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_APP};

/// 与 `ui/src/main.rs` 里的同名常量必须一致。
pub const WM_INKLINK_SHOW: u32 = WM_APP + 12;
pub const WM_INKLINK_HIDE: u32 = WM_APP + 13;

const UI_CLASS: &str = "inklink-ui-bar";
const UI_KEY: &str = "Software\\inklink\\UI";
const VALUE_SHOW: &str = "ShowSeq";
const VALUE_HIDE: &str = "HideSeq";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn post(msg: u32) {
    let class = wide(UI_CLASS);
    unsafe {
        let hwnd: HWND = FindWindowW(class.as_ptr(), std::ptr::null());
        if !hwnd.is_null() {
            let _ = PostMessageW(hwnd, msg, 0, 0);
        }
    }
}

/// 把 `name` 的值 +1（不存在就写 1）。失败就算了 —— 这是兜底通道，不是主路径。
fn bump(name: &str) {
    unsafe {
        let sub = wide(UI_KEY);
        let mut key: HKEY = std::ptr::null_mut();
        let rc = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            0,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            &mut key,
        );
        if rc != 0 {
            return;
        }
        let vname = wide(name);
        let mut cur: u32 = 0;
        let mut cb: u32 = 4;
        let mut ty: u32 = 0;
        let q = RegQueryValueExW(
            key,
            vname.as_ptr(),
            std::ptr::null(),
            &mut ty,
            &mut cur as *mut u32 as *mut u8,
            &mut cb,
        );
        if q != 0 || ty != REG_DWORD {
            cur = 0;
        }
        let next: u32 = cur.wrapping_add(1);
        let _ = RegSetValueExW(
            key,
            vname.as_ptr(),
            0,
            REG_DWORD,
            &next as *const u32 as *const u8,
            4,
        );
        let _ = RegCloseKey(key);
    }
}

/// 输入法被切到前台：让浮条出现。
pub fn show() {
    post(WM_INKLINK_SHOW);
    bump(VALUE_SHOW);
}

/// 输入法被切走：让浮条收起（浮条那边延迟一点，避免连续切换时闪）。
pub fn hide() {
    post(WM_INKLINK_HIDE);
    bump(VALUE_HIDE);
}
