# inklink TSF deploy v18 - DLL + engine server
# Run as Administrator: powershell -ExecutionPolicy Bypass -File .\deploy-v18.ps1

$ErrorActionPreference = 'Continue'
$clsid   = '{8F2A6C1E-4B3D-4F7A-9A21-5C6D7E8F9A0B}'
$profile = '{1B2C3D4E-5F60-4718-9A2B-3C4D5E6F7081}'
$tip     = "0804:$clsid$profile"
$dir     = 'C:\Program Files\inklink'
$source  = 'C:\Users\Public\inklink_tsf11.dll'

Write-Host '== 0. source check =='
if (-not (Test-Path $source)) { Write-Host "   MISSING: $source"; exit 1 }
$src = Get-Item $source
Write-Host ("   size = " + $src.Length)

Write-Host '== 1. install engine + lexicon =='
$engDir = 'C:\inklink-engine'
New-Item -ItemType Directory -Force $engDir | Out-Null
Copy-Item 'C:\Users\Public\inklink-engine.exe' (Join-Path $engDir 'inklink.exe') -Force
New-Item -ItemType Directory -Force (Join-Path $engDir 'data') | Out-Null
Copy-Item 'C:\Users\Public\base.tsv' (Join-Path $engDir 'data\base.tsv') -Force
Write-Host ("   engine = " + (Join-Path $engDir 'inklink.exe'))
Write-Host ("   lexicon = " + (Test-Path (Join-Path $engDir 'data\base.tsv')))

Write-Host '== 2. copy DLL to unique destination =='
New-Item -ItemType Directory -Force $dir | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$dll = Join-Path $dir ("inklink_tsf_" + $stamp + ".dll")
Copy-Item $source $dll -Force
$dst = Get-Item $dll
Write-Host ("   dest = " + $dst.FullName + "  size = " + $dst.Length)
if ($dst.Length -ne $src.Length) { Write-Host '   SIZE MISMATCH'; exit 1 }

Write-Host '== 3. register =='
Add-Type -TypeDefinition 'using System;using System.Runtime.InteropServices;public class R3{[DllImport("kernel32.dll",CharSet=CharSet.Unicode)]public static extern IntPtr LoadLibraryW(string p);[DllImport("kernel32.dll",CharSet=CharSet.Ansi)]public static extern IntPtr GetProcAddress(IntPtr h,string n);[UnmanagedFunctionPointer(CallingConvention.StdCall)]public delegate int F();}'
$h = [R3]::LoadLibraryW($dll)
if ($h -eq [IntPtr]::Zero) { Write-Host '   LOAD FAIL'; exit 1 }
$pa = [R3]::GetProcAddress($h, 'DllRegisterServer')
$f = [System.Runtime.InteropServices.Marshal]::GetDelegateForFunctionPointer($pa, [R3+F])
$hr = $f.Invoke()
Write-Host ("   HRESULT = 0x{0:X8}" -f $hr)
if ($hr -ne 0) { exit 1 }

Write-Host '== 4. language list =='
$langList = Get-WinUserLanguageList
$zh = $langList | Where-Object { $_.LanguageTag -eq 'zh-Hans-CN' } | Select-Object -First 1
if ($zh -eq $null) {
    $zhNew = New-WinUserLanguageList 'zh-Hans-CN'
    $langList.Add($zhNew[0])
    $zh = $langList | Where-Object { $_.LanguageTag -eq 'zh-Hans-CN' } | Select-Object -First 1
}
if ($zh -and ($zh.InputMethodTips -notcontains $tip)) { $zh.InputMethodTips.Add($tip) }
Set-WinUserLanguageList -LanguageList $langList -Force

Write-Host '== 5. start engine server =='
$running = Get-Process inklink -ErrorAction SilentlyContinue
if ($running) { Write-Host '   already running'; $running | Stop-Process -Force; Start-Sleep 1 }
$engExe = Join-Path $engDir 'inklink.exe'
Start-Process -FilePath $engExe -ArgumentList '--serve' -WorkingDirectory $engDir -WindowStyle Hidden
Start-Sleep -Seconds 4
$p2 = Get-Process inklink -ErrorAction SilentlyContinue
if ($p2) { Write-Host ('   engine running, pid = ' + $p2.Id) } else { Write-Host '   ENGINE FAILED TO START' }

Write-Host ''
Write-Host '部署完成。测试步骤：'
Write-Host '  1) 关闭并重新打开记事本'
Write-Host '  2) Win+Space 切到 inklink'
Write-Host '  3) 输入 xiangmu 然后按空格'
Write-Host '  4) 应该出现「项目」'
