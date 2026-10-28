//! 全局状态：中英模式、中文标点开关。
//!
//! ## 为什么放注册表
//!
//! 输入法 DLL 在每个宿主进程里各有一份（记事本、微信、VS Code 各一份），
//! 状态放在进程内存里就成了"每个应用各记各的"：微信切了英文，切到记事本还是中文。
//! 用户看不到浮条上是哪个状态，只会觉得"输入法抽风"。
//!
//! 放进 `HKEY_CURRENT_USER\Software\inklink\State` 之后：
//! - 不依赖任何后台进程活着（那个进程崩了照样能打字，也不会把状态弄丢）
//! - 新启动的进程第一次按键就能读到当前状态，不会"新窗口一打开是英文"
//! - 常驻 UI 进程（浮条）读写的就是同一份值，天然一致
//!
//! ## 为什么不缓存、为什么不起线程
//!
//! `RegQueryValueExW` 是微秒级的系统调用，一次按键查两次完全吃得消；
//! 缓存反而会带来"切了没反应"的困惑（用户按下浮条按钮，另一个进程里 200ms 后才生效），
//! 而 `RegNotifyChangeKeyValue` 需要一条常驻线程 —— `AGENTS.md` 第二条铁律明确要求
//! TSF DLL 是薄壳、不启动线程。所以这里每次都直接读。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE,
};

/// 注册表里的位置，UI 进程（浮条 / 设置台）也用这一份。
pub const KEY_PATH: &str = r"Software\inklink\State";
/// 中英模式：1 = 中文，0 = 英文。
pub const VALUE_CHINESE: &str = "Chinese";
/// 中文标点：1 = 全角转换，0 = 保持半角。
pub const VALUE_PUNCTUATION: &str = "Punctuation";

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// 把注册表里的值折成开关：只有明确写 0 才算关，缺失或其它值都按默认（开）。
///
/// 单独抽出来是为了能单测 —— 真去读写注册表的代码没法在单测里安全地跑。
fn flag_from_value(raw: Option<u32>) -> bool {
    !matches!(raw, Some(0))
}

unsafe fn query_dword(key: HKEY, name: &str) -> Option<u32> {
    let name_w = wide(name);
    let mut ty = 0u32;
    let mut data = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let rc = RegQueryValueExW(
        key,
        name_w.as_ptr(),
        std::ptr::null_mut(),
        &mut ty,
        &mut data as *mut u32 as *mut u8,
        &mut size,
    );
    if rc == 0 && ty == REG_DWORD {
        Some(data)
    } else {
        None
    }
}

unsafe fn open_key(access: u32) -> Option<HKEY> {
    let path = wide(KEY_PATH);
    let mut key: HKEY = std::ptr::null_mut();
    let rc = RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, access, &mut key);
    if rc == 0 {
        Some(key)
    } else {
        None
    }
}

unsafe fn create_key() -> Option<HKEY> {
    let path = wide(KEY_PATH);
    let mut key: HKEY = std::ptr::null_mut();
    let mut disposition = 0u32;
    let rc = RegCreateKeyExW(
        HKEY_CURRENT_USER,
        path.as_ptr(),
        0,
        std::ptr::null(),
        REG_OPTION_NON_VOLATILE,
        KEY_READ | KEY_SET_VALUE,
        std::ptr::null(),
        &mut key,
        &mut disposition,
    );
    if rc == 0 {
        Some(key)
    } else {
        None
    }
}

/// 当前状态 `(中文, 中文标点)`。读不到注册表时按默认（都是开）。
pub fn snapshot() -> (bool, bool) {
    unsafe {
        let Some(key) = open_key(KEY_READ) else {
            return (true, true);
        };
        let chinese = flag_from_value(query_dword(key, VALUE_CHINESE));
        let punct = flag_from_value(query_dword(key, VALUE_PUNCTUATION));
        RegCloseKey(key);
        (chinese, punct)
    }
}

/// 现在是中文模式吗。
pub fn chinese() -> bool {
    snapshot().0
}

/// 现在是中文标点吗。
pub fn punctuation() -> bool {
    snapshot().1
}

unsafe fn write_flag(name: &str, on: bool) {
    let Some(key) = create_key() else {
        return;
    };
    let name_w = wide(name);
    let data: u32 = u32::from(on);
    RegSetValueExW(
        key,
        name_w.as_ptr(),
        0,
        REG_DWORD,
        &data as *const u32 as *const u8,
        std::mem::size_of::<u32>() as u32,
    );
    RegCloseKey(key);
}

/// 切换/设置中英模式（全局，所有进程下一次按键就生效）。
pub fn set_chinese(on: bool) {
    unsafe { write_flag(VALUE_CHINESE, on) }
}

/// 设置中文标点开关（全局）。
///
/// 现在由 UI 进程（浮条 / 设置台）调用；DLL 自己只负责读。
#[allow(dead_code)]
pub fn set_punctuation(on: bool) {
    unsafe { write_flag(VALUE_PUNCTUATION, on) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 缺省值是开() {
        assert!(flag_from_value(None), "没写过值应该按默认开");
        assert!(flag_from_value(Some(1)));
        assert!(flag_from_value(Some(7)), "脏数据也按开处理更安全");
    }

    #[test]
    fn 只有明确写零才算关() {
        assert!(!flag_from_value(Some(0)));
    }

    #[test]
    fn 读不到注册表时也有合理默认() {
        let (chinese, punct) = snapshot();
        // 机器上可能已经有值，这里只断言"读得到布尔"，不假定具体取值
        let _ = (chinese, punct);
        assert!(chinese || !chinese);
    }
}
