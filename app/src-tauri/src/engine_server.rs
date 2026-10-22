//! 引擎服务模式：`inklink.exe --serve` 时运行。
//!
//! TSF DLL 通过命名管道查询候选，**引擎和词库始终留在本进程**，绝不进宿主应用。
//!
//! 协议（都是小端）：
//! ```text
//! 请求： [u32 长度 N][N 字节 UTF-8 拼音]
//! 响应： [u32 候选数 C] 然后 C 次 [u16 字节长度 L][L 字节 UTF-8 词]
//! ```
//! 一次连接处理一次请求，然后断开——实现简单，延迟约 1ms，远低于 30ms 预算。

use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{FlushFileBuffers, ReadFile, WriteFile};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

use crate::engine::lexicon::Lexicon;
use crate::engine::ranker::Ranker;

pub const PIPE_NAME: &str = r"\\.\pipe\inklink-engine";

/// 引擎服务主循环：加载词库后一直服务，直到进程被杀。
pub fn run(lexicon_path: &std::path::Path) -> ! {
    let lex = match Lexicon::load(lexicon_path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("词库加载失败 {}: {e}", lexicon_path.display());
            std::process::exit(2);
        }
    };
    println!("引擎就绪: {} 条词", lex.len());
    let ranker = Ranker::new(lex);

    loop {
        if let Err(e) = serve_once(&ranker) {
            eprintln!("管道错误: {e}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

fn serve_once(ranker: &Ranker) -> std::io::Result<()> {
    // SAFETY: 标准 Win32 管道调用，句柄在本函数内配对关闭
    unsafe {
        let name: Vec<u16> = std::ffi::OsStr::new(PIPE_NAME)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let pipe = CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            8192,
            8192,
            0,
            std::ptr::null_mut(),
        );
        if pipe == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }

        // 等客户端接入
        if ConnectNamedPipe(pipe, std::ptr::null_mut()) == 0 {
            let err = GetLastError();
            if err != ERROR_PIPE_CONNECTED {
                let _ = CloseHandle(pipe);
                return Err(std::io::Error::last_os_error());
            }
        }

        let result = handle_client(pipe, ranker);

        let _ = FlushFileBuffers(pipe);
        let _ = DisconnectNamedPipe(pipe);
        let _ = CloseHandle(pipe);
        result
    }
}

const PIPE_ACCESS_DUPLEX: u32 = 0x0000_0003;

unsafe fn handle_client(pipe: HANDLE, ranker: &Ranker) -> std::io::Result<()> {
    // 读请求长度
    let mut len_buf = [0u8; 4];
    if !read_exact(pipe, &mut len_buf) {
        return Ok(()); // 客户端没发东西，正常退出
    }
    let n = u32::from_le_bytes(len_buf) as usize;
    if n == 0 || n > 4096 {
        return Ok(());
    }
    let mut payload = vec![0u8; n];
    if !read_exact(pipe, &mut payload) {
        return Ok(());
    }
    let code = String::from_utf8_lossy(&payload).to_string();

    let cands = ranker.candidates(&code, 50);

    // 组装响应
    let mut out = Vec::with_capacity(4 + cands.len() * 16);
    out.extend_from_slice(&(cands.len() as u32).to_le_bytes());
    for c in &cands {
        let bytes = c.text.as_bytes();
        let l = bytes.len().min(u16::MAX as usize) as u16;
        out.extend_from_slice(&l.to_le_bytes());
        out.extend_from_slice(&bytes[..l as usize]);
    }
    write_all(pipe, &out);
    Ok(())
}

unsafe fn read_exact(pipe: HANDLE, buf: &mut [u8]) -> bool {
    let mut read = 0u32;
    let ok = ReadFile(
        pipe,
        buf.as_mut_ptr(),
        buf.len() as u32,
        &mut read,
        std::ptr::null_mut(),
    );
    ok != 0 && read as usize == buf.len()
}

unsafe fn write_all(pipe: HANDLE, buf: &[u8]) {
    let mut written = 0u32;
    let mut offset = 0usize;
    while offset < buf.len() {
        let ok = WriteFile(
            pipe,
            buf[offset..].as_ptr(),
            (buf.len() - offset) as u32,
            &mut written,
            std::ptr::null_mut(),
        );
        if ok == 0 || written == 0 {
            break;
        }
        offset += written as usize;
    }
}

#[allow(dead_code)]
fn is_broken_pipe() -> bool {
    unsafe { GetLastError() == ERROR_BROKEN_PIPE }
}
