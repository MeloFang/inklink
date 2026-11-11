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
# VC++ 运行库（x86）放在引擎同目录：干净的 Windows 没装可再发行组件时，
# rime-server.exe 会因为缺 msvcp140.dll 直接起不来（候选永远为空）。
foreach ($dll in @('msvcp140.dll', 'msvcp140_1.dll', 'msvcp140_2.dll', 'vcruntime140.dll', 'vcruntime140_1.dll', 'concrt140.dll')) {
    if (Test-Path "$SRC\$dll") { Install-File "$SRC\$dll" "$ENG\$dll" | Out-Null }
}
# 英文补全词表（1.1.46）：引擎按 exe 同目录找它，漏了就是"打 gith 不补 github"
if (Test-Path "$SRC\en-words.txt") { Install-File "$SRC\en-words.txt" "$ENG\en-words.txt" | Out-Null }
# 设置窗（P2a/P4b）：浮条 ☰ 的「设置…」按 DLL 同目录找它。漏装的话菜单点了没反应。
if (Test-Path "$SRC\inklink-settings.exe") { Install-File "$SRC\inklink-settings.exe" "$PF\inklink-settings.exe" | Out-Null }
# 随包字体（朱雀仿宋，OFL）：DLL 按 <DLL 目录>\fonts 私有加载
if (Test-Path "$SRC\fonts") {
    New-Item -ItemType Directory -Force "$PF\fonts" | Out-Null
    Copy-Item "$SRC\fonts\*" "$PF\fonts" -Recurse -Force
}

# rime 数据必须落到 $ENG\rime\data（1.1.37 的包把内容拍平到了 rime\ 下，装完引擎没词库）。
# 这里显式建目录再拷内容，两种包结构都能装。
if (Test-Path "$SRC\rime\data") {
    New-Item -ItemType Directory -Force "$ENG\rime\data" | Out-Null
    Copy-Item "$SRC\rime\data\*" "$ENG\rime\data" -Recurse -Force
}
if (Test-Path "$SRC\rime\user") {
    New-Item -ItemType Directory -Force "$ENG\rime\user" | Out-Null
    Copy-Item "$SRC\rime\user\*" "$ENG\rime\user" -Recurse -Force
}
# 注意：这里**不要**再 Copy-Item "$SRC\rime\data" "$ENG\rime\"。
# 目标目录已存在时那会把内容再套一层，产出 rime\data\data —— 引擎读到空目录，
# 表现就是"打什么都是零候选"（2026-11-11 同事机器）。
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
    # 语言列表里那一行的小图标。这里指向**单独的 .ico**，不是 DLL——
    # DLL 里没有编图标资源（build.rs 只写版本号），指过去等于没有，
    # 系统就退回显示「简体」两个字。用 .ico 还有个好处：DLL 换版本名时它不会失效。
    Set-ItemProperty $lp -Name 'IconFile'   -Value (Join-Path $PF 'inklink.ico')
    Set-ItemProperty $lp -Name 'IconIndex'  -Value 0 -Type DWord
}

# ── 4. 引擎路径 + 卸载登记 ─────────────────────────────────────
Write-Host ''
Step 4 '配置引擎路径、登记卸载入口...'
New-Item -Path 'HKLM:\SOFTWARE\inklink' -Force | Out-Null
Set-ItemProperty 'HKLM:\SOFTWARE\inklink' -Name 'EnginePath' -Value "$ENG\rime-server.exe"

# 应用列表（设置 → 应用 → 已安装的应用）里要能看到、能卸载。
# 之前没写这段，同事装完根本找不到卸载入口（2026-11-11 反馈）。
$unKey = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\inklink'
New-Item -Path $unKey -Force | Out-Null
Set-ItemProperty $unKey -Name 'DisplayName'     -Value 'inklink 输入法'
Set-ItemProperty $unKey -Name 'DisplayVersion'  -Value $Version
Set-ItemProperty $unKey -Name 'Publisher'       -Value 'inklink (open source, GPL-3.0)'
Set-ItemProperty $unKey -Name 'DisplayIcon'     -Value "$PF\inklink.ico"
Set-ItemProperty $unKey -Name 'InstallLocation' -Value $PF
Set-ItemProperty $unKey -Name 'UninstallString' -Value "powershell -NoProfile -ExecutionPolicy Bypass -File `"$PF\uninstall.ps1`""
Set-ItemProperty $unKey -Name 'QuietUninstallString' -Value "powershell -NoProfile -ExecutionPolicy Bypass -File `"$PF\uninstall.ps1`" -Quiet"
Set-ItemProperty $unKey -Name 'NoModify' -Value 1 -Type DWord
Set-ItemProperty $unKey -Name 'NoRepair' -Value 1 -Type DWord
$sizeKb = 0
foreach ($d in @($PF, $ENG)) {
    if (Test-Path $d) { $sizeKb += [int](((Get-ChildItem $d -Recurse -File -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum) / 1KB) }
}
Set-ItemProperty $unKey -Name 'EstimatedSize' -Value $sizeKb -Type DWord
Write-Host "    已登记：inklink 输入法 $Version（应用列表可卸载）"

# 把卸载器和诊断工具放到安装目录，装完也能随时自救
foreach ($f in @('uninstall.ps1', 'uninstall.cmd', 'diagnose.ps1', 'diagnose.cmd', 'install.ps1', 'install.cmd', 'README.txt')) {
    $src = Join-Path $PSScriptRoot $f
    if (Test-Path $src) { Copy-Item $src (Join-Path $PF $f) -Force }
}

# ── 5. 不设开机自启（引擎由输入法按需拉起）────────────────────
Write-Host ''
Step 5 '清理开机自启...'
# 以前在 Run 里挂 rime-server.exe：它是控制台程序，每次登录都闪一个黑框。
# 现在引擎由 DLL 在第一次打字时用 CREATE_NO_WINDOW 拉起，自启既没必要也难看。
# 这一段保留是为了把老版本留下的那条删掉。
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
Remove-ItemProperty -Path $run -Name 'inklink-engine' -ErrorAction SilentlyContinue

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
Step 7 '启动引擎并实测一次查询...'

function Probe-Engine([string]$code) {
    $c = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'inklink-engine', [System.IO.Pipes.PipeDirection]::InOut)
    try { $c.Connect(3000) } catch { return -1 }
    try {
        $bytes = [Text.Encoding]::UTF8.GetBytes($code)
        $w = New-Object System.IO.BinaryWriter($c)
        $w.Write([byte]1); $w.Write([uint32]$bytes.Length); $w.Write($bytes); $w.Write([uint16]0); $w.Flush()
        $r = New-Object System.IO.BinaryReader($c)
        return [int]$r.ReadUInt32()
    } catch { return -1 } finally { $c.Dispose() }
}

$engineOk = $false
for ($attempt = 1; $attempt -le 3; $attempt++) {
    if (-not (Get-Process rime-server -ErrorAction SilentlyContinue)) {
        Start-Process "$ENG\rime-server.exe" -WorkingDirectory $ENG -WindowStyle Hidden `
            -RedirectStandardOutput "$ENG\rime-server.stdout.log" `
            -RedirectStandardError  "$ENG\rime-server.stderr.log"
        Start-Sleep -Seconds 4   # 首次启动要编译词库，约 4 秒
    }
    $n = Probe-Engine 'nihao'
    if ($n -gt 0) {
        Write-Host "    引擎实测通过：nihao -> $n 个候选" -ForegroundColor Green
        $engineOk = $true
        break
    }
    Write-Host "    第 $attempt 次探测没拿到候选（首次可能要编译词库），再等..." -ForegroundColor Yellow
    Start-Sleep -Seconds 5
}
if (-not $engineOk) {
    Write-Host '    警告：引擎在跑但查不到候选。安装目录里有 diagnose.cmd，跑一下把报告发回。' -ForegroundColor Red
    Write-Host "    引擎日志：$ENG\rime-server.log" -ForegroundColor Yellow
}

# 浮条不再独立进程：切到 inklink 输入法时由 DLL 在宿主进程里画（搜狗那种）。

# ── 自检 ───────────────────────────────────────────────────────
Write-Host ''
Write-Host '== 安装结果自检 ==' -ForegroundColor Cyan
$checks = @(
    @{ Name = '64 位 DLL';  Ok = (Test-Path $dll64) },
    @{ Name = '32 位 DLL';  Ok = (Test-Path $dll32) },
    @{ Name = '引擎 exe';   Ok = (Test-Path "$ENG\rime-server.exe") },
    @{ Name = 'rime.dll';   Ok = (Test-Path "$ENG\rime.dll") },
    @{ Name = 'VC 运行库（x86）'; Ok = (Test-Path "$ENG\vcruntime140.dll") },
    @{ Name = 'rime 数据'; Ok = (Test-Path "$ENG\rime\data\default.yaml") },
    @{ Name = '英文词表';  Ok = (Test-Path "$ENG\en-words.txt") },
    @{ Name = '随包字体';  Ok = (Test-Path "$PF\fonts\ZhuqueFangsong-Regular.ttf") },
    @{ Name = '设置窗';    Ok = (Test-Path "$PF\inklink-settings.exe") },
    @{ Name = 'COM 64 位';  Ok = ((Get-ItemProperty $com64 -ErrorAction SilentlyContinue).'(default)' -eq $dll64) },
    @{ Name = 'COM 32 位';  Ok = ((Get-ItemProperty $com32 -ErrorAction SilentlyContinue).'(default)' -eq $dll32) },
    @{ Name = 'TIP 注册';   Ok = (Test-Path "HKLM:\SOFTWARE\Microsoft\CTF\TIP\$CLSID\LanguageProfile\0x00000804\$PROFILE") },
    @{ Name = '引擎路径';   Ok = ((Get-ItemProperty 'HKLM:\SOFTWARE\inklink' -ErrorAction SilentlyContinue).EnginePath -ne $null) }
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
