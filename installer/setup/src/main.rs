//! inklink 的单文件安装器。
//!
//! 为什么要有这个 exe：`SignTool` 只能给 PE 签名。原来的安装方式是
//! `install.cmd` + `install.ps1` 两个脚本，脚本签不了名（就算签了，用户
//! 双击的也是 .cmd），所以想让"用户下载的这一个文件是签过名的"这件事成立，
//! 就得把「解包 + 写注册表 + 起引擎」全部搬进一个 exe。
//!
//! 载荷是追加在 exe 末尾的自定义档案（由 `tools\release.ps1` 生成）：
//!
//! ```text
//! 压缩后的一坨: deflate( [u32 名字长度][名字 utf8][u64 数据长度][数据] × N )
//! 尾部 28 字节: [u32 N][u64 档案起始偏移][u64 解压后长度][8 字节 "INKLNK01"]
//! ```
//!
//! 名字带前缀：`pf/` 落到 `C:\Program Files\inklink`，`eng/` 落到 `C:\inklink-engine`。
//! 整块 deflate 而不是逐条压：我们自己的格式，怎么省事怎么来；
//! 23 MB 裸数据压完 ≈9 MB，用户下载量差一倍多。

use std::ffi::{c_void, OsStr};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::read::DeflateDecoder;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    HKEY_LOCAL_MACHINE, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const MAGIC: &[u8; 8] = b"INKLNK01";
const TRAILER_LEN: usize = 28; // u32 + u64 + u64 + 8
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const PROGRAM_FILES: &str = r"C:\Program Files\inklink";
const ENGINE_DIR: &str = r"C:\inklink-engine";

const CLSID: &str = "{8F2A6C1E-4B3D-4F7A-9A21-5C6D7E8F9A0B}";
const PROFILE: &str = "{1B2C3D4E-5F60-4718-9A2B-3C4D5E6F7081}";
const TIPCAT: &str = "{34745C63-B2F0-4784-8B67-5E12C8701A31}"; // GUID_TFCAT_TIP_KEYBOARD

struct Entry {
    name: String,
    data: Vec<u8>,
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

fn exe_path() -> PathBuf {
    let mut buf = [0u16; 1024];
    let n = unsafe { GetModuleFileNameW(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) };
    PathBuf::from(String::from_utf16_lossy(&buf[..n as usize]))
}

fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned: u32 = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// 没提权就带着 `--elevated` 重新拉起自己，让 UAC 弹窗替我们提权。
fn relaunch_elevated(exe: &Path) -> bool {
    let file = wide(&exe.to_string_lossy());
    let verb = wide("runas");
    let params = wide("--elevated");
    let r = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW 返回值 > 32 才算成功（<=32 是错误码）
    r as usize > 32
}

// ── 载荷解析 ────────────────────────────────────────────────────

fn read_payload(exe: &Path) -> Result<Vec<Entry>, String> {
    let mut f = fs::File::open(exe).map_err(|e| format!("打不开自身：{e}"))?;
    let size = f.metadata().map_err(|e| format!("读不到自身大小：{e}"))?.len();
    if size < TRAILER_LEN as u64 {
        return Err("这个文件里没有安装载荷（是不是直接跑了构建产物？）".into());
    }
    f.seek(SeekFrom::End(-(TRAILER_LEN as i64)))
        .map_err(|e| format!("定位失败：{e}"))?;
    let mut tail = [0u8; TRAILER_LEN];
    f.read_exact(&mut tail).map_err(|e| format!("读尾部失败：{e}"))?;
    if &tail[20..28] != MAGIC {
        return Err("载荷标识不匹配（exe 末尾不是 inklink 的档案）".into());
    }
    let count = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]) as usize;
    let read_u64 = |at: usize| {
        u64::from_le_bytes([
            tail[at],
            tail[at + 1],
            tail[at + 2],
            tail[at + 3],
            tail[at + 4],
            tail[at + 5],
            tail[at + 6],
            tail[at + 7],
        ])
    };
    let start = read_u64(4);
    let raw_len = read_u64(12);
    if start >= size || start + TRAILER_LEN as u64 > size {
        return Err("载荷起始位置越界".into());
    }
    let compressed_len = (size - start - TRAILER_LEN as u64) as usize;
    f.seek(SeekFrom::Start(start)).map_err(|e| format!("定位载荷失败：{e}"))?;
    let mut compressed = vec![0u8; compressed_len];
    f.read_exact(&mut compressed).map_err(|e| format!("读载荷失败：{e}"))?;

    let mut raw = Vec::with_capacity(raw_len as usize);
    let mut dec = DeflateDecoder::new(&compressed[..]);
    dec.read_to_end(&mut raw).map_err(|e| format!("解压失败：{e}"))?;
    if raw.len() as u64 != raw_len {
        return Err(format!("解压后长度不符（期望 {raw_len}，实际 {}）", raw.len()));
    }

    // 解析条目
    let mut pos = 0usize;
    let take = |pos: &mut usize, n: usize, raw: &[u8]| -> Result<Vec<u8>, String> {
        if *pos + n > raw.len() {
            return Err("载荷被截断".into());
        }
        let v = raw[*pos..*pos + n].to_vec();
        *pos += n;
        Ok(v)
    };
    let mut entries = Vec::with_capacity(count);
    for i in 0..count {
        let name_len = u32::from_le_bytes(
            take(&mut pos, 4, &raw)?.try_into().unwrap_or([0, 0, 0, 0]),
        ) as usize;
        if name_len == 0 || name_len > 4096 {
            return Err(format!("条目 {i} 名字长度异常：{name_len}"));
        }
        let name = take(&mut pos, name_len, &raw)?;
        let data_len = u64::from_le_bytes(
            take(&mut pos, 8, &raw)?.try_into().unwrap_or([0; 8]),
        ) as usize;
        let data = take(&mut pos, data_len, &raw)?;
        entries.push(Entry {
            name: String::from_utf8_lossy(&name).to_string(),
            data,
        });
    }
    Ok(entries)
}

// ── 注册表 ──────────────────────────────────────────────────────

fn reg_write(root: HKEY, path: &str, name: Option<&str>, kind: u32, data: &[u8]) -> bool {
    unsafe {
        let sub = wide(path);
        let mut key: HKEY = std::ptr::null_mut();
        let mut disposition: u32 = 0;
        let rc = RegCreateKeyExW(
            root,
            sub.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut key,
            &mut disposition,
        );
        if rc != 0 {
            return false;
        }
        let value_name = name.map(wide);
        let rc = RegSetValueExW(
            key,
            value_name
                .as_ref()
                .map(|v| v.as_ptr())
                .unwrap_or(std::ptr::null()),
            0,
            kind,
            data.as_ptr(),
            data.len() as u32,
        );
        RegCloseKey(key);
        rc == 0
    }
}

fn reg_sz(root: HKEY, path: &str, name: Option<&str>, value: &str) -> bool {
    let mut bytes: Vec<u8> = value.encode_utf16().flat_map(|c| c.to_le_bytes()).collect();
    bytes.extend_from_slice(&[0, 0]); // 结尾 NUL：TSF 会直接按 C 字符串读
    reg_write(root, path, name, REG_SZ, &bytes)
}

fn reg_dword(root: HKEY, path: &str, name: &str, value: u32) -> bool {
    reg_write(root, path, Some(name), REG_DWORD, &value.to_le_bytes())
}

// ── 安装 ────────────────────────────────────────────────────────

fn stop_engine() {
    let _ = Command::new("taskkill")
        .args(["/IM", "rime-server.exe", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .status();
    std::thread::sleep(std::time::Duration::from_millis(600));
}

fn unpack(entries: &[Entry], pf: &Path, eng: &Path) -> Result<usize, String> {
    let mut written = 0;
    for e in entries {
        let (base, rel) = if let Some(rest) = e.name.strip_prefix("pf/") {
            (pf, rest)
        } else if let Some(rest) = e.name.strip_prefix("eng/") {
            (eng, rest)
        } else {
            continue;
        };
        // 防目录穿越：载荷是我们自己打的，但不该给"信任输入"开这个口子
        if rel.contains("..") {
            return Err(format!("条目名不合法：{}", e.name));
        }
        let target = base.join(rel.replace('/', "\\"));
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("建目录失败 {}：{e}", parent.display()))?;
        }
        fs::write(&target, &e.data).map_err(|e| format!("写文件失败 {}：{e}", target.display()))?;
        written += 1;
    }
    Ok(written)
}

fn start_engine(eng: &Path) -> bool {
    Command::new(eng.join("rime-server.exe"))
        .arg("--serve")
        .current_dir(eng)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .is_ok()
}

fn install() -> Result<(), String> {
    let exe = exe_path();
    let entries = read_payload(&exe)?;
    println!("载荷：{} 个文件", entries.len());

    let pf = PathBuf::from(PROGRAM_FILES);
    let eng = PathBuf::from(ENGINE_DIR);

    println!("停止旧引擎…");
    stop_engine();

    println!("解包到 {} 与 {} …", pf.display(), eng.display());
    let n = unpack(&entries, &pf, &eng)?;
    println!("  写入 {n} 个文件");

    let dll64 = pf.join("inklink_tsf.dll");
    let dll32 = pf.join("inklink_tsf32.dll");
    let engine_exe = eng.join("rime-server.exe");

    println!("注册 COM 组件…");
    let com64 = format!(r"SOFTWARE\Classes\CLSID\{CLSID}\InprocServer32");
    let com32 = format!(r"SOFTWARE\Classes\Wow6432Node\CLSID\{CLSID}\InprocServer32");
    for (path, dll) in [(&com64, &dll64), (&com32, &dll32)] {
        if !reg_sz(HKEY_LOCAL_MACHINE, path, None, &dll.to_string_lossy()) {
            return Err(format!("写 COM 注册失败：HKLM\\{path}（是不是没用管理员权限？）"));
        }
        reg_sz(HKEY_LOCAL_MACHINE, path, Some("ThreadingModel"), "Apartment");
    }

    println!("注册 TSF 输入法…");
    for root in [r"SOFTWARE\Microsoft\CTF\TIP", r"SOFTWARE\Wow6432Node\Microsoft\CTF\TIP"] {
        let tip = format!(r"{root}\{CLSID}");
        let _ = reg_sz(
            HKEY_LOCAL_MACHINE,
            &format!(r"{tip}\Category\Category\{TIPCAT}\{CLSID}"),
            None,
            "",
        );
        let _ = reg_sz(
            HKEY_LOCAL_MACHINE,
            &format!(r"{tip}\Category\Item\{CLSID}\{TIPCAT}"),
            None,
            "",
        );
        let lp = format!(r"{tip}\LanguageProfile\0x00000804\{PROFILE}");
        let _ = reg_sz(HKEY_LOCAL_MACHINE, &lp, None, "inklink 输入法");
        let _ = reg_dword(HKEY_LOCAL_MACHINE, &lp, "Enable", 1);
        let _ = reg_sz(HKEY_LOCAL_MACHINE, &lp, Some("IconFile"), &dll64.to_string_lossy());
        let _ = reg_dword(HKEY_LOCAL_MACHINE, &lp, "IconIndex", 0);
    }

    println!("配置引擎路径…");
    if !reg_sz(
        HKEY_LOCAL_MACHINE,
        r"SOFTWARE\inklink",
        Some("EnginePath"),
        &engine_exe.to_string_lossy(),
    ) {
        return Err("写 HKLM\\SOFTWARE\\inklink\\EnginePath 失败".into());
    }

    println!("设置开机自启…");
    let _ = reg_sz(
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Run",
        Some("inklink-engine"),
        &format!("\"{}\"", engine_exe.display()),
    );

    println!("启动引擎…");
    if start_engine(&eng) {
        println!("  已拉起 rime-server.exe");
    } else {
        println!("  警告：引擎没起来（第一次打字时 DLL 会自己拉起它，不影响使用）");
    }

    println!();
    println!("安装完成。");
    println!("  64 位 DLL : {}", dll64.display());
    println!("  32 位 DLL : {}", dll32.display());
    println!("  引擎      : {}", engine_exe.display());
    println!();
    println!("下一步：在「设置 → 时间和语言 → 语言和区域」里添加键盘 → inklink 输入法。");
    println!("（注册表已经写好了，但把输入法挂进你的语言列表只有系统设置能做。）");
    Ok(())
}

fn uninstall() -> Result<(), String> {
    println!("卸载 inklink…");
    stop_engine();

    for path in [
        format!(r"SOFTWARE\Classes\CLSID\{CLSID}"),
        format!(r"SOFTWARE\Classes\Wow6432Node\CLSID\{CLSID}"),
        format!(r"SOFTWARE\Microsoft\CTF\TIP\{CLSID}"),
        format!(r"SOFTWARE\Wow6432Node\Microsoft\CTF\TIP\{CLSID}"),
        r"SOFTWARE\inklink".to_string(),
    ] {
        let sub = wide(&path);
        unsafe {
            RegDeleteTreeW(HKEY_LOCAL_MACHINE, sub.as_ptr());
        }
    }
    let run = wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
    unsafe {
        // 只删值，不删整个 Run 键（别的程序也在里面）
        let mut key: HKEY = std::ptr::null_mut();
        let mut disp: u32 = 0;
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            run.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut key,
            &mut disp,
        ) == 0
        {
            let name = wide("inklink-engine");
            RegSetValueExW(key, name.as_ptr(), 0, REG_SZ, std::ptr::null(), 0);
            RegCloseKey(key);
        }
    }

    for dir in [PathBuf::from(PROGRAM_FILES), PathBuf::from(ENGINE_DIR)] {
        if dir.exists() {
            match fs::remove_dir_all(&dir) {
                Ok(()) => println!("  已删除 {}", dir.display()),
                Err(e) => println!("  删不掉 {}（{}），可能被占用，重启后再删", dir.display(), e),
            }
        }
    }
    println!();
    println!("卸载完成。请到系统设置里把 inklink 输入法移除（如果还列在那里）。");
    Ok(())
}

fn pause() {
    use std::io::Write;
    print!("按回车关闭…");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let elevated_flag = args.iter().any(|a| a == "--elevated");
    let uninstall_mode = args.iter().any(|a| a == "--uninstall");

    // --extract <目录>：只解包，不写注册表、不碰 Program Files。
    // 用途：CI 里验证"这个 exe 的载荷没坏"，以及不想安装时翻包里的文件。
    if let Some(i) = args.iter().position(|a| a == "--extract") {
        let dir = match args.get(i + 1) {
            Some(d) => PathBuf::from(d),
            None => {
                println!("用法：inklink-setup.exe --extract <目录>");
                return;
            }
        };
        match read_payload(&exe_path()) {
            Ok(entries) => match unpack(&entries, &dir, &dir) {
                Ok(n) => println!("已解包 {n} 个文件到 {}", dir.display()),
                Err(e) => eprintln!("解包失败：{e}"),
            },
            Err(e) => eprintln!("读载荷失败：{e}"),
        }
        return;
    }

    if !is_elevated() && !elevated_flag {
        let exe = exe_path();
        if relaunch_elevated(&exe) {
            return; // 提权后的那个实例继续干活
        }
        println!("需要管理员权限：请右键 → 以管理员身份运行。");
        pause();
        return;
    }

    let result = if uninstall_mode { uninstall() } else { install() };
    if let Err(msg) = result {
        eprintln!();
        eprintln!("出错了：{msg}");
        eprintln!();
        eprintln!("日志看这里：事件查看器 / 或者把这段错误发到 issue。");
    }
    pause();
}
