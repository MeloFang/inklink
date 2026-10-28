# inklink 输入法卸载脚本
# 用法：右键「以管理员身份运行」uninstall.cmd

$ErrorActionPreference = 'Continue'

$CLSID   = '{8F2A6C1E-4B3D-4F7A-9A21-5C6D7E8F9A0B}'
$PROFILE = '{1B2C3D4E-5F60-4718-9A2B-3C4D5E6F7081}'
$PF      = 'C:\Program Files\inklink'
$ENG     = 'C:\inklink-engine'

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Host '需要管理员权限，请右键以管理员身份运行' -ForegroundColor Red
    exit 1
}

Write-Host '== inklink 输入法卸载 ==' -ForegroundColor Cyan

# 1. 从输入法列表移除
Write-Host '[1/5] 从输入法列表移除...'
$tipStr = "0804:$CLSID$PROFILE"
try {
    $list = Get-WinUserLanguageList
    $changed = $false
    foreach ($lang in $list) {
        while ($lang.InputMethodTips -contains $tipStr) {
            $lang.InputMethodTips.Remove($tipStr)
            $changed = $true
        }
    }
    if ($changed) { Set-WinUserLanguageList -LanguageList $list -Force }
} catch { Write-Host "  跳过：$_" -ForegroundColor DarkGray }

# 2. 停引擎和浮条
Write-Host '[2/5] 停止引擎和浮条...'
Get-Process rime-server -ErrorAction SilentlyContinue | Stop-Process -Force
Get-Process inklink-ui  -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 1

# 3. 清注册表
Write-Host '[3/5] 清理注册表...'
foreach ($k in @(
    "HKLM:\SOFTWARE\Microsoft\CTF\TIP\$CLSID",
    "HKLM:\SOFTWARE\Wow6432Node\Microsoft\CTF\TIP\$CLSID",
    "HKLM:\SOFTWARE\Classes\CLSID\$CLSID",
    "HKLM:\SOFTWARE\Classes\Wow6432Node\CLSID\$CLSID",
    "HKLM:\SOFTWARE\inklink"
)) {
    Remove-Item $k -Recurse -Force -ErrorAction SilentlyContinue
}
Remove-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'inklink-engine' -ErrorAction SilentlyContinue
Remove-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'inklink-ui' -ErrorAction SilentlyContinue

# 4. 删文件
Write-Host '[4/5] 删除文件...'
Remove-Item $PF  -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item $ENG -Recurse -Force -ErrorAction SilentlyContinue

# 5. 完成
Write-Host '[5/5] 完成'
Write-Host ''
Write-Host '卸载完成。' -ForegroundColor Green
Write-Host '已经在运行的软件需要重启一次才会彻底释放 DLL。'
Write-Host '如果输入法列表里还残留，重启一次电脑即可。'
