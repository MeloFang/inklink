# inklink 旧版本清理工具
# 适用于 2.2.3 及更早版本：这些版本没有应用列表卸载登记。
$ErrorActionPreference = 'Continue'
$CLSID='{8F2A6C1E-4B3D-4F7A-9A21-5C6D7E8F9A0B}'
Write-Host '正在清理旧版 inklink...' -ForegroundColor Cyan
Get-Process rime-server -ErrorAction SilentlyContinue | Stop-Process -Force
Get-Process inklink-ui -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 800
foreach ($k in @(
  "HKLM:\SOFTWARE\Microsoft\CTF\TIP\$CLSID",
  "HKLM:\SOFTWARE\Wow6432Node\Microsoft\CTF\TIP\$CLSID",
  "HKLM:\SOFTWARE\Classes\CLSID\$CLSID",
  "HKLM:\SOFTWARE\Classes\Wow6432Node\CLSID\$CLSID",
  'HKLM:\SOFTWARE\inklink',
  'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\inklink'
)) { Remove-Item $k -Recurse -Force -ErrorAction SilentlyContinue }
Remove-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'inklink-engine' -ErrorAction SilentlyContinue
Remove-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'inklink-ui' -ErrorAction SilentlyContinue
Remove-Item 'C:\Program Files\inklink' -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item 'C:\inklink-engine' -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item (Join-Path $env:LOCALAPPDATA 'inklink') -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item 'HKCU:\Software\inklink' -Recurse -Force -ErrorAction SilentlyContinue
Write-Host '旧版 inklink 已清理。现在可以安装 2.2.5。' -ForegroundColor Green
Write-Host '如果仍有文件被占用，请重启 Windows 后再运行一次。'
