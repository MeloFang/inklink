//! 全局状态的读写 + 开机自启 + 浮条位置记忆。
//!
//! **唯一真相源是注册表**（见 `docs/adr/0007-加常驻UI进程与全局状态.md`）：
//! DLL 每次按键现读，本进程也只是读写它 —— 所以浮条被关掉/崩掉，输入法照常工作。
//!
//! ```text
//! HKCU\Software\inklink\State
//!   Chinese     DWORD  1 中文 / 0 英文
//!   Punctuation DWORD  1 中文标点 / 0 半角标点
//! HKCU\Software\inklink\UI
//!   PosX, PosY  DWORD  浮条左上角（屏幕坐标），没有就用默认右下角
//! HKCU\Software\Microsoft\Windows\CurrentVersion\Run
//!   inklink-ui  REG_SZ 开机自启
//! ```

use std::ffi::c_void;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegGetValueW, RegSetValueExW,
    HKEY_CURRENT_USER, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_DWORD,
};

pub const STATE_KEY: &str = "Software\\inklink\\State";
pub const UI_KEY: &str = "Software\\inklink\\UI";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_NAME: &str = "inklink-ui";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn read_dword(sub: &str, name: &str) -> Option<u32> {
    let sub = wide(sub);
    let name = wide(name);
    let mut buf: u32 = 0;
    let mut len = std::mem::size_of::<u32>() as u32;
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            name.as_ptr(),
            RRF_RT_DWORD,
            std::ptr::null_mut(),
            &mut buf as *mut u32 as *mut c_void,
            &mut len,
        )
    };
    if rc == 0 {
        Some(buf)
    } else {
        None
    }
}

/// 写一个 DWORD。返回 Win32 错误码（0 = 成功）。
///
/// 注意**必须**用 `RegCreateKeyExW` 而不是 `RegSetKeyValueW`：后者要求子键已存在，
/// 新机器上第一次写（比如刚装完就按 Shift 切英文）会直接失败。
pub fn write_dword(sub: &str, name: &str, v: u32) -> u32 {
    let sub = wide(sub);
    let name = wide(name);
    unsafe {
        let mut key: windows_sys::Win32::System::Registry::HKEY = std::ptr::null_mut();
        let rc = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        );
        if rc != 0 {
            return rc;
        }
        let rc = RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_DWORD,
            &v as *const u32 as *const u8,
            std::mem::size_of::<u32>() as u32,
        );
        RegCloseKey(key);
        rc
    }
}

/// 缺失和脏值都按「开」处理，只有明确写 0 才算关 —— 和 DLL 侧 `state.rs` 同一规则。
pub fn read_flag(name: &str) -> bool {
    read_dword(STATE_KEY, name) != Some(0)
}

/// 读 DLL 写的显隐序号（`HKCU\Software\inklink\UI`）。读不到就当 0。
pub fn read_seq(name: &str) -> i32 {
    match read_dword(UI_KEY, name) {
        Some(v) => v as i32,
        None => 0,
    }
}

pub fn write_flag(name: &str, on: bool) -> u32 {
    write_dword(STATE_KEY, name, if on { 1 } else { 0 })
}

pub fn read_pos() -> Option<(i32, i32)> {
    let x = read_dword(UI_KEY, "PosX")? as i32;
    let y = read_dword(UI_KEY, "PosY")? as i32;
    // 屏幕换过、分辨率变小的时候旧坐标可能是负的或飞出屏幕，直接当没存过
    if x < -10000 || y < -10000 || x > 10000 || y > 10000 {
        return None;
    }
    Some((x, y))
}

pub fn write_pos(x: i32, y: i32) {
    write_dword(UI_KEY, "PosX", x as u32);
    write_dword(UI_KEY, "PosY", y as u32);
}

pub fn autostart_enabled() -> bool {
    let run = wide(RUN_KEY);
    let name = wide(RUN_NAME);
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            run.as_ptr(),
            name.as_ptr(),
            0x0000_0002, // RRF_RT_REG_SZ
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) == 0
    }
}

pub fn set_autostart(on: bool) {
    let run = wide(RUN_KEY);
    let name = wide(RUN_NAME);
    unsafe {
        if on {
            let exe = std::env::current_exe()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            if exe.is_empty() {
                return;
            }
            let quoted = format!("\"{exe}\"");
            let data = wide(&quoted);
            let mut key: windows_sys::Win32::System::Registry::HKEY = std::ptr::null_mut();
            let rc = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                run.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                std::ptr::null(),
                &mut key,
                std::ptr::null_mut(),
            );
            if rc == 0 {
                RegSetValueExW(
                    key,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    data.as_ptr() as *const u8,
                    (data.len() * 2) as u32,
                );
                RegCloseKey(key);
            }
        } else {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, run.as_ptr(), name.as_ptr());
        }
    }
}

/// exe 所在目录（`C:\Program Files\inklink\`），用来找 `inklink.ico`。
pub fn exe_dir() -> String {
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// 单实例：已经有一个浮条在跑就别再开第二个（否则托盘会出现两个图标）。
///
/// 两道闸，缺一不可：
///   1. **窗口类**：屏幕上已经有 `inklink-ui-bar` 就说明有实例在跑。这一条不依赖
///      任何内核对象权限，受限令牌下也照样有效。
///   2. **互斥体**：兜住"上一个实例刚创建、窗口还没建出来"这种窄窗口。
///
/// 只用互斥体是不够的：`CreateMutexW` 在受限令牌下会返回 NULL（ERROR_ACCESS_DENIED），
/// 那时旧实现返回 false（"没在跑"），于是每启动一次就多一个浮条 —— 实测见过 5 个同时跑。
pub fn already_running() -> bool {
    use windows_sys::Win32::System::Threading::CreateMutexW;
    use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;
    const ERROR_ALREADY_EXISTS: u32 = 183;
    unsafe {
        let class = wide("inklink-ui-bar");
        if !FindWindowW(class.as_ptr(), std::ptr::null()).is_null() {
            return true;
        }
        let name = wide("Local\\inklink-ui-singleton");
        let h = CreateMutexW(std::ptr::null_mut(), 0, name.as_ptr());
        if h.is_null() {
            // 建不出互斥体（受限令牌）—— 窗口那一道闸已经查过了，放行
            return false;
        }
        windows_sys::Win32::Foundation::GetLastError() == ERROR_ALREADY_EXISTS
    }
}
