//! 与引擎进程通信 + 上屏。
//!
//! 铁律：这里**只做转发**，不含词库、不含切分逻辑。

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_FILE_NOT_FOUND, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING, ReadFile, WriteFile,
};
use windows_sys::Win32::System::Pipes::{PeekNamedPipe, WaitNamedPipeW};

pub const PIPE_NAME: &str = r"\\.\pipe\inklink-engine";

/// 查询的超时预算（毫秒）。正常情况下引擎 1–30ms 就回；这个数只是保险丝。
///
/// 为什么必须有：以前这里是裸 `ReadFile`，没有任何超时。一旦引擎和 DLL 的
/// 协议对不上（或者引擎卡住），**调用方应用的 UI 线程就永久挂起**——
/// 2026-10-27 微信就是这么被系统当"未响应"关掉的。
const QUERY_TIMEOUT_MS: u32 = 150;
/// 选词上报的超时预算：它只是"记账"，丢了也只影响下次排序，绝不能拖住用户。
const SELECT_TIMEOUT_MS: u32 = 100;

/// 连上引擎；`timeout_ms` 内没有可用实例就返回 `None`。
///
/// 只有"管道根本不存在"（引擎没跑）才顺手拉起引擎；实例都忙属于正常现象，
/// 直接放弃本次请求，不要阻塞。
unsafe fn open_pipe(timeout_ms: u32) -> Option<HANDLE> {
    let name = wide(PIPE_NAME);
    // SAFETY: 纯查询，name 以 0 结尾
    if unsafe { WaitNamedPipeW(name.as_ptr(), timeout_ms) } == 0 {
        let err = unsafe { GetLastError() };
        if err == ERROR_FILE_NOT_FOUND {
            ensure_engine();
        }
        return None;
    }
    // SAFETY: 句柄由调用方配对关闭
    let pipe = unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if pipe == INVALID_HANDLE_VALUE {
        return None;
    }
    Some(pipe)
}

/// 带超时的读满：数据没到就返回 `false`，绝不无限等。
///
/// 用 `PeekNamedPipe` 先看有没有字节，有才 `ReadFile`（此时不会阻塞）。
unsafe fn read_exact_timeout(pipe: HANDLE, buf: &mut [u8], timeout_ms: u32) -> bool {
    let mut got = 0usize;
    let start = Instant::now();
    while got < buf.len() {
        let mut avail: u32 = 0;
        // SAFETY: pipe 有效；只查可用字节数
        let ok = unsafe { PeekNamedPipe(pipe, std::ptr::null_mut(), 0, std::ptr::null_mut(), &mut avail, std::ptr::null_mut()) };
        if ok == 0 {
            return false;
        }
        if avail > 0 {
            let want = ((buf.len() - got) as u32).min(avail);
            let mut read: u32 = 0;
            // SAFETY: 上面已确认至少有 want 字节可读
            let ok = unsafe {
                ReadFile(
                    pipe,
                    buf[got..].as_mut_ptr(),
                    want,
                    &mut read,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 || read == 0 {
                return false;
            }
            got += read as usize;
            continue;
        }
        if start.elapsed() >= Duration::from_millis(timeout_ms as u64) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    true
}

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

/// 向引擎查询候选。连不上 / 超时都返回空（输入法绝不能让应用卡住）。
pub fn query(code: &str) -> Vec<String> {
    if code.is_empty() {
        return Vec::new();
    }
    // SAFETY: 句柄在本函数内配对关闭；读操作全部带超时
    unsafe {
        let Some(pipe) = open_pipe(QUERY_TIMEOUT_MS) else {
            return Vec::new();
        };

        let bytes = code.as_bytes();
        // 协议 v2：首字节是消息类型，1 = 查询
        let mut req = Vec::with_capacity(5 + bytes.len());
        req.push(1u8);
        req.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        req.extend_from_slice(bytes);
        write_all(pipe, &req);

        let mut len_buf = [0u8; 4];
        if !read_exact_timeout(pipe, &mut len_buf, QUERY_TIMEOUT_MS) {
            let _ = CloseHandle(pipe);
            return Vec::new();
        }
        let count = u32::from_le_bytes(len_buf) as usize;
        let mut out = Vec::with_capacity(count.min(64));
        for _ in 0..count {
            let mut l2 = [0u8; 2];
            if !read_exact_timeout(pipe, &mut l2, QUERY_TIMEOUT_MS) {
                break;
            }
            let len = u16::from_le_bytes(l2) as usize;
            let mut buf = vec![0u8; len];
            if !read_exact_timeout(pipe, &mut buf, QUERY_TIMEOUT_MS) {
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

/// 选词上报：告诉引擎"用户在 `code` 这个拼音下选了 `word`"，让它记进用户词库。
///
/// 为什么把 code 和 word 都传过去：引擎那头没法知道用户选的是哪个词 ——
/// 下标只有配上"当时查询的是什么"才有意义，而管道每个请求一个连接，
/// 引擎跨连接记"上一次查询"在多应用并发时会串味。
///
/// 失败无所谓（不影响上屏），所以不返回错误。
pub fn select(index: usize, code: &str, word: &str) {
    if word.is_empty() {
        return;
    }
    // SAFETY: 句柄在本函数内配对关闭；读操作带超时
    unsafe {
        let Some(pipe) = open_pipe(SELECT_TIMEOUT_MS) else {
            return;
        };
        // 协议 v3：类型 2 = 选词
        //   [u8 type][u32 index][u32 code_len][code][u16 word_len][word]
        let code_bytes = code.as_bytes();
        let word_bytes = word.as_bytes();
        let mut req = Vec::with_capacity(11 + code_bytes.len() + word_bytes.len());
        req.push(2u8);
        req.extend_from_slice(&(index as u32).to_le_bytes());
        req.extend_from_slice(&(code_bytes.len() as u32).to_le_bytes());
        req.extend_from_slice(code_bytes);
        req.extend_from_slice(&(word_bytes.len() as u16).to_le_bytes());
        req.extend_from_slice(word_bytes);
        write_all(pipe, &req);
        // 读掉响应（count=0），避免服务端阻塞在 flush
        let mut len_buf = [0u8; 4];
        let _ = read_exact_timeout(pipe, &mut len_buf, SELECT_TIMEOUT_MS);
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
