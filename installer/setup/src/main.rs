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
use windows_sys::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_DELAY_UNTIL_REBOOT, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
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
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
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
    let size = f
        .metadata()
        .map_err(|e| format!("读不到自身大小：{e}"))?
        .len();
    if size < TRAILER_LEN as u64 {
        return Err("这个文件里没有安装载荷（是不是直接跑了构建产物？）".into());
    }
    f.seek(SeekFrom::End(-(TRAILER_LEN as i64)))
        .map_err(|e| format!("定位失败：{e}"))?;
    let mut tail = [0u8; TRAILER_LEN];
    f.read_exact(&mut tail)
        .map_err(|e| format!("读尾部失败：{e}"))?;
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
    f.seek(SeekFrom::Start(start))
        .map_err(|e| format!("定位载荷失败：{e}"))?;
    let mut compressed = vec![0u8; compressed_len];
    f.read_exact(&mut compressed)
        .map_err(|e| format!("读载荷失败：{e}"))?;

    let mut raw = Vec::with_capacity(raw_len as usize);
    let mut dec = DeflateDecoder::new(&compressed[..]);
    dec.read_to_end(&mut raw)
        .map_err(|e| format!("解压失败：{e}"))?;
    if raw.len() as u64 != raw_len {
        return Err(format!(
            "解压后长度不符（期望 {raw_len}，实际 {}）",
            raw.len()
        ));
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
        let name_len =
            u32::from_le_bytes(take(&mut pos, 4, &raw)?.try_into().unwrap_or([0, 0, 0, 0]))
                as usize;
        if name_len == 0 || name_len > 4096 {
            return Err(format!("条目 {i} 名字长度异常：{name_len}"));
        }
        let name = take(&mut pos, name_len, &raw)?;
        let data_len =
            u64::from_le_bytes(take(&mut pos, 8, &raw)?.try_into().unwrap_or([0; 8])) as usize;
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
    for name in ["rime-server.exe", "inklink-settings.exe", "inklink-ui.exe"] {
        let _ = Command::new("taskkill")
            .args(["/IM", name, "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
    }
    std::thread::sleep(std::time::Duration::from_millis(600));
}

/// 升级前清理旧版本留下的安装痕迹。
///
/// 旧版可能没有卸载登记，也可能留下带版本号的 DLL；新版 setup 必须自己接管升级，
/// 用户不应该再单独运行 cleanup-old.cmd。只清理 inklink 自己的注册项和文件，
/// 不碰用户词库/笔记（这些属于用户数据，升级应保留）。
fn cleanup_legacy_install() {
    println!("清理旧版安装痕迹…");
    let uninstall = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\inklink";
    unsafe {
        let p = wide(uninstall);
        RegDeleteTreeW(HKEY_LOCAL_MACHINE, p.as_ptr());
    }
    let run = wide(r"Software\Microsoft\Windows\CurrentVersion\Run");
    unsafe {
        let mut key: HKEY = std::ptr::null_mut();
        let mut disp = 0u32;
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
            for name in ["inklink-engine", "inklink-ui"] {
                let n = wide(name);
                // 删除注册表值：用空 REG_SZ 写入会留下残值，必须 RegDeleteValueW。
                windows_sys::Win32::System::Registry::RegDeleteValueW(key, n.as_ptr());
            }
            RegCloseKey(key);
        }
    }
    let pf = Path::new(PROGRAM_FILES);
    if let Ok(files) = fs::read_dir(pf) {
        for entry in files.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("inklink_tsf_") && name.ends_with(".dll") {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    println!("  旧版清理完成，保留用户词库和设置");
}

/// 稳健地覆盖一个文件：先写同目录临时文件，再 `MoveFileExW` 原子替换。
///
/// 为什么要这么绕：直接 `fs::write` 覆盖 Program Files 里的同名文件时，
/// 只要目标正好被杀软扫描、或被某个进程短暂占着，就会拿到
/// `拒绝访问 (os error 5)` / `共享冲突`，整台机器的安装直接中断
/// （2026-11-12 本机复现：34 个文件写到 concrt140.dll 时炸）。
/// 临时文件 + 替换 + 重试既避开半截文件，也给了占用方松手的时间。
fn write_file_atomic(target: &Path, data: &[u8]) -> std::io::Result<()> {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let tmp = target.with_file_name(format!("{name}.inklink-new"));
    fs::write(&tmp, data)?;

    let to = wide(&target.to_string_lossy());
    let mut last = std::io::Error::from_raw_os_error(5);
    for attempt in 0..6 {
        let from = wide(&tmp.to_string_lossy());
        let ok = unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok != 0 {
            return Ok(());
        }
        last = std::io::Error::last_os_error();
        if attempt < 5 {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
    let _ = fs::remove_file(&tmp);
    Err(last)
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
            fs::create_dir_all(parent)
                .map_err(|e| format!("建目录失败 {}：{e}", parent.display()))?;
        }
        write_file_atomic(&target, &e.data)
            .map_err(|e| format!("写文件失败 {}：{e}", target.display()))?;
        written += 1;
    }
    Ok(written)
}

/// 拉起引擎。这里故意不用 `std::process::Command`——它没法关掉句柄继承，
/// 而引擎一旦继承了我们（或调用者 cmd）的 stdout，`Start-Process -Wait`、
/// cmd 的 `start /wait` 这类"等进程树结束"的调用会被一个常驻进程永远拖住。
fn start_engine(eng: &Path) -> bool {
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, DETACHED_PROCESS,
        PROCESS_INFORMATION, STARTUPINFOW,
    };
    let exe = eng.join("rime-server.exe");
    let mut cmdline = wide(&format!("\"{}\" --serve", exe.display()));
    let dir = wide(&eng.to_string_lossy());
    let mut si = STARTUPINFOW::default();
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        let ok = CreateProcessW(
            std::ptr::null(),
            cmdline.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0, // bInheritHandles = FALSE：不继承任何句柄
            CREATE_NO_WINDOW | DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB,
            std::ptr::null(),
            dir.as_ptr(),
            &si,
            &mut pi,
        );
        if ok != 0 {
            CloseHandle(pi.hProcess);
            CloseHandle(pi.hThread);
            return true;
        }
        // 作业对象不允许 breakaway 时会失败，去掉这个标志再试一次。
        let ok = CreateProcessW(
            std::ptr::null(),
            cmdline.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NO_WINDOW | DETACHED_PROCESS,
            std::ptr::null(),
            dir.as_ptr(),
            &si,
            &mut pi,
        );
        if ok != 0 {
            CloseHandle(pi.hProcess);
            CloseHandle(pi.hThread);
            return true;
        }
    }
    false
}

/// 把输入法挂进用户的 zh-Hans-CN 语言列表（`Win+Space` 才看得到它）。
///
/// 单文件安装器只能写注册表，但"语言列表里有这个键盘"是另一回事：
/// 只有 `Set-WinUserLanguageList` 会改它。以前这里只打印一句"请去设置里添加键盘"，
/// 结果同事装完按 Win+Space 找不到 inklink（2026-11-12 反馈截图）。
/// 复用 PowerShell 是刻意的：这段逻辑和 installer\install.ps1 完全一致，
/// 比在 Rust 里走 WinRT 的 Windows.Globalization 少一大堆样板。
fn add_to_language_list() -> bool {
    let script = format!("$c='{CLSID}';$p='{PROFILE}';$tip='0804:'+$c+$p;").to_string()
        + "$l=Get-WinUserLanguageList;"
        + "$z=$l|Where-Object{$_.LanguageTag -eq 'zh-Hans-CN'}|Select-Object -First 1;"
        + "if(-not $z){$n=New-WinUserLanguageList 'zh-Hans-CN';$l.Add($n[0]);"
        + "$z=$l|Where-Object{$_.LanguageTag -eq 'zh-Hans-CN'}|Select-Object -First 1};"
        + "if($z.InputMethodTips -notcontains $tip){$z.InputMethodTips.Add($tip);"
        + "Set-WinUserLanguageList $l -Force};exit 0";
    run_powershell(&script)
}

/// 从语言列表里移除（卸载时用）。失败不影响卸载。
fn remove_from_language_list() -> bool {
    let script = format!("$c='{CLSID}';$p='{PROFILE}';$tip='0804:'+$c+$p;").to_string()
        + "$l=Get-WinUserLanguageList;$ch=$false;"
        + "foreach($z in $l){while($z.InputMethodTips -contains $tip){"
        + "$z.InputMethodTips.Remove($tip);$ch=$true}};"
        + "if($ch){Set-WinUserLanguageList $l -Force};exit 0";
    run_powershell(&script)
}

/// 跑一段 PowerShell 并等它结束，返回是否退出码为 0。
/// 全部用单引号拼脚本，避免命令行引号被转义坏。
fn run_powershell(script: &str) -> bool {
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW,
    };
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};

    let mut cmdline = wide(&format!(
        "powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -Command \"{script}\""
    ));
    let mut si = STARTUPINFOW::default();
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        let ok = CreateProcessW(
            std::ptr::null(),
            cmdline.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        );
        if ok == 0 {
            return false;
        }
        // 最多等 20 秒：系统接口偶尔会卡，但不能把安装器吊死
        WaitForSingleObject(pi.hProcess, 20_000);
        let mut code: u32 = 1;
        GetExitCodeProcess(pi.hProcess, &mut code);
        CloseHandle(pi.hProcess);
        CloseHandle(pi.hThread);
        code == 0
    }
}

/// 装完实测一次查询：引擎在跑 ≠ 能查出词。
///
/// 同事机器上踩过：DLL 装好了、浮条正常，但引擎根本连不上（`pipe unavailable`），
/// 表现是"打拼音只有字母没有候选"。安装器如果只说"已拉起 rime-server.exe"，
/// 用户会以为装好了。这里直接走一遍真实协议，把结论打到屏幕上。
fn probe_engine(code: &str) -> i32 {
    let path = r"\\.\pipe\inklink-engine";
    let mut last_err = String::new();
    for attempt in 0..3 {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
        {
            Ok(mut pipe) => {
                use std::io::{Read, Write};
                let bytes = code.as_bytes();
                let mut req = Vec::with_capacity(7 + bytes.len());
                req.push(1u8); // 类型 1 = 查询
                req.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                req.extend_from_slice(bytes);
                req.extend_from_slice(&0u16.to_le_bytes()); // app 长度 0
                if pipe.write_all(&req).is_err() {
                    last_err = "写管道失败".into();
                } else {
                    let mut len = [0u8; 4];
                    match pipe.read_exact(&mut len) {
                        Ok(()) => return u32::from_le_bytes(len) as i32,
                        Err(e) => last_err = format!("读管道失败：{e}"),
                    }
                }
            }
            Err(e) => last_err = e.to_string(),
        }
        if attempt < 2 {
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    }
    println!("  探测失败：{last_err}");
    -1
}

fn install() -> Result<(), String> {
    let exe = exe_path();
    let entries = read_payload(&exe)?;
    println!("载荷：{} 个文件", entries.len());

    let pf = PathBuf::from(PROGRAM_FILES);
    let eng = PathBuf::from(ENGINE_DIR);

    println!("停止旧引擎…");
    stop_engine();
    cleanup_legacy_install();

    println!("解包到 {} 与 {} …", pf.display(), eng.display());
    let n = unpack(&entries, &pf, &eng)?;
    println!("  写入 {n} 个文件");

    // 把安装器本身留在安装目录，应用列表的卸载命令直接调用它的 --uninstall。
    // 这样单文件 setup 不依赖外部 uninstall.ps1（旧版正是因此找不到卸载入口）。
    let setup_copy = pf.join("inklink-setup.exe");
    let setup_bytes = fs::read(&exe).map_err(|e| format!("读取安装器自身失败：{e}"))?;
    write_file_atomic(&setup_copy, &setup_bytes).map_err(|e| format!("保存卸载器失败：{e}"))?;

    let dll64 = pf.join("inklink_tsf.dll");
    let dll32 = pf.join("inklink_tsf32.dll");
    let engine_exe = eng.join("rime-server.exe");

    println!("注册 COM 组件…");
    let com64 = format!(r"SOFTWARE\Classes\CLSID\{CLSID}\InprocServer32");
    let com32 = format!(r"SOFTWARE\Classes\Wow6432Node\CLSID\{CLSID}\InprocServer32");
    for (path, dll) in [(&com64, &dll64), (&com32, &dll32)] {
        if !reg_sz(HKEY_LOCAL_MACHINE, path, None, &dll.to_string_lossy()) {
            return Err(format!(
                "写 COM 注册失败：HKLM\\{path}（是不是没用管理员权限？）"
            ));
        }
        reg_sz(
            HKEY_LOCAL_MACHINE,
            path,
            Some("ThreadingModel"),
            "Apartment",
        );
    }

    println!("注册 TSF 输入法…");
    for root in [
        r"SOFTWARE\Microsoft\CTF\TIP",
        r"SOFTWARE\Wow6432Node\Microsoft\CTF\TIP",
    ] {
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
        // 指向单独的 .ico，不是 DLL：DLL 里没有编图标资源（build.rs 只写版本号），
        // 指过去语言列表会退回显示「简体」两个字；.ico 也不会因 DLL 换版本名而失效。
        let icon = pf.join("inklink.ico");
        let _ = reg_sz(
            HKEY_LOCAL_MACHINE,
            &lp,
            Some("IconFile"),
            &icon.to_string_lossy(),
        );
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

    println!("登记应用列表卸载入口…");
    let uninstall_key = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\inklink";
    let setup_cmd = format!(r#""{}" --uninstall"#, setup_copy.display());
    if !reg_sz(
        HKEY_LOCAL_MACHINE,
        uninstall_key,
        Some("DisplayName"),
        "inklink 输入法",
    ) || !reg_sz(
        HKEY_LOCAL_MACHINE,
        uninstall_key,
        Some("DisplayVersion"),
        env!("CARGO_PKG_VERSION"),
    ) || !reg_sz(
        HKEY_LOCAL_MACHINE,
        uninstall_key,
        Some("Publisher"),
        "inklink (open source, GPL-3.0)",
    ) || !reg_sz(
        HKEY_LOCAL_MACHINE,
        uninstall_key,
        Some("InstallLocation"),
        PROGRAM_FILES,
    ) || !reg_sz(
        HKEY_LOCAL_MACHINE,
        uninstall_key,
        Some("UninstallString"),
        &setup_cmd,
    ) || !reg_sz(
        HKEY_LOCAL_MACHINE,
        uninstall_key,
        Some("QuietUninstallString"),
        &setup_cmd,
    ) {
        return Err("写应用列表卸载登记失败".into());
    }

    println!("设置开机自启…");
    let _ = reg_sz(
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Run",
        Some("inklink-engine"),
        &format!("\"{}\"", engine_exe.display()),
    );

    println!("加入输入法列表…");
    let listed = add_to_language_list();
    if listed {
        println!("  已加入：按 Win+Space 就能切到 inklink 输入法");
    } else {
        println!("  警告：自动加入失败，请手动到「设置 → 时间和语言 → 语言和区域」添加键盘");
    }

    println!("启动引擎…");
    if start_engine(&eng) {
        println!("  已拉起 rime-server.exe");
    } else {
        println!("  警告：引擎没起来（第一次打字时 DLL 会自己拉起它，不影响使用）");
    }

    // 真跑一次查询：引擎在跑但查不到词，用户打字还是只有字母。
    println!("实测一次查询（nihao）…");
    std::thread::sleep(std::time::Duration::from_secs(2));
    let cands = probe_engine("nihao");
    if cands > 0 {
        println!("  引擎实测通过：nihao -> {cands} 个候选");
    } else {
        println!("  警告：引擎没有给出候选（可能是首次编译词库还没完成）。");
        println!("  请等 10 秒后再打一次；仍然不行就跑安装目录里的 diagnose.cmd。");
    }

    println!();
    println!("安装完成。");
    println!("  64 位 DLL : {}", dll64.display());
    println!("  32 位 DLL : {}", dll32.display());
    println!("  引擎      : {}", engine_exe.display());
    println!();
    if listed {
        println!("下一步：按 Win+Space 切到「inklink 输入法」，打 nihao 试试。");
    } else {
        println!("下一步：在「设置 → 时间和语言 → 语言和区域」里添加键盘 → inklink 输入法。");
    }
    println!("重要：微信、WPS、浏览器等已经打开的程序，要重新打开才能用上新版输入法。");
    println!("卸载：设置 → 应用 → 已安装的应用 → inklink 输入法。");
    Ok(())
}

fn uninstall() -> Result<(), String> {
    println!("卸载 inklink…");
    stop_engine();
    // 先从语言列表里摘掉，再删注册表：反过来的话系统可能还留着一个指向不存在 DLL 的键盘项
    if remove_from_language_list() {
        println!("  已从输入法列表移除");
    }

    for path in [
        format!(r"SOFTWARE\Classes\CLSID\{CLSID}"),
        format!(r"SOFTWARE\Classes\Wow6432Node\CLSID\{CLSID}"),
        format!(r"SOFTWARE\Microsoft\CTF\TIP\{CLSID}"),
        format!(r"SOFTWARE\Wow6432Node\Microsoft\CTF\TIP\{CLSID}"),
        r"SOFTWARE\inklink".to_string(),
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\inklink".to_string(),
    ] {
        let sub = wide(&path);
        unsafe {
            RegDeleteTreeW(HKEY_LOCAL_MACHINE, sub.as_ptr());
        }
    }
    // 用户级设置（中英模式、外观、AI 配置、学习开关）
    {
        let sub = wide(r"SOFTWARE\inklink");
        unsafe {
            RegDeleteTreeW(HKEY_CURRENT_USER, sub.as_ptr());
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

    // 文件删除：能删的立刻删；删不掉的登记成「重启后删除」。
    // 这一步是正规安装器（搜狗/微软拼音）的做法：explorer、微信把 DLL 加载进内存后
    // 谁都删不掉它，只能交给重启时的系统清理。以前直接 fs::remove_dir_all 失败就跳过，
    // 结果目录一直留在 Program Files 里（2026-11-12 同事反馈根因）。
    let mut deferred = 0usize;
    for dir in [PathBuf::from(PROGRAM_FILES), PathBuf::from(ENGINE_DIR)] {
        if !dir.exists() {
            continue;
        }
        match fs::remove_dir_all(&dir) {
            Ok(()) => println!("  已删除 {}", dir.display()),
            Err(_) => {
                let n = schedule_delete_tree(&dir);
                deferred += n;
                println!(
                    "  {} 里的 {n} 个文件被占用，已登记为「重启后删除」",
                    dir.display()
                );
            }
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let data = PathBuf::from(local).join("inklink");
        if data.exists() {
            let _ = fs::remove_dir_all(&data);
            println!("  已删除 {}", data.display());
        }
    }
    println!();
    println!("卸载完成。");
    if deferred > 0 {
        println!("还剩下 {deferred} 个文件正被程序占用，已安排在下一次开机时自动删除。");
    }
    println!("如果 Win+Space 里还能看到 inklink，注销或重启一次就会消失");
    println!("（输入法 DLL 仍被 explorer 等进程加载着，这是正常的）。");
    Ok(())
}

/// 把整棵目录里的文件登记成「重启后删除」，并尽量把目录本身也登记掉。
///
/// Windows 的 `MOVEFILE_DELAY_UNTIL_REBOOT` 只能删**空目录**，
/// 所以先逐个登记文件、再登记目录；顺序由系统在重启时保证（它按登记顺序处理，
/// 文件在前、目录在后刚好能删干净）。返回登记成功的文件个数。
fn schedule_delete_tree(dir: &Path) -> usize {
    let mut dirs: Vec<PathBuf> = Vec::new();

    fn walk(d: &Path, files: &mut Vec<PathBuf>, dirs: &mut Vec<PathBuf>) {
        if let Ok(entries) = fs::read_dir(d) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, files, dirs);
                    dirs.push(p);
                } else {
                    files.push(p);
                }
            }
        }
    }

    let mut files: Vec<PathBuf> = Vec::new();
    walk(dir, &mut files, &mut dirs);
    dirs.push(dir.to_path_buf());

    let mut count = 0usize;
    let mut fallback: Vec<String> = Vec::new();

    for f in files.iter().chain(dirs.iter()) {
        let w = wide(&f.to_string_lossy());
        // 正确用法：源文件在前、目标给 NULL —— 表示「重启时删掉这个文件」。
        let ok = unsafe { MoveFileExW(w.as_ptr(), std::ptr::null(), MOVEFILE_DELAY_UNTIL_REBOOT) };
        if ok != 0 {
            count += 1;
        } else {
            fallback.push(f.to_string_lossy().to_string());
        }
    }

    // MoveFileEx 在某些环境（受策略/沙箱限制）会返回 ERROR_PATH_NOT_FOUND，
    // 这时直接把条目写进系统自己的待删队列，效果一样：下次开机由 SMSS 删掉。
    if !fallback.is_empty() {
        let added = append_pending_renames(&fallback);
        count += added;
    }
    count
}

/// 把路径追加到 `PendingFileRenameOperations`（系统重启时的删除队列）。
///
/// 格式是 REG_MULTI_SZ，每个「删除」占两个字符串：`\??\C:\path` 后面跟一个空串。
/// 必须先读出旧内容再追加，否则会把别的程序的待删任务冲掉。
fn append_pending_renames(paths: &[String]) -> usize {
    use windows_sys::Win32::System::Registry::{
        RegOpenKeyExW, RegQueryValueExW, KEY_READ, KEY_WRITE, REG_MULTI_SZ,
    };

    const SESSION_MANAGER: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager";
    const VALUE: &str = "PendingFileRenameOperations";

    let key_path = wide(SESSION_MANAGER);
    let mut key: HKEY = std::ptr::null_mut();
    let rc = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            key_path.as_ptr(),
            0,
            KEY_READ | KEY_WRITE,
            &mut key,
        )
    };
    if rc != 0 {
        return 0;
    }

    let name = wide(VALUE);
    let mut existing: Vec<u16> = Vec::new();
    let mut ty: u32 = 0;
    let mut size: u32 = 0;
    let rc = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut ty,
            std::ptr::null_mut(),
            &mut size,
        )
    };
    if rc == 0 && size > 0 {
        existing.resize((size / 2) as usize, 0);
        let mut got = size;
        let rc2 = unsafe {
            RegQueryValueExW(
                key,
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut ty,
                existing.as_mut_ptr() as *mut u8,
                &mut got,
            )
        };
        if rc2 == 0 {
            existing.truncate((got / 2) as usize);
        } else {
            existing.clear();
        }
    }
    while existing.last() == Some(&0) {
        existing.pop();
    }

    let mut out = existing;
    let mut added = 0usize;
    for p in paths {
        let full = if p.starts_with(r"\\?\") || p.starts_with(r"\??\") {
            p.clone()
        } else {
            format!(r"\??\{p}")
        };
        out.extend(full.encode_utf16());
        out.push(0);
        out.push(0); // 目标为空 = 删除
        added += 1;
    }
    out.push(0); // MULTI_SZ 结尾

    let bytes: Vec<u8> = out.iter().flat_map(|c| c.to_le_bytes()).collect();
    let rc = unsafe {
        RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_MULTI_SZ,
            bytes.as_ptr(),
            bytes.len() as u32,
        )
    };
    unsafe {
        RegCloseKey(key);
    }
    if rc == 0 {
        added
    } else {
        0
    }
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

    // --schedule-test <目录>：内部自检。把整棵目录登记成「重启后删除」并打印条数。
    // 这是给开发和排障用的：卸载时到底有没有把任务交给系统，光看代码不算验证过。
    if let Some(i) = args.iter().position(|a| a == "--schedule-test") {
        let dir = match args.get(i + 1) {
            Some(d) => PathBuf::from(d),
            None => {
                println!("用法：inklink-setup.exe --schedule-test <目录>");
                return;
            }
        };
        let n = schedule_delete_tree(&dir);
        println!("已登记 {n} 条重启删除任务（目录 {}）", dir.display());
        return;
    }

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

    let result = if uninstall_mode {
        uninstall()
    } else {
        install()
    };
    if let Err(msg) = result {
        eprintln!();
        eprintln!("出错了：{msg}");
        eprintln!();
        eprintln!("日志看这里：事件查看器 / 或者把这段错误发到 issue。");
    }
    pause();
}
