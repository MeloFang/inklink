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
    // 单例：DLL 可能被多个应用同时拉起引擎，用命名互斥量保证只有一个实例在服务。
    if !claim_singleton() {
        eprintln!("已有引擎实例在运行，本实例退出");
        std::process::exit(0);
    }

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

/// 用命名互斥量抢占单例。成功返回 true；已有实例返回 false。
///
/// 互斥量故意不释放——进程退出时由系统回收，正好表达"我在运行"。
fn claim_singleton() -> bool {
    use windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS;
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = std::ffi::OsStr::new("Global\\inklink-engine-singleton")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: 标准 CreateMutex 调用；句柄故意不关闭（进程生命周期内有效）
    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if handle.is_null() {
            // 创建失败就别卡住，放行（最坏情况是多个实例，仍能服务）
            return true;
        }
        windows_sys::Win32::Foundation::GetLastError() != ERROR_ALREADY_EXISTS
    }
}

fn serve_once(ranker: &Ranker) -> std::io::Result<()> {
    // SAFETY: 标准 Win32 管道调用，句柄在本函数内配对关闭
    unsafe {
        let name: Vec<u16> = std::ffi::OsStr::new(PIPE_NAME)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        // NULL DACL：允许任何用户连接。引擎可能以别的账户运行，而 TSF DLL
        // 跑在每个应用（各种用户）里，默认 DACL 只放行创建者自己 → 跨用户连不上。
        let (sa, _sd) = everyone_security_attributes();

        let pipe = CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            8192,
            8192,
            0,
            &sa,
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

/// 构造一个带 NULL DACL（所有人可访问）的 SECURITY_ATTRIBUTES。
///
/// 描述符用 `Box` 放在堆上：返回的 `SECURITY_ATTRIBUTES.lpSecurityDescriptor`
/// 指向它，只要 Box 活着地址就稳定。**不能**指向栈上局部变量——move 后地址会失效。
fn everyone_security_attributes(
) -> (windows_sys::Win32::Security::SECURITY_ATTRIBUTES, Box<windows_sys::Win32::Security::SECURITY_DESCRIPTOR>) {
    use windows_sys::Win32::Security::{
        InitializeSecurityDescriptor, SetSecurityDescriptorDacl, SECURITY_ATTRIBUTES,
        SECURITY_DESCRIPTOR,
    };

    let mut sd = Box::new(unsafe { std::mem::zeroed::<SECURITY_DESCRIPTOR>() });
    // SAFETY: sd 是零初始化的有效缓冲区，Box 保证地址稳定
    unsafe {
        InitializeSecurityDescriptor(sd.as_mut() as *mut _ as *mut core::ffi::c_void, 1);
        // bDaclPresent=TRUE, pDacl=NULL → 空 DACL = 所有人放行
        SetSecurityDescriptorDacl(
            sd.as_mut() as *mut _ as *mut core::ffi::c_void,
            1,
            std::ptr::null(),
            0,
        );
    }

    let mut sa: SECURITY_ATTRIBUTES = unsafe { std::mem::zeroed() };
    sa.nLength = std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32;
    sa.lpSecurityDescriptor = sd.as_mut() as *mut _ as *mut core::ffi::c_void;
    sa.bInheritHandle = 0;
    (sa, sd)
}

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
