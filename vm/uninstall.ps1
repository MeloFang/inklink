# 在虚拟机里卸载 inklink TSF 输入法
# 用法（管理员 PowerShell）：powershell -ExecutionPolicy Bypass -File .\uninstall.ps1

$ErrorActionPreference = 'Continue'
$clsid   = '{8f2a6c1e-4b3d-4f7a-9a21-5c6d7e8f9a0b}'
$profile = '{1b2c3d4e-5f60-4718-9a2b-3c4d5e6f7081}'
$tip     = "0804:$clsid$profile"
$dll     = 'C:\Program Files\inklink\inklink_tsf.dll'

Write-Host '== 1. 从输入法列表移除 =='
$list = Get-WinUserLanguageList
if ($list[0].InputMethodTips -contains $tip) {
  $list[0].InputMethodTips.Remove($tip) | Out-Null
  Set-WinUserLanguageList -LanguageList $list -Force
  Write-Host '   已移除'
} else {
  Write-Host '   列表里没有，跳过'
}

Write-Host '== 2. regsvr32 /u 反注册 =='
if (Test-Path $dll) {
  $p = Start-Process -FilePath 'regsvr32.exe' -ArgumentList @('/s', '/u', $dll) -PassThru -Wait
  Write-Host "   退出码: $($p.ExitCode)"
}

Write-Host '== 3. 清理注册表残留 =='
foreach ($k in @("HKLM:\SOFTWARE\Classes\CLSID\$clsid", "HKLM:\SOFTWARE\Microsoft\CTF\TIP\$clsid")) {
  if (Test-Path $k) { Remove-Item $k -Recurse -Force; Write-Host "   已删除 $k" }
}

Write-Host ''
Write-Host '已卸载。若 DLL 仍被占用，重启虚拟机即可。'
