# inklink 输入法安装脚本
# 用法：右键「以管理员身份运行」install.cmd，或管理员 PowerShell 里执行本脚本。

$ErrorActionPreference = 'Stop'

$CLSID   = '{8F2A6C1E-4B3D-4F7A-9A21-5C6D7E8F9A0B}'
$PROFILE = '{1B2C3D4E-5F60-4718-9A2B-3C4D5E6F7081}'
$TIPCAT  = '{34745C63-B2F0-4784-8B67-5E12C8701A31}'
$PF      = 'C:\Program Files\inklink'
$ENG     = 'C:\inklink-engine'
$SRC     = Join-Path $PSScriptRoot 'files'

function Fail($msg) {
    Write-Host "安装失败：$msg" -ForegroundColor Red
    exit 1
}

# 文件被占用时改用带时间戳的新名字，保证可重复安装
function Install-File($src, $dst) {
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

# 0. 管理员检查
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) { Fail '需要管理员权限，请右键以管理员身份运行' }
if (-not (Test-Path $SRC)) { Fail "找不到 files 目录：$SRC" }

Write-Host '== inklink 输入法安装 ==' -ForegroundColor Cyan

# 先停引擎，避免 exe 被占用
Get-Process rime-server -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 1

# 1. 复制文件
Write-Host '[1/7] 复制文件...'
New-Item -ItemType Directory -Force $PF  | Out-Null
New-Item -ItemType Directory -Force $ENG | Out-Null
$dll64 = Install-File "$SRC\inklink_tsf.dll"   "$PF\inklink_tsf.dll"
$dll32 = Install-File "$SRC\inklink_tsf32.dll" "$PF\inklink_tsf32.dll"
Install-File "$SRC\rime-server.exe" "$ENG\rime-server.exe" | Out-Null
Install-File "$SRC\rime.dll"        "$ENG\rime.dll"        | Out-Null
if (Test-Path "$SRC\rime\data") {
    New-Item -ItemType Directory -Force "$ENG\rime\data" | Out-Null
    Copy-Item "$SRC\rime\data\*" "$ENG\rime\data" -Recurse -Force
}
if (Test-Path "$SRC\rime\user") {
    New-Item -ItemType Directory -Force "$ENG\rime\user" | Out-Null
    Copy-Item "$SRC\rime\user\*" "$ENG\rime\user" -Recurse -Force
}

# 2. COM 注册：64 位与 32 位各一份（WPS 等 32 位应用需要 32 位 DLL）
Write-Host '[2/7] 注册 COM 组件（64 位 + 32 位）...'
$com64 = "HKLM:\SOFTWARE\Classes\CLSID\$CLSID\InprocServer32"
New-Item -Path $com64 -Force | Out-Null
Set-ItemProperty $com64 -Name '(default)'       -Value $dll64
Set-ItemProperty $com64 -Name 'ThreadingModel'  -Value 'Apartment'

$com32 = "HKLM:\SOFTWARE\Classes\Wow6432Node\CLSID\$CLSID\InprocServer32"
New-Item -Path $com32 -Force | Out-Null
Set-ItemProperty $com32 -Name '(default)'       -Value $dll32
Set-ItemProperty $com32 -Name 'ThreadingModel'  -Value 'Apartment'

# 3. TSF TIP 注册（64 位与 32 位各一份）
Write-Host '[3/7] 注册 TSF 输入法...'
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

# 4. 引擎路径（DLL 找不到引擎时按这个路径自动拉起）
Write-Host '[4/7] 配置引擎路径...'
New-Item -Path 'HKLM:\SOFTWARE\inklink' -Force | Out-Null
Set-ItemProperty 'HKLM:\SOFTWARE\inklink' -Name 'EnginePath' -Value "$ENG\rime-server.exe"

# 5. 开机自启引擎（词库加载约 2-3 秒，提前加载好）
Write-Host '[5/7] 设置开机自启...'
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
Set-ItemProperty $run -Name 'inklink-engine' -Value "`"$ENG\rime-server.exe`""

# 6. 加入输入法列表
Write-Host '[6/7] 添加到输入法列表...'
$tipStr = "0804:$CLSID$PROFILE"
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

# 7. 启动引擎
Write-Host '[7/7] 启动引擎...'
Start-Process "$ENG\rime-server.exe" -WorkingDirectory $ENG -WindowStyle Hidden
Start-Sleep -Seconds 3

Write-Host ''
Write-Host '安装完成！' -ForegroundColor Green
Write-Host ''
Write-Host '使用方法：' -ForegroundColor Yellow
Write-Host '  1. 按 Win+Space 切换到「inklink 输入法」'
Write-Host '  2. 直接打拼音，例如 xiangmu -> 项目'
Write-Host '  3. 空格=选第一个，数字键=选对应候选，Esc=取消'
Write-Host '  4. Shift / CapsLock / 回车 = 拼音原样上屏成英文'
Write-Host '  5. \ 打出顿号「、」，, . ; / 自动转中文标点'
Write-Host ''
Write-Host '注意：' -ForegroundColor Yellow
Write-Host '  - 刚装完第一次打字可能没反应，等 2-3 秒再打一次（引擎在加载词库）'
Write-Host '  - 已经在运行的软件需要重启一次才能用上（DLL 已被加载，不会热替换）'
