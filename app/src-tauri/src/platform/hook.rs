//! 低级键盘钩子：面板可见期间采集按键，不抢焦点。
//!
//! **铁律**（见 `docs/adr/0005`）：回调里只做「原子读 + 判断 + 入队」，
//! 任何多余的工作都可能拖慢全系统输入。

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

/// 面板是否可见。由 `lib.rs` 在显隐时更新；钩子回调只读这一个原子量。
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// 仅测试用：允许处理注入事件。
///
/// 默认必须是 `false`——否则我们自己 `SendInput` 上屏的字符会被钩子重新吞掉。
/// 打开它只为了用模拟按键验证 UI 链路。
static ALLOW_INJECTED: AtomicBool = AtomicBool::new(false);

pub fn set_allow_injected(allow: bool) {
    ALLOW_INJECTED.store(allow, Ordering::Relaxed);
}

/// 按键出口。钩子线程往里塞，工作线程取出后发给前端。
static KEY_SINK: OnceLock<Sender<String>> = OnceLock::new();

/// 正在上屏：期间钩子必须放行，避免吞掉自己发出的字符。
static COMMITTING: AtomicBool = AtomicBool::new(false);

pub fn set_committing(v: bool) {
    COMMITTING.store(v, Ordering::Relaxed);
}

/// 是否正在拼写（决定退格/数字/标点要不要吞掉）。
static COMPOSING: AtomicBool = AtomicBool::new(false);

pub fn set_composing(v: bool) {
    COMPOSING.store(v, Ordering::Relaxed);
}

pub fn set_active(active: bool) {
    ACTIVE.store(active, Ordering::Relaxed);
}

/// 安装钩子并启动分发线程。返回是否安装成功。
#[cfg(windows)]
pub fn start(sink: Sender<String>) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetMessageW, SetWindowsHookExW, WH_KEYBOARD_LL,
    };

    let _ = KEY_SINK.set(sink);

    let (tx, rx) = std::sync::mpsc::channel::<bool>();
    std::thread::spawn(move || {
        // SAFETY: 安装全局低级键盘钩子；模块句柄传 0 表示当前进程
        let hook =
            unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), std::ptr::null_mut(), 0) };
        let ok = !hook.is_null();
        let _ = tx.send(ok);
        if !ok {
            return;
        }
        // 低级钩子要求安装线程有消息循环
        // SAFETY: MSG 是 POD，零初始化合法
        let mut msg: windows_sys::Win32::UI::WindowsAndMessaging::MSG =
            unsafe { std::mem::zeroed() };
        loop {
            // SAFETY: 标准 GetMessage 循环
            let ret = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
            if ret <= 0 {
                break;
            }
        }
    });

    rx.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap_or(false)
}

#[cfg(not(windows))]
pub fn start(_sink: Sender<String>) -> bool {
    false
}

/// 把虚拟键码翻译成前端认识的名字；不处理的键返回 `None`。
#[cfg(windows)]
fn map_key(vk: u32) -> Option<String> {
    const VK_BACK: u32 = 0x08;
    const VK_RETURN: u32 = 0x0D;
    const VK_ESCAPE: u32 = 0x1B;
    const VK_SPACE: u32 = 0x20;
    const VK_LEFT: u32 = 0x25;
    const VK_UP: u32 = 0x26;
    const VK_RIGHT: u32 = 0x27;
    const VK_DOWN: u32 = 0x28;
    const VK_0: u32 = 0x30;
    const VK_9: u32 = 0x39;
    const VK_A: u32 = 0x41;
    const VK_Z: u32 = 0x5A;
    const VK_OEM_MINUS: u32 = 0xBD; // -  （部分键盘是 0x6D 小键盘）
    const VK_OEM_PLUS: u32 = 0xBB; // =
    const VK_OEM_COMMA: u32 = 0xBC; // ,
    const VK_OEM_PERIOD: u32 = 0xBE; // .
    const VK_OEM_7: u32 = 0xDE; // 单引号/撇号

    match vk {
        VK_BACK => Some("Backspace".into()),
        VK_RETURN => Some("Enter".into()),
        VK_ESCAPE => Some("Escape".into()),
        VK_SPACE => Some(" ".into()),
        VK_UP => Some("ArrowUp".into()),
        VK_DOWN => Some("ArrowDown".into()),
        VK_LEFT => Some("ArrowLeft".into()),
        VK_RIGHT => Some("ArrowRight".into()),
        VK_OEM_MINUS => Some("-".into()),
        VK_OEM_PLUS => Some("=".into()),
        VK_OEM_COMMA => Some(",".into()),
        VK_OEM_PERIOD => Some(".".into()),
        VK_OEM_7 => Some("'".into()),
        VK_A..=VK_Z => Some((vk as u8 as char).to_ascii_lowercase().to_string()),
        VK_0..=VK_9 => Some((vk as u8 as char).to_string()),
        _ => None,
    }
}

#[cfg(windows)]
unsafe extern "system" fn hook_proc(code: i32, wparam: usize, lparam: isize) -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, KBDLLHOOKSTRUCT, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    // SAFETY: 钩子回调契约：code < 0 必须直接透传
    if code < 0 || lparam == 0 {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    }
    // 面板没显示：一次原子读就返回，不做任何事
    if !ACTIVE.load(Ordering::Relaxed) || COMMITTING.load(Ordering::Relaxed) {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    }

    let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
    // 自己 SendInput 出来的事件必须放行，否则上屏会被吞
    const LLKHF_INJECTED: u32 = 0x10;
    if kb.flags & LLKHF_INJECTED != 0 && !ALLOW_INJECTED.load(Ordering::Relaxed) {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    }

    // 带修饰键的组合键一律放行（否则 Ctrl+` 热键会被自己吃掉）
    if modifier_down() {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    }

    let Some(key) = map_key(kb.vkCode) else {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    };

    // 只有「正在拼写」时才吞这些键：
    //   - 没在拼写时，退格/空格/回车/数字/标点都应该照常作用于文档
    //   - 字母永远吞（用来启动拼写）
    if !should_swallow(kb.vkCode) {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    }

    // 只处理按下事件；抬起事件也吞掉，避免目标窗口收到半截按键
    if wparam == WM_KEYDOWN as usize || wparam == WM_SYSKEYDOWN as usize {
        if let Some(sink) = KEY_SINK.get() {
            // 非阻塞：满了就丢，绝不在这里等
            let _ = sink.send(key);
        }
    } else if wparam != WM_KEYUP as usize && wparam != WM_SYSKEYUP as usize {
        return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
    }

    1 // 吞掉
}

/// 这个键该不该被面板吞掉。
#[cfg(windows)]
fn should_swallow(vk: u32) -> bool {
    const VK_BACK: u32 = 0x08;
    const VK_RETURN: u32 = 0x0D;
    const VK_ESCAPE: u32 = 0x1B;
    const VK_SPACE: u32 = 0x20;
    const VK_LEFT: u32 = 0x25;
    const VK_UP: u32 = 0x26;
    const VK_RIGHT: u32 = 0x27;
    const VK_DOWN: u32 = 0x28;
    const VK_0: u32 = 0x30;
    const VK_9: u32 = 0x39;
    const VK_A: u32 = 0x41;
    const VK_Z: u32 = 0x5A;
    const VK_OEM_MINUS: u32 = 0xBD;
    const VK_OEM_PLUS: u32 = 0xBB;
    const VK_OEM_COMMA: u32 = 0xBC;
    const VK_OEM_PERIOD: u32 = 0xBE;
    const VK_OEM_7: u32 = 0xDE;

    let composing = COMPOSING.load(Ordering::Relaxed);

    match vk {
        // 字母：正常吃（用来拼写）；但大写锁定 / Shift 按下时放行——那是要打英文
        VK_A..=VK_Z => !caps_or_shift(),
        // Esc：永远吃掉，用来收起面板
        VK_ESCAPE => true,
        // 以下只在拼写中才吃
        VK_BACK | VK_SPACE | VK_LEFT | VK_UP | VK_RIGHT | VK_DOWN | VK_OEM_MINUS | VK_OEM_PLUS
        | VK_OEM_COMMA | VK_OEM_PERIOD | VK_OEM_7 => composing,
        // 回车：拼写中吃（上屏），否则放行（文档里就是换行）
        VK_RETURN => composing,
        // 数字：拼写中有候选时用来选词；没在拼写就放行
        VK_0..=VK_9 => composing,
        _ => false,
    }
}

/// 大写锁定开着，或者按住 Shift —— 此时用户要打的是英文大写，不该被输入法吞掉。
#[cfg(windows)]
fn caps_or_shift() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    const VK_SHIFT: i32 = 0x10;
    const VK_CAPITAL: i32 = 0x14;
    // SAFETY: 纯查询
    unsafe {
        let shift = (GetAsyncKeyState(VK_SHIFT) as u16 & 0x8000) != 0;
        let caps = (GetAsyncKeyState(VK_CAPITAL) as u16 & 0x0001) != 0;
        shift || caps
    }
}

#[cfg(windows)]
fn modifier_down() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    const VK_CONTROL: i32 = 0x11;
    const VK_MENU: i32 = 0x12;
    const VK_LWIN: i32 = 0x5B;
    const VK_RWIN: i32 = 0x5C;
    // SAFETY: 纯查询
    unsafe {
        [VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
            .iter()
            .any(|vk| (GetAsyncKeyState(*vk) as u16 & 0x8000) != 0)
    }
}
