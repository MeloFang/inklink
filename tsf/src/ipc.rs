//! 与引擎进程通信 + 上屏。
//!
//! 铁律：这里**只做转发**，不含词库、不含切分逻辑。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING, ReadFile, WriteFile,
};

pub const PIPE_NAME: &str = r"\\.\pipe\inklink-engine";

/// 取当前光标（插入点）的屏幕坐标：返回 (x, 光标底边 y)。
///
/// 三级回退（现代应用如 WPS/Electron 常没有标准光标，GetGUIThreadInfo 会失败）：
///   1. GetGUIThreadInfo 的光标矩形
///   2. GetCaretPos + ClientToScreen
///   3. 鼠标位置（用户一般在点击处打字）
pub fn caret_screen_pos() -> Option<(i32, i32)> {
    unsafe {
        if let Some(p) = caret_via_gui_thread() {
            return Some(p);
        }
        if let Some(p) = caret_via_get_caret_pos() {
            return Some(p);
        }
        // 最后兜底：鼠标位置
        use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        Some((pt.x, pt.y))
    }
}

/// 方法 1：前台线程的光标矩形。
unsafe fn caret_via_gui_thread() -> Option<(i32, i32)> {
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, GUITHREADINFO,
    };
    let fg = GetForegroundWindow();
    if fg.is_null() {
        return None;
    }
    let tid = GetWindowThreadProcessId(fg, std::ptr::null_mut());
    if tid == 0 {
        return None;
    }
    let mut info: GUITHREADINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
    if GetGUIThreadInfo(tid, &mut info) == 0 || info.hwndCaret.is_null() {
        return None;
    }
    // rcCaret 全零 = 应用没有真正的光标（Electron/WPS 常见），视为无效
    let rc = info.rcCaret;
    if rc.left == 0 && rc.top == 0 && rc.right == 0 && rc.bottom == 0 {
        return None;
    }
    let mut pt = windows_sys::Win32::Foundation::POINT {
        x: rc.left,
        y: rc.bottom,
    };
    if ClientToScreen(info.hwndCaret, &mut pt) == 0 {
        return None;
    }
    Some((pt.x, pt.y))
}

/// 方法 2：GetCaretPos（相对于拥有焦点的窗口的客户区）。
unsafe fn caret_via_get_caret_pos() -> Option<(i32, i32)> {
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetFocus;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCaretPos;
    let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    if GetCaretPos(&mut pt) == 0 {
        return None;
    }
    // (0,0) 基本等于"没有光标"（Electron 类应用会这么返回），视为无效
    if pt.x == 0 && pt.y == 0 {
        return None;
    }
    let focus = GetFocus();
    if focus.is_null() || ClientToScreen(focus, &mut pt) == 0 {
        return None;
    }
    Some((pt.x, pt.y))
}

/// 引擎进程是否在跑（管道是否有可用实例）。
pub fn engine_alive() -> bool {
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
    let name = wide(PIPE_NAME);
    // 0 毫秒 = 立刻返回：有实例就 TRUE，没管道就 FALSE
    // SAFETY: 纯查询
    unsafe { WaitNamedPipeW(name.as_ptr(), 0) != 0 }
}

/// 确保引擎在跑：连不上就把它拉起来（同用户上下文，不阻塞）。
///
/// 这是"引擎崩了输入法就废"的根治手段——下次激活或按键时自动复活。
pub fn ensure_engine() {
    if engine_alive() {
        return;
    }
    let exe = engine_exe_path();
    if exe.is_empty() {
        return;
    }
    start_process(&exe);
}

/// 引擎可执行文件路径：注册表 > 默认安装位置。
fn engine_exe_path() -> String {
    use windows_sys::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    let key = wide("SOFTWARE\\inklink");
    let value = wide("EnginePath");
    let mut buf = [0u16; 512];
    let mut size = (buf.len() * 2) as u32;
    // SAFETY: 标准注册表读取，缓冲区大小按字节给出
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            &mut size,
        )
    };
    if rc == 0 {
        let len = (size as usize / 2).saturating_sub(1);
        let s = String::from_utf16_lossy(&buf[..len]);
        if !s.is_empty() {
            return s;
        }
    }
    // 回退到默认安装位置
    let fallback = r"C:\inklink-engine\inklink.exe";
    if std::path::Path::new(fallback).exists() {
        fallback.to_string()
    } else {
        String::new()
    }
}

/// 拉起引擎进程（不等待，创建后立刻关闭句柄）。
fn start_process(exe: &str) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW,
    };

    let mut cmd: Vec<u16> = format!("\"{exe}\" --serve")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut si: STARTUPINFOW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };

    // SAFETY: 标准 CreateProcessW；命令行可写、结构体已初始化
    unsafe {
        let ok = CreateProcessW(
            std::ptr::null(),
            cmd.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        );
        if ok != 0 {
            let _ = CloseHandle(pi.hProcess);
            let _ = CloseHandle(pi.hThread);
        }
    }
}

/// 向引擎查询候选。连不上就返回空（输入法不能让应用卡住）。
pub fn query(code: &str) -> Vec<String> {
    if code.is_empty() {
        return Vec::new();
    }
    // SAFETY: 句柄在本函数内配对关闭；缓冲区长度与传入长度一致
    unsafe {
        let name = wide(PIPE_NAME);
        let pipe: HANDLE = CreateFileW(
            name.as_ptr(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if pipe == INVALID_HANDLE_VALUE {
            // 连不上：顺手拉一下引擎（自愈），本次查询先返回空
            ensure_engine();
            return Vec::new();
        }

        let bytes = code.as_bytes();
        // 协议 v2：首字节是消息类型，1 = 查询
        let mut req = Vec::with_capacity(5 + bytes.len());
        req.push(1u8);
        req.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        req.extend_from_slice(bytes);
        write_all(pipe, &req);

        let mut len_buf = [0u8; 4];
        if !read_exact(pipe, &mut len_buf) {
            let _ = CloseHandle(pipe);
            return Vec::new();
        }
        let count = u32::from_le_bytes(len_buf) as usize;
        let mut out = Vec::with_capacity(count.min(64));
        for _ in 0..count {
            let mut l2 = [0u8; 2];
            if !read_exact(pipe, &mut l2) {
                break;
            }
            let len = u16::from_le_bytes(l2) as usize;
            let mut buf = vec![0u8; len];
            if !read_exact(pipe, &mut buf) {
                break;
            }
            if let Ok(s) = String::from_utf8(buf) {
                out.push(s);
            }
        }
        let _ = CloseHandle(pipe);
        out
    }
}

/// 选词上报：告诉引擎"用户选了第 index 个候选"，让它写进用户词库。
///
/// 失败无所谓（不影响上屏），所以不返回错误。
pub fn select(index: usize) {
    // SAFETY: 句柄在本函数内配对关闭
    unsafe {
        let name = wide(PIPE_NAME);
        let pipe: HANDLE = CreateFileW(
            name.as_ptr(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        );
        if pipe == INVALID_HANDLE_VALUE {
            return;
        }
        // 协议 v2：类型 2 = 选词，后跟 u32 下标
        let mut req = [0u8; 5];
        req[0] = 2;
        req[1..5].copy_from_slice(&(index as u32).to_le_bytes());
        write_all(pipe, &req);
        // 读掉响应（count=0），避免服务端阻塞在 flush
        let mut len_buf = [0u8; 4];
        let _ = read_exact(pipe, &mut len_buf);
        let _ = CloseHandle(pipe);
    }
}

/// 把文本送进当前获得焦点的应用（在光标处插入）。
/// 保留作回退：T4 之后主路径是 ITfRange::SetText，若某些应用不配合可回退到 SendInput。
#[allow(dead_code)]
pub fn send_text(text: &str) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, SendInput,
    };
    const MAX_CHUNK: usize = 64;

    let units: Vec<u16> = text.encode_utf16().collect();
    if units.is_empty() {
        return false;
    }
    let mut sent_all = true;

    for chunk in units.chunks(MAX_CHUNK) {
        let mut inputs: Vec<INPUT> = Vec::with_capacity(chunk.len() * 2);
        for &u in chunk {
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0,
                        wScan: u,
                        dwFlags: KEYEVENTF_UNICODE,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0,
                        wScan: u,
                        dwFlags: KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
        }
        // SAFETY: inputs 的布局与 SendInput 期望的一致
        let sent = unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
        if sent as usize != inputs.len() {
            sent_all = false;
            break;
        }
    }
    sent_all
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

unsafe fn read_exact(pipe: HANDLE, buf: &mut [u8]) -> bool {
    let mut read = 0u32;
    // SAFETY: 调用方保证 pipe 有效，buf 长度与实际可写区域一致
    let ok = unsafe {
        ReadFile(
            pipe,
            buf.as_mut_ptr(),
            buf.len() as u32,
            &mut read,
            std::ptr::null_mut(),
        )
    };
    ok != 0 && read as usize == buf.len()
}

unsafe fn write_all(pipe: HANDLE, buf: &[u8]) {
    let mut written = 0u32;
    let mut offset = 0usize;
    while offset < buf.len() {
        // SAFETY: 同上，pipe 有效、切片仍在缓冲区内
        let ok = unsafe {
            WriteFile(
                pipe,
                buf[offset..].as_ptr(),
                (buf.len() - offset) as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 || written == 0 {
            break;
        }
        offset += written as usize;
    }
}
