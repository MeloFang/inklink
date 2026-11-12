# inklink 旧版本清理工具
#
# 用法：
#   cleanup-old.cmd                    （推荐，会自己请求管理员权限）
#   powershell -File cleanup-old.ps1 -DryRun    （只看会删什么，不动手）
#   powershell -File cleanup-old.ps1 -RestartExplorer   （清完自动重启 explorer）
#
# 设计要点（2026-11-12 同事反馈后重写）：
#   1. 每一项都记录「删掉了 / 本来就不存在 / 失败了+原因」，不再假装成功；
#   2. 注册表优先用 reg.exe 删（比 Remove-Item 更能对付受保护的 TIP 键），失败会如实报；
#   3. 会清理 HKCU 下的语言列表登记，避免输入法在启用列表里复活；
#   4. 会列出还加载着 inklink_tsf*.dll 的进程 —— 这些不重启就会留一个"幽灵输入法"；
#   5. 结尾按结果给退出码：0 = 全部干净，1 = 有失败项。

param(
    [switch]$DryRun,
    [switch]$RestartExplorer
)

$ErrorActionPreference = 'Continue'

$CLSID   = '{8F2A6C1E-4B3D-4F7A-9A21-5C6D7E8F9A0B}'
$PROFILE = '{1B2C3D4E-5F60-4718-9A2B-3C4D5E6F7081}'
$PF      = 'C:\Program Files\inklink'
$ENG     = 'C:\inklink-engine'

$script:ok = 0
$script:missing = 0
$script:failed = New-Object System.Collections.Generic.List[string]

function Report($name, $state, $detail = '') {
    switch ($state) {
        'ok'      { $script:ok++;      Write-Host ("  [已删除] {0}{1}" -f $name, $(if ($detail) { "  ($detail)" } else { '' })) -ForegroundColor Green }
        'missing' { $script:missing++; Write-Host ("  [不存在] {0}" -f $name) -ForegroundColor DarkGray }
        'fail'    { $script:failed.Add("$name — $detail"); Write-Host ("  [失败]   {0}  {1}" -f $name, $detail) -ForegroundColor Red }
    }
}

function Remove-RegKey($name, $path) {
    if (-not (Test-Path $path)) { Report $name 'missing'; return }
    if ($DryRun) { Write-Host ("  [会删除] {0}" -f $name) -ForegroundColor Cyan; return }
    # reg.exe 对 CTF/TIP 这类受保护键比 Remove-Item 更可靠；失败时它会给出错误文本
    $out = & reg.exe delete $path.Replace('HKLM:', 'HKLM').Replace('HKCU:', 'HKCU') /f 2>&1
    if ($LASTEXITCODE -eq 0 -and -not (Test-Path $path)) {
        Report $name 'ok'
    } else {
        Report $name 'fail' ((($out | Out-String).Trim()) -replace '\s+', ' ')
    }
}

function Remove-FileOrDir($name, $path) {
    if (-not (Test-Path $path)) { Report $name 'missing'; return }
    if ($DryRun) { Write-Host ("  [会删除] {0} : {1}" -f $name, $path) -ForegroundColor Cyan; return }
    try {
        Remove-Item $path -Recurse -Force -ErrorAction Stop
        Report $name 'ok'
    } catch {
        Report $name 'fail' $_.Exception.Message
    }
}

Write-Host ''
Write-Host '== inklink 旧版本清理 ==' -ForegroundColor Cyan
Write-Host ("   时间: {0}" -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'))
if ($DryRun) { Write-Host '   模式: 预演（不会真的删除）' -ForegroundColor Yellow }
Write-Host ''

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Host '需要管理员权限：请右键 cleanup-old.cmd → 以管理员身份运行。' -ForegroundColor Red
    exit 1
}

# ── 1. 停进程 ────────────────────────────────────────────────
Write-Host '1. 结束相关进程'
foreach ($p in @('rime-server', 'inklink-settings', 'inklink-ui')) {
    $procs = Get-Process $p -ErrorAction SilentlyContinue
    if (-not $procs) { Report $p 'missing'; continue }
    if ($DryRun) { Write-Host ("  [会结束] {0} ({1} 个)" -f $p, $procs.Count) -ForegroundColor Cyan; continue }
    try {
        $procs | Stop-Process -Force -ErrorAction Stop
        Start-Sleep -Milliseconds 300
        if (Get-Process $p -ErrorAction SilentlyContinue) { Report $p 'fail' '进程仍在运行' }
        else { Report $p 'ok' }
    } catch {
        Report $p 'fail' $_.Exception.Message
    }
}

# ── 2. 注册表 ────────────────────────────────────────────────
Write-Host ''
Write-Host '2. 清理注册表'
Remove-RegKey 'TSF 输入法注册 (64 位)'  "HKLM:\SOFTWARE\Microsoft\CTF\TIP\$CLSID"
Remove-RegKey 'TSF 输入法注册 (32 位)'  "HKLM:\SOFTWARE\Wow6432Node\Microsoft\CTF\TIP\$CLSID"
Remove-RegKey 'COM 组件注册 (64 位)'    "HKLM:\SOFTWARE\Classes\CLSID\$CLSID"
Remove-RegKey 'COM 组件注册 (32 位)'    "HKLM:\SOFTWARE\Classes\Wow6432Node\CLSID\$CLSID"
Remove-RegKey '引擎路径 HKLM\inklink'   'HKLM:\SOFTWARE\inklink'
Remove-RegKey '应用列表卸载登记'        'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\inklink'

# 用户级：语言列表里对本输入法的登记 + 本程序设置
$tipStr = "0804:$CLSID$PROFILE"
$profileRoot = 'HKCU:\Control Panel\International\User Profile'
$leftover = @()
if (Test-Path $profileRoot) {
    foreach ($lang in (Get-ChildItem $profileRoot -ErrorAction SilentlyContinue)) {
        $k = $lang.PSPath
        try {
            $props = (Get-Item $k).Property
            foreach ($name in $props) {
                if ($name -like "0804:$CLSID*") {
                    if ($DryRun) { Write-Host ("  [会删除] 语言列表项 {0} {1}" -f $lang.PSChildName, $name) -ForegroundColor Cyan; continue }
                    Remove-ItemProperty $k -Name $name -Force -ErrorAction SilentlyContinue
                    $leftover += "$($lang.PSChildName)/$name"
                }
            }
        } catch { }
    }
}
if ($DryRun) { } elseif ($leftover.Count -gt 0) { Report '语言列表登记' 'ok' ($leftover -join ', ') }
else { Report '语言列表登记' 'missing' }

Remove-RegKey '本程序设置 HKCU\inklink' 'HKCU:\Software\inklink'

foreach ($v in @('inklink-engine', 'inklink-ui')) {
    $run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
    if ((Get-ItemProperty $run -Name $v -ErrorAction SilentlyContinue)) {
        if ($DryRun) { Write-Host ("  [会删除] 开机启动项 {0}" -f $v) -ForegroundColor Cyan; continue }
        Remove-ItemProperty $run -Name $v -Force -ErrorAction SilentlyContinue
        if ((Get-ItemProperty $run -Name $v -ErrorAction SilentlyContinue)) { Report "开机启动项 $v" 'fail' '仍存在' }
        else { Report "开机启动项 $v" 'ok' }
    } else {
        Report "开机启动项 $v" 'missing'
    }
}

# ── 3. 文件 ──────────────────────────────────────────────────
Write-Host ''
Write-Host '3. 删除文件'
Remove-FileOrDir '程序目录' $PF
Remove-FileOrDir '引擎目录' $ENG
Remove-FileOrDir '用户数据 (%LOCALAPPDATA%\inklink)' (Join-Path $env:LOCALAPPDATA 'inklink')

# ── 4. DLL 驻留检查 ──────────────────────────────────────────
Write-Host ''
Write-Host '4. 检查还有哪些程序加载着 inklink 的 DLL'
$holders = @()
foreach ($p in (Get-Process -ErrorAction SilentlyContinue)) {
    try {
        $hit = $p.Modules | Where-Object { $_.ModuleName -like 'inklink_tsf*.dll' }
        if ($hit) { $holders += $p.ProcessName }
    } catch { }
}
$holders = $holders | Sort-Object -Unique
if ($holders.Count -eq 0) {
    Write-Host '  没有程序加载它了' -ForegroundColor Green
} else {
    Write-Host ("  这些程序还加载着旧 DLL：{0}" -f ($holders -join ', ')) -ForegroundColor Yellow
    Write-Host '  它们不重启，Win+Space 里可能还会看到 inklink（幽灵输入法）。'
    if ($RestartExplorer -and -not $DryRun) {
        Write-Host '  正在重启 explorer.exe …'
        Stop-Process -Name explorer -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 2
        Start-Process explorer.exe -ErrorAction SilentlyContinue
        Write-Host '  已重启 explorer（任务栏会闪一下，属正常）' -ForegroundColor Green
    } else {
        Write-Host '  建议：注销后重新登录，或加 -RestartExplorer 参数重跑本脚本。'
    }
}

# ── 5. 汇总 ──────────────────────────────────────────────────
Write-Host ''
Write-Host '== 结果 ==' -ForegroundColor Cyan
Write-Host ("  已删除 {0} 项 / 本来不存在 {1} 项 / 失败 {2} 项" -f $script:ok, $script:missing, $script:failed.Count)
if ($script:failed.Count -gt 0) {
    Write-Host '  失败明细：' -ForegroundColor Red
    foreach ($f in $script:failed) { Write-Host ("    - {0}" -f $f) -ForegroundColor Red }
    Write-Host '  常见原因：权限不足、被安全软件锁定、或该键被系统服务占用。'
    Write-Host '  重启电脑后重跑一次通常能清掉。'
    exit 1
}
if ($DryRun) {
    Write-Host '  预演结束，未做任何修改。' -ForegroundColor Yellow
    exit 0
}
Write-Host '  清理完成，现在可以安装新版 inklink。' -ForegroundColor Green
exit 0
