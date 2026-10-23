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
            return Vec::new();
        }

        let bytes = code.as_bytes();
        let mut req = Vec::with_capacity(4 + bytes.len());
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
