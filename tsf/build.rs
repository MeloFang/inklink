// 两件事：
//   1. 把仓库根目录 VERSION 里的版本号编进 DLL，日志里就能看出跑的是哪一版。
//      版本号本身由 tools\version.ps1 维护（单一真相源），这里只读不写。
//   2. 把产品图标作为资源编进 DLL。
//
// 为什么要编图标：Windows 的语言列表（Win+空格 那个列表）在 profile 没给出可用图标时，
// 会退回显示语言的文字缩写（我们是「简体」）。注册表里 `IconFile` 明明指着一个能用的
// .ico，系统仍然显示文字 —— 实测 `ExtractIconEx` 对微软自己的 tiptsf.dll 返回 0，
// 而微软拼音照样显示「拼」，说明那个字符本来就是"兜底文字"，不是图标。
// 真正被系统拿去画的是**文本服务 DLL 自己的图标资源**，所以图标必须编进 inklink_tsf.dll。
//
// 用 Windows SDK 的 rc.exe 编 .res，再交给链接器。找不到 rc.exe 时只告警、不失败
// （CI 的 windows-latest 一定有；本地没装 SDK 也能构建，只是语言列表里没有图标）。
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let version = std::fs::read_to_string("../VERSION")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "0.0.0".to_string());
    println!("cargo:rustc-env=INKLINK_VERSION={version}");
    println!("cargo:rerun-if-changed=../VERSION");

    let icon = PathBuf::from("../installer/inklink.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    if icon.exists() {
        embed_icon(&icon);
    }
}

fn embed_icon(icon: &PathBuf) {
    let Some(rc) = find_rc() else {
        println!(
            "cargo:warning=没找到 Windows SDK 的 rc.exe，DLL 里不会编进图标（语言列表会显示「简体」）"
        );
        return;
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let rcfile = out.join("icon.rc");
    // rc.exe 只认反斜杠路径，而且不认 canonicalize 加上的 `\\?\` 前缀（会直接报错退出）
    let icon_abs = std::fs::canonicalize(icon).unwrap_or_else(|_| icon.to_path_buf());
    let icon_str = icon_abs.display().to_string();
    let icon_str = icon_str
        .strip_prefix(r"\\?\")
        .unwrap_or(&icon_str)
        .to_string();
    let script = format!("1 ICON \"{icon_str}\"\n");
    if std::fs::write(&rcfile, script).is_err() {
        println!("cargo:warning=写 icon.rc 失败，跳过图标");
        return;
    }

    let res = out.join("icon.res");
    let result = Command::new(&rc)
        .arg("/nologo")
        .arg(format!("/fo{}", res.display()))
        .arg(&rcfile)
        .output();
    match result {
        Ok(o) if o.status.success() && res.exists() => {
            println!("cargo:rustc-link-arg={}", res.display());
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            let outtxt = String::from_utf8_lossy(&o.stdout);
            println!("cargo:warning=rc.exe 失败（{}）：{err}{outtxt}", o.status);
        }
        Err(e) => println!("cargo:warning=调用 rc.exe 失败：{e}"),
    }
}

/// 在 Windows SDK 目录下按版本倒序找 rc.exe。
fn find_rc() -> Option<PathBuf> {
    let roots = [
        r"C:\Program Files (x86)\Windows Kits\10\bin",
        r"C:\Program Files\Windows Kits\10\bin",
    ];
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut versions: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        // 目录名是 10.0.26100.0 这种，字典序 == 版本序
        versions.sort();
        versions.reverse();
        for v in versions {
            for arch in ["x64", "arm64", "x86"] {
                let candidate = v.join(arch).join("rc.exe");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}
