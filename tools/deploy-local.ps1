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
    [switch]$SkipTest
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

    if ($rc64 -ne 0 -or $rc32 -ne 0) { throw "构建失败（cargo 退出码 x64=$rc64 x86=$rc32）" }
    # 产物必须是这次构建写出来的，否则等于把旧 DLL 换个新名字部署
    foreach ($p in @($dll64, $dll32)) {
        if (-not (Test-Path $p)) { throw "没产出：$p" }
        if ((Get-Item $p).LastWriteTime -lt $start) { throw "产物没更新：$p（最后写入 $((Get-Item $p).LastWriteTime.ToString('HH:mm:ss'))）" }
    }
}

# ── 部署 ───────────────────────────────────────────────────────
$PF      = 'C:\Program Files\inklink'
$ENG     = 'C:\inklink-engine'
$CLSID   = '{8F2A6C1E-4B3D-4F7A-9A21-5C6D7E8F9A0B}'
$target64 = Join-Path $PF "inklink_tsf_$version.dll"
$target32 = Join-Path $PF "inklink_tsf32_$version.dll"

New-Item -ItemType Directory -Force $PF | Out-Null
Copy-Item $dll64 $target64 -Force
Copy-Item $dll32 $target32 -Force

Set-ItemProperty "HKLM:\SOFTWARE\Classes\CLSID\$CLSID\InprocServer32" -Name '(default)' -Value $target64
Set-ItemProperty "HKLM:\SOFTWARE\Classes\Wow6432Node\CLSID\$CLSID\InprocServer32" -Name '(default)' -Value $target32

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