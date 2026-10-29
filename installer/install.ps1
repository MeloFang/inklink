# inklink 输入法安装脚本（带日志）
#
# 用法：右键「以管理员身份运行」install.cmd
# 日志：C:\Users\Public\inklink-install.log
#
# 任何一步失败都会抛出异常，由 install.cmd 捕获后打印堆栈并保留窗口，
# 同时整段输出都会进日志文件，方便远程排错。

$ErrorActionPreference = 'Stop'

$CLSID   = '{8F2A6C1E-4B3D-4F7A-9A21-5C6D7E8F9A0B}'
$PROFILE = '{1B2C3D4E-5F60-4718-9A2B-3C4D5E6F7081}'
$TIPCAT  = '{34745C63-B2F0-4784-8B67-5E12C8701A31}'
$PF      = 'C:\Program Files\inklink'
$ENG     = 'C:\inklink-engine'
$SRC     = Join-Path $PSScriptRoot 'files'
$LOG     = Join-Path $env:PUBLIC 'inklink-install.log'
$Version = '__INKLINK_VERSION__'

function Fail($msg) { throw "安装失败：$msg" }
function Step($n, $msg) { Write-Host "[$n/7] $msg" -ForegroundColor White }

# ── 环境信息 ───────────────────────────────────────────────────
# 这一段是远程排错的关键：系统版本、架构、权限决定后面每一处行为。
Write-Host '== inklink 输入法安装 ==' -ForegroundColor Cyan
Write-Host "  版本       : $Version"
Write-Host "  时间       : $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')"
Write-Host "  系统       : $([Environment]::OSVersion.VersionString)"
Write-Host "  架构       : $env:PROCESSOR_ARCHITECTURE"
Write-Host "  PowerShell : $($PSVersionTable.PSVersion)"
Write-Host "  安装包位置 : $PSScriptRoot"
Write-Host "  日志文件   : $LOG"

# 0. 管理员检查
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
Write-Host "  管理员权限 : $(if ($isAdmin) { '是' } else { '否' })"
if (-not $isAdmin) { Fail '需要管理员权限，请右键 install.cmd 选「以管理员身份运行」' }
if (-not (Test-Path $SRC)) { Fail "安装包不完整：找不到 $SRC（是不是直接从压缩包里双击运行的？先把 zip 解压到文件夹）" }

# 文件被占用时改用带时间戳的新名字，保证可重复安装
function Install-File($src, $dst) {
    if (-not (Test-Path $src)) { Fail "安装包缺少文件：$src" }
    try {
        Copy-Item $src $dst -Force -ErrorAction Stop
        return $dst
    } catch {
        $dir  = [IO.Path]::GetDirectoryName($dst)
        $base = [IO.Path]::GetFileNameWithoutExtension($dst)
        $ext  = [IO.Path]::GetExtension($dst)
        $alt  = Join-Path $dir ("{0}_{1}{2}" -f $base, (Get-Date -Format 'HHmmss'), $ext)
        Copy-Item $src $alt -Force
        Write-Host "    （$dst 被占用，改用 $(Split-Path $alt -Leaf)）" -ForegroundColor Yellow
        return $alt
    }
}

# ── 1. 停引擎 + 复制文件 ───────────────────────────────────────
Write-Host ''
Step 1 '停止旧引擎、复制文件...'
Get-Process rime-server -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Seconds 1

New-Item -ItemType Directory -Force $PF  | Out-Null
New-Item -ItemType Directory -Force $ENG | Out-Null
$dll64 = Install-File "$SRC\inklink_tsf.dll"   "$PF\inklink_tsf.dll"
$dll32 = Install-File "$SRC\inklink_tsf32.dll" "$PF\inklink_tsf32.dll"
# 语言列表里那一行的图标：DllRegisterServer 按 DLL 同目录的 inklink.ico 找它
if (Test-Path "$SRC\inklink.ico") { Install-File "$SRC\inklink.ico" "$PF\inklink.ico" | Out-Null }
if (Test-Path "$SRC\inklink-badge.ico") { Install-File "$SRC\inklink-badge.ico" "$PF\inklink-badge.ico" | Out-Null }
Install-File "$SRC\rime-server.exe" "$ENG\rime-server.exe" | Out-Null
Install-File "$SRC\rime.dll"        "$ENG\rime.dll"        | Out-Null
# 常驻 UI 进程（浮条 + 托盘）。它不加载进别人的进程，升级可以随时替换。
$uiDst = $null
if (Test-Path "$SRC\inklink-ui.exe") {
    if (Get-Process inklink-ui -ErrorAction SilentlyContinue) { Get-Process inklink-ui | Stop-Process -Force; Start-Sleep -Milliseconds 400 }
    $uiDst = Install-File "$SRC\inklink-ui.exe" "$PF\inklink-ui.exe"
}
if (Test-Path "$SRC\rime\data") { Copy-Item "$SRC\rime\data" "$ENG\rime\" -Recurse -Force }
if (Test-Path "$SRC\rime\user") { Copy-Item "$SRC\rime\user" "$ENG\rime\" -Recurse -Force }
Write-Host "    64 位 DLL : $dll64"
Write-Host "    32 位 DLL : $dll32"

# ── 2. COM 注册（64 位 + 32 位各一份，WPS 这类 32 位应用需要） ──
Write-Host ''
Step 2 '注册 COM 组件...'
$com64 = "HKLM:\SOFTWARE\Classes\CLSID\$CLSID\InprocServer32"
New-Item -Path $com64 -Force | Out-Null
Set-ItemProperty $com64 -Name '(default)'      -Value $dll64
Set-ItemProperty $com64 -Name 'ThreadingModel' -Value 'Apartment'

$com32 = "HKLM:\SOFTWARE\Classes\Wow6432Node\CLSID\$CLSID\InprocServer32"
New-Item -Path $com32 -Force | Out-Null
Set-ItemProperty $com32 -Name '(default)'      -Value $dll32
Set-ItemProperty $com32 -Name 'ThreadingModel' -Value 'Apartment'

# ── 3. TSF TIP 注册 ────────────────────────────────────────────
Write-Host ''
Step 3 '注册 TSF 输入法...'
foreach ($root in @('HKLM:\SOFTWARE\Microsoft\CTF\TIP', 'HKLM:\SOFTWARE\Wow6432Node\Microsoft\CTF\TIP')) {
    $tip = "$root\$CLSID"
    New-Item -Path "$tip\Category\Category\$TIPCAT\$CLSID" -Force | Out-Null
    New-Item -Path "$tip\Category\Item\$CLSID\$TIPCAT"     -Force | Out-Null
    $lp = "$tip\LanguageProfile\0x00000804\$PROFILE"
    New-Item -Path $lp -Force | Out-Null
    Set-ItemProperty $lp -Name '(default)'  -Value 'inklink 输入法'
    Set-ItemProperty $lp -Name 'Enable'     -Value 1 -Type DWord
    Set-ItemProperty $lp -Name 'IconFile'   -Value $dll64
    Set-ItemProperty $lp -Name 'IconIndex'  -Value 0 -Type DWord
}

# ── 4. 引擎路径 ────────────────────────────────────────────────
Write-Host ''
Step 4 '配置引擎路径...'
New-Item -Path 'HKLM:\SOFTWARE\inklink' -Force | Out-Null
Set-ItemProperty 'HKLM:\SOFTWARE\inklink' -Name 'EnginePath' -Value "$ENG\rime-server.exe"

# ── 5. 开机自启引擎 + 浮条 ────────────────────────────────────
Write-Host ''
Step 5 '设置开机自启...'
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
New-Item -Path $run -Force | Out-Null
Set-ItemProperty $run -Name 'inklink-engine' -Value "`"$ENG\rime-server.exe`""
if ($uiDst) { Set-ItemProperty $run -Name 'inklink-ui' -Value "`"$uiDst`"" }

# ── 6. 加入输入法列表 ──────────────────────────────────────────
# 这一段最容易在「非中文系统 / 精简系统」上失败（Windows.Globalization 模块
# 缺失、语言包没装）。失败不算致命：注册表已经写好，用户也能在设置里手动添加。
Write-Host ''
Step 6 '添加到输入法列表...'
$tipStr = "0804:$CLSID$PROFILE"
try {
    $list = Get-WinUserLanguageList
    $zh = $list | Where-Object { $_.LanguageTag -eq 'zh-Hans-CN' } | Select-Object -First 1
    if (-not $zh) {
        $newList = New-WinUserLanguageList 'zh-Hans-CN'
        foreach ($item in $newList) { $list.Add($item) }
        $zh = $list | Where-Object { $_.LanguageTag -eq 'zh-Hans-CN' } | Select-Object -First 1
    }
    if ($zh.InputMethodTips -notcontains $tipStr) {
        $zh.InputMethodTips.Add($tipStr)
        Set-WinUserLanguageList -LanguageList $list -Force
    }
    Write-Host "    已加入：$tipStr"
} catch {
    Write-Host "    警告：自动加入输入法列表失败（$($_.Exception.Message)）" -ForegroundColor Yellow
    Write-Host "    不影响安装。请手动添加：设置 → 时间和语言 → 语言和区域 → 中文(简体) → 添加键盘 → inklink 输入法" -ForegroundColor Yellow
}

# ── 7. 启动引擎 ────────────────────────────────────────────────
Write-Host ''
Step 7 '启动引擎...'
Start-Process "$ENG\rime-server.exe" -WorkingDirectory $ENG -WindowStyle Hidden
Start-Sleep -Seconds 3
$proc = Get-Process rime-server -ErrorAction SilentlyContinue
if ($proc) {
    Write-Host "    引擎已启动：pid $($proc[0].Id)" -ForegroundColor Green
} else {
    Write-Host '    警告：引擎没能启动（不影响安装，第一次打字时 DLL 会自己拉起它）' -ForegroundColor Yellow
}

# 浮条：能启动就启动，起不来不算安装失败（不影响打字）
if ($uiDst -and (Test-Path $uiDst)) {
    if (-not (Get-Process inklink-ui -ErrorAction SilentlyContinue)) {
        Start-Process $uiDst -WorkingDirectory $PF
        Start-Sleep -Seconds 1
    }
    if (Get-Process inklink-ui -ErrorAction SilentlyContinue) {
        Write-Host '    浮条已启动（右下角，可拖动）' -ForegroundColor Green
    } else {
        Write-Host '    警告：浮条没起来（不影响打字，可手动运行 inklink-ui.exe）' -ForegroundColor Yellow
    }
}

# ── 自检 ───────────────────────────────────────────────────────
Write-Host ''
Write-Host '== 安装结果自检 ==' -ForegroundColor Cyan
$checks = @(
    @{ Name = '64 位 DLL';  Ok = (Test-Path $dll64) },
    @{ Name = '32 位 DLL';  Ok = (Test-Path $dll32) },
    @{ Name = '引擎 exe';   Ok = (Test-Path "$ENG\rime-server.exe") },
    @{ Name = 'rime.dll';   Ok = (Test-Path "$ENG\rime.dll") },
    @{ Name = 'COM 64 位';  Ok = ((Get-ItemProperty $com64 -ErrorAction SilentlyContinue).'(default)' -eq $dll64) },
    @{ Name = 'COM 32 位';  Ok = ((Get-ItemProperty $com32 -ErrorAction SilentlyContinue).'(default)' -eq $dll32) },
    @{ Name = 'TIP 注册';   Ok = (Test-Path "HKLM:\SOFTWARE\Microsoft\CTF\TIP\$CLSID\LanguageProfile\0x00000804\$PROFILE") },
    @{ Name = '引擎路径';   Ok = ((Get-ItemProperty 'HKLM:\SOFTWARE\inklink' -ErrorAction SilentlyContinue).EnginePath -ne $null) },
    @{ Name = '浮条 exe';   Ok = ($uiDst -and (Test-Path $uiDst)) }
)
$bad = 0
foreach ($c in $checks) {
    if ($c.Ok) {
        Write-Host "  [OK]   $($c.Name)"
    } else {
        Write-Host "  [缺少] $($c.Name)" -ForegroundColor Red
        $bad++
    }
}

Write-Host ''
if ($bad -gt 0) {
    Fail "$bad 项自检未通过，见上面的清单和日志 $LOG"
}

Write-Host '安装完成！' -ForegroundColor Green
Write-Host ''
Write-Host '使用方法：' -ForegroundColor Yellow
Write-Host '  1. 按 Win+Space 切换到「inklink 输入法」（可能要先按一次 Ctrl+Shift）'
Write-Host '  2. 直接打拼音，例如 xiangmu -> 项目'
Write-Host '  3. 空格=选第一个，数字键 1-5=选当前那一行的第 N 个，Esc=取消'
Write-Host '  4. ↓ 或 = 展开候选（5x5），↑↓ 换行、←→ 换列，- = / PgUp PgDn 翻页'
Write-Host '  5. 拼写中按 - 是分词符（xiang-mu），Shift 单击切英文'
Write-Host ''
Write-Host '注意：' -ForegroundColor Yellow
Write-Host '  - 刚装完第一次打字可能没反应，等 2-3 秒再打一次（引擎在加载词库）'
Write-Host '  - 已经在运行的软件需要重启一次才能用上（DLL 已被加载，不会热替换）'
Write-Host "  - 日志文件：$LOG"
