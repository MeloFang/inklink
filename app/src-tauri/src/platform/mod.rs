//! 平台层：窗口句柄、上屏（SendInput）、键盘钩子。
//!
//! Windows 专有 API 集中在这里，便于将来隔离与替换。

pub mod hook;
/// 加上 `WS_EX_NOACTIVATE`：显示时不夺取焦点，目标应用保持前台（光标不消失）。
#[cfg(windows)]
pub fn make_no_activate(hwnd: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_NOACTIVATE,
    };
    if hwnd == 0 {
        return;
    }
    // SAFETY: 只改本进程窗口的扩展样式
    unsafe {
        let ex = GetWindowLongPtrW(hwnd as _, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd as _, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE as isize);
    }
}

#[cfg(not(windows))]
pub fn make_no_activate(_hwnd: isize) {}
/// 取当前前台窗口句柄（0 表示没有）。
pub fn foreground_window() -> isize {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        // SAFETY: 纯查询，无副作用
        unsafe { GetForegroundWindow() as isize }
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// 把焦点还给目标窗口。
///
/// 面板刚隐藏时前台还挂在面板上，直接 SendInput 会打到空气里，所以先切回来。
/// `SetForegroundWindow` 在前后台进程不同时可能被系统拒绝，这里用
/// `AttachThreadInput` 临时共享输入队列绕开限制。
pub fn focus_window(hwnd: isize) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowThreadProcessId, IsWindow, SetForegroundWindow,
        };

        if hwnd == 0 {
            return;
        }
        // SAFETY: 句柄来自 GetForegroundWindow，Use after destroy 时 IsWindow 会挡掉
        unsafe {
            if IsWindow(hwnd as _) == 0 {
                return;
            }
            let target_thread = GetWindowThreadProcessId(hwnd as _, std::ptr::null_mut());
            let current_thread = GetCurrentThreadId();
            let attached =
                target_thread != 0 && AttachThreadInput(current_thread, target_thread, 1) != 0;
            SetForegroundWindow(hwnd as _);
            if attached {
                AttachThreadInput(current_thread, target_thread, 0);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
    }
}

/// 用 `SendInput` + `KEYEVENTF_UNICODE` 直接发字符。
///
/// 返回是否全部发送成功。失败时调用方应回退到剪贴板方案。
pub fn send_unicode(text: &str) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
            SendInput,
        };

        let units: Vec<u16> = text.encode_utf16().collect();
        if units.is_empty() {
            return true;
        }

        let mut inputs: Vec<INPUT> = Vec::with_capacity(units.len() * 2);
        for unit in units {
            for flags in [KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
                inputs.push(INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: 0,
                            wScan: unit,
                            dwFlags: flags,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                });
            }
        }

        let sent = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_mut_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        sent == inputs.len() as u32
    }
    #[cfg(not(windows))]
    {
        let _ = text;
        false
    }
}

/// 发一次 Ctrl+V。
pub fn send_paste() -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
        };

        const VK_CONTROL: u16 = 0x11;
        const VK_V: u16 = 0x56;

        let mut inputs: Vec<INPUT> = Vec::with_capacity(4);
        let mut push = |vk: u16, up: bool| {
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: vk,
                        wScan: 0,
                        dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
        };
        push(VK_CONTROL, false);
        push(VK_V, false);
        push(VK_V, true);
        push(VK_CONTROL, true);

        let sent = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_mut_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        sent == inputs.len() as u32
    }
    #[cfg(not(windows))]
    {
        false
    }
}
