# 本机构建 + 部署（开发用）
#
#   powershell -ExecutionPolicy Bypass -File tools\deploy-local.ps1            # 自增修订号 → 构建 → 部署
#   powershell -ExecutionPolicy Bypass -File tools\deploy-local.ps1 -NoBump    # 版本号不变，重新构建部署
#   powershell -ExecutionPolicy Bypass -File tools\deploy-local.ps1 -NoBuild   # 只把已有产物拷过去
#
# 为什么每次都要换 DLL 文件名：DLL 一旦被进程加载就卸不掉，同名覆盖注册表也指向旧文件，
# 只有「新文件名 + 改注册表 + 重启目标程序」才真正换上新代码。

param(
    [switch]$NoBump,
    [switch]$NoBuild,
    [switch]$SkipTest,
    # 只构建，不往 Program Files 铺、不改注册表、不启引擎（CI 用：那里没有 TSF 环境）
    [switch]$NoDeploy
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\version.ps1"

if (-not $NoBump) { Step-InklinkVersion | Out-Null }
$version = Get-InklinkVersion
$root    = Get-InklinkRepoRoot
$tsf     = Join-Path $root 'tsf'

Write-Host "== inklink $version ==" -ForegroundColor Cyan

$dll64 = Join-Path $tsf 'target-static\release\inklink_tsf.dll'
$dll32 = Join-Path $tsf 'target-static\i686-pc-windows-msvc\release\inklink_tsf.dll'

# ── 构建 ───────────────────────────────────────────────────────
if (-not $NoBuild) {
    $start = Get-Date
    $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"   # 必须先修 PATH：Program Files 下的 GNU rustc 会盖掉 MSVC
    Push-Location $tsf
    try {
        if (-not $SkipTest) {
            Write-Host '[测试] cargo test --release' -ForegroundColor White
            cargo test --release
            if ($LASTEXITCODE -ne 0) { throw "单测失败（退出码 $LASTEXITCODE）" }
        }
        Write-Host '[构建] x64' -ForegroundColor White
        cargo build --release --lib --target-dir target-static
        $rc64 = $LASTEXITCODE
        Write-Host '[构建] x86' -ForegroundColor White
        cargo build --release --lib --target i686-pc-windows-msvc --target-dir target-static
        $rc32 = $LASTEXITCODE
    } finally { Pop-Location }

    # 设置窗（P2a）：独立小 exe，跟 DLL 放同一个目录，浮条菜单里的「设置…」会拉起它
    $settingsDir = Join-Path $root 'settings'
    if (Test-Path $settingsDir) {
        Write-Host '[构建] 设置窗' -ForegroundColor White
        Push-Location $settingsDir
        try { cargo build --release } finally { Pop-Location }
        if ($LASTEXITCODE -ne 0) { throw "设置窗构建失败（退出码 $LASTEXITCODE）" }
    }

    if ($rc64 -ne 0 -or $rc32 -ne 0) { throw "构建失败（cargo 退出码 x64=$rc64 x86=$rc32）" }
    # 产物必须是这次构建写出来的，否则等于把旧 DLL 换个新名字部署
    foreach ($p in @($dll64, $dll32)) {
        if (-not (Test-Path $p)) { throw "没产出：$p" }
        if ((Get-Item $p).LastWriteTime -lt $start) { throw "产物没更新：$p（最后写入 $((Get-Item $p).LastWriteTime.ToString('HH:mm:ss'))）" }
    }
}

# ── 部署 ───────────────────────────────────────────────────────
if ($NoDeploy) {
    Write-Host "== inklink $version 构建完成（-NoDeploy：没有部署）==" -ForegroundColor Green
    Write-Host "  $dll64"
    Write-Host "  $dll32"
    return
}

$PF      = 'C:\Program Files\inklink'
$ENG     = 'C:\inklink-engine'
$CLSID   = '{8F2A6C1E-4B3D-4F7A-9A21-5C6D7E8F9A0B}'
$target64 = Join-Path $PF "inklink_tsf_$version.dll"
$target32 = Join-Path $PF "inklink_tsf32_$version.dll"

New-Item -ItemType Directory -Force $PF | Out-Null
Copy-Item $dll64 $target64 -Force
Copy-Item $dll32 $target32 -Force

# 注册表指向新 DLL 必须紧跟拷贝 —— 后面任何一步失败（字体/设置窗被占用）都不能
# 把系统留在"新文件已拷、注册表还在指旧 DLL"的半吊子状态（2026-11-08 实际踩到）。
Set-ItemProperty "HKLM:\SOFTWARE\Classes\CLSID\$CLSID\InprocServer32" -Name '(default)' -Value $target64
Set-ItemProperty "HKLM:\SOFTWARE\Classes\Wow6432Node\CLSID\$CLSID\InprocServer32" -Name '(default)' -Value $target32

# 随包字体（朱雀仿宋）：AI 面板按 <DLL 目录>\fonts 私有加载
$fontsSrc = Join-Path $root 'installer\fonts'
if (Test-Path $fontsSrc) {
    New-Item -ItemType Directory -Force (Join-Path $PF 'fonts') | Out-Null
    try {
        Copy-Item "$fontsSrc\*" (Join-Path $PF 'fonts') -Force -ErrorAction Stop
    } catch {
        Write-Host "  字体跳过（正被占用，重启后重跑 -NoBump 即可）：$($_.Exception.Message)" -ForegroundColor Yellow
    }
}

# 语言列表里那一行的图标（DllRegisterServer 会按 DLL 同目录的 inklink.ico 找它）
$ico = Join-Path $root 'installer\inklink.ico'
if (Test-Path $ico) { Copy-Item $ico (Join-Path $PF 'inklink.ico') -Force
        $badge = Join-Path $root 'installer\inklink-badge.ico'
        if (Test-Path $badge) { Copy-Item $badge (Join-Path $PF 'inklink-badge.ico') -Force } }

# 设置窗：浮条菜单点「设置…」时按 DLL 同目录找它
$settingsExe = Join-Path $root 'settings\target\release\inklink-settings.exe'
if (Test-Path $settingsExe) {
    $dstSet = Join-Path $PF 'inklink-settings.exe'
    try {
        Copy-Item $settingsExe $dstSet -Force -ErrorAction Stop
    } catch {
        Write-Host "  设置窗跳过（先关掉「inklink 设置」再跑一次 -NoBump）：$($_.Exception.Message)" -ForegroundColor Yellow
    }
}

# 浮条现在画在 DLL 里（宿主进程内，搜狗那种），不再有独立的 inklink-ui.exe。
Set-ItemProperty "HKLM:\SOFTWARE\Classes\CLSID\$CLSID\InprocServer32" -Name '(default)' -Value $target64
Set-ItemProperty "HKLM:\SOFTWARE\Classes\Wow6432Node\CLSID\$CLSID\InprocServer32" -Name '(default)' -Value $target32

# 语言列表里那一行的小图标指到 inklink.ico。
# 不指 DLL：DLL 里没有编图标资源（build.rs 只写版本号），指过去系统会退回显示
# 「简体」两个字；而且每次部署 DLL 都换文件名，指 DLL 的话 IconFile 立刻变成死路径
# （2026-11-02 就是这么坏的：安装器写的是不带版本号的 inklink_tsf.dll，部署后那个文件没了）。
$iconPath = Join-Path $PF 'inklink.ico'
if (Test-Path $iconPath) {
    $profile = '{1B2C3D4E-5F60-4718-9A2B-3C4D5E6F7081}'
    foreach ($root in @('HKLM:\SOFTWARE\Microsoft\CTF\TIP', 'HKLM:\SOFTWARE\Wow6432Node\Microsoft\CTF\TIP')) {
        $lp = "$root\$CLSID\LanguageProfile\0x00000804\$profile"
        if (Test-Path $lp) {
            Set-ItemProperty $lp -Name 'IconFile'  -Value $iconPath
            Set-ItemProperty $lp -Name 'IconIndex' -Value 0 -Type DWord
        }
    }
}

# ── 清理旧 DLL ─────────────────────────────────────────────────
# DLL 被加载后不能热替换，所以每次部署都是新文件名 + 改注册表，旧文件会一直堆着。
# 这里删掉注册表已经不再引用的 inklink_tsf*.dll（包括 v19 时代的旧命名）。
# 被正在运行的程序占着的会删不掉 —— 跳过并提示，下次部署再收拾，不让它变成硬错误。
$keep = @($target64, $target32) | ForEach-Object { (Split-Path $_ -Leaf).ToLowerInvariant() }
$staleDlls = Get-ChildItem $PF -Filter 'inklink_tsf*.dll' -ErrorAction SilentlyContinue |
    Where-Object { $keep -notcontains $_.Name.ToLowerInvariant() -and $_.DirectoryName -eq $PF }
if ($staleDlls) {
    $removed = 0
    $locked  = @()
    foreach ($d in $staleDlls) {
        if (-not $d.FullName.StartsWith($PF + '\', [StringComparison]::OrdinalIgnoreCase)) { continue }
        try {
            Remove-Item $d.FullName -Force -ErrorAction Stop
            $removed++
        } catch {
            $locked += $d.Name
        }
    }
    if ($removed -gt 0) { Write-Host "  已清理旧 DLL $removed 个" -ForegroundColor DarkGray }
    if ($locked.Count -gt 0) {
        Write-Host ("  有 {0} 个旧 DLL 正被程序占用（重启那些程序后再跑一次 -NoBump 即可清掉）：{1}" -f $locked.Count, ($locked -join ', ')) -ForegroundColor Yellow
    }
}

# ── 引擎 ───────────────────────────────────────────────────────
# 把刚编出来的引擎同步过去。以前这里只"没在跑就启动"，编完的 exe 留在
# 源码目录里从不下发 —— 表现为"改了引擎、重启引擎还是老行为"
# （2026-11-11 实测：AI provider 改成 openai 后仍走 Ollama 协议）。
$engSrc = Join-Path $root 'rime-server\rime-server.exe'
if (Test-Path $engSrc) {
    $engDst = Join-Path $ENG 'rime-server.exe'
    if (-not (Test-Path $engDst) -or
        (Get-Item $engSrc).LastWriteTime -gt (Get-Item $engDst).LastWriteTime) {
        Get-Process rime-server -ErrorAction SilentlyContinue | Stop-Process -Force
        Start-Sleep -Milliseconds 500
        New-Item -ItemType Directory -Force $ENG | Out-Null
        Copy-Item $engSrc $engDst -Force
        Write-Host '  引擎已更新' -ForegroundColor Green
    }
}

if (-not (Get-Process rime-server -ErrorAction SilentlyContinue)) {
    if (Test-Path (Join-Path $ENG 'rime-server.exe')) {
        # 输出重定向到文件：不重定向的话引擎会继承调用者的控制台/管道句柄，
        # 用 Start-Process -Wait -Redirect* 之类的调用会一直等不到 EOF（踩过好几次）。
        Start-Process (Join-Path $ENG 'rime-server.exe') -WorkingDirectory $ENG -WindowStyle Hidden `
            -RedirectStandardOutput (Join-Path $ENG 'rime-server.stdout.log') `
            -RedirectStandardError (Join-Path $ENG 'rime-server.stderr.log')
        Write-Host '  引擎已启动' -ForegroundColor Green
    } else {
        Write-Host "  警告：找不到 $ENG\rime-server.exe，引擎没起" -ForegroundColor Yellow
    }
}

# ── 模糊音配置 ─────────────────────────────────────────────────
# 按 rime-config\fuzzy.yaml 生成引擎的 luna_pinyin_simp.custom.yaml；
# 内容没变就什么都不做（不会白白重启引擎）
& (Join-Path $PSScriptRoot 'apply-fuzzy.ps1') -Destination (Join-Path $ENG 'rime\user')

Write-Host ''
Write-Host "已部署 $version" -ForegroundColor Green
Write-Host "  64 位 : $target64"
Write-Host "  32 位 : $target32"
Write-Host '  已加载旧 DLL 的程序要重启一次才会用上新版本' -ForegroundColor Yellow