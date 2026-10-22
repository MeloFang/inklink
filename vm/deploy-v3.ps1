# 在虚拟机里部署 inklink TSF 输入法（T1：能被系统识别并出现在列表里）
# 用法（管理员 PowerShell）：powershell -ExecutionPolicy Bypass -File .\deploy.ps1
#
# 为什么用 regsvr32 而不是内联 C#：PowerShell 的 here-string 解析会吃掉 C# 代码，
# 而且 regsvr32 走的就是标准注册路径，行为最接近用户实际安装。

$ErrorActionPreference = 'Stop'
$clsid   = '{8f2a6c1e-4b3d-4f7a-9a21-5c6d7e8f9a0b}'
$profile = '{1b2c3d4e-5f60-4718-9a2b-3c4d5e6f7081}'
$tip     = "0804:$clsid$profile"
$dir     = 'C:\Program Files\inklink'
$dll     = Join-Path $dir 'inklink_tsf.dll'

Write-Host '== 1. 复制 DLL =='
New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item 'C:\Users\Public\inklink_tsf.dll' $dll -Force
Write-Host "   $dll"

Write-Host '== 2. regsvr32 注册 =='
$p = Start-Process -FilePath 'regsvr32.exe' -ArgumentList @('/s', $dll) -PassThru -Wait
Write-Host "   退出码: $($p.ExitCode)"
if ($p.ExitCode -ne 0) { throw "regsvr32 失败，退出码 $($p.ExitCode)" }

Write-Host '== 3. 加入当前用户的输入法列表 =='
$list = Get-WinUserLanguageList
if ($list[0].InputMethodTips -notcontains $tip) {
  $list[0].InputMethodTips.Add($tip)
  Set-WinUserLanguageList -LanguageList $list -Force
}

Write-Host '== 4. 验证 =='
Start-Sleep -Seconds 1
$now = (Get-WinUserLanguageList)[0].InputMethodTips
if ($now -contains $tip) {
  Write-Host '   OK 已出现在输入法列表' -ForegroundColor Green
} else {
  Write-Host '   FAIL 未出现在列表，系统没接受这个 TIP' -ForegroundColor Red
  Write-Host '   当前列表：'
  $now | ForEach-Object { Write-Host "     $_" }
}

Write-Host ''
Write-Host '接着按 Win+Space，看列表里有没有「inklink 输入法」。'
Write-Host '卸载：powershell -ExecutionPolicy Bypass -File .\uninstall.ps1'
