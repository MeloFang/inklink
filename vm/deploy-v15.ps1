# inklink TSF deploy v15 - unique destination, size verified, direct register
# Run as Administrator: powershell -ExecutionPolicy Bypass -File .\deploy-v15.ps1

param([string]$SourceDll = 'C:\Users\Public\inklink_tsf8.dll')

$ErrorActionPreference = 'Continue'
$clsid   = '{8F2A6C1E-4B3D-4F7A-9A21-5C6D7E8F9A0B}'
$profile = '{1B2C3D4E-5F60-4718-9A2B-3C4D5E6F7081}'
$tip     = "0804:$clsid$profile"
$dir     = 'C:\Program Files\inklink'

Write-Host '== 0. source check =='
if (-not (Test-Path $SourceDll)) { Write-Host "   SOURCE MISSING: $SourceDll"; exit 1 }
$src = Get-Item $SourceDll
Write-Host ("   source = " + $src.FullName)
Write-Host ("   size   = " + $src.Length)

Write-Host '== 1. copy to unique destination =='
New-Item -ItemType Directory -Force $dir | Out-Null
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$dll = Join-Path $dir ("inklink_tsf_" + $stamp + ".dll")
Copy-Item $SourceDll $dll -Force
$dst = Get-Item $dll
Write-Host ("   dest   = " + $dst.FullName)
Write-Host ("   size   = " + $dst.Length)
if ($dst.Length -ne $src.Length) { Write-Host '   SIZE MISMATCH - abort'; exit 1 }

Write-Host '== 2. register (direct call) =='
Add-Type -TypeDefinition 'using System;using System.Runtime.InteropServices;public class R2{[DllImport("kernel32.dll",CharSet=CharSet.Unicode)]public static extern IntPtr LoadLibraryW(string p);[DllImport("kernel32.dll",CharSet=CharSet.Ansi)]public static extern IntPtr GetProcAddress(IntPtr h,string n);[UnmanagedFunctionPointer(CallingConvention.StdCall)]public delegate int F();}'
$h = [R2]::LoadLibraryW($dll)
if ($h -eq [IntPtr]::Zero) { Write-Host "   LOAD FAIL"; exit 1 }
$pa = [R2]::GetProcAddress($h, 'DllRegisterServer')
if ($pa -eq [IntPtr]::Zero) { Write-Host '   no export'; exit 1 }
$f = [System.Runtime.InteropServices.Marshal]::GetDelegateForFunctionPointer($pa, [R2+F])
$hr = $f.Invoke()
Write-Host ("   HRESULT = 0x{0:X8}" -f $hr)
if ($hr -ne 0) { Write-Host '   register FAILED'; exit 1 }

Write-Host '== 3. verify registry points to new dll =='
$reg = (Get-ItemProperty "HKLM:\SOFTWARE\Classes\CLSID\$clsid\InprocServer32").'(default)'
Write-Host ("   registered = " + $reg)

Write-Host '== 4. ensure zh-Hans-CN + TIP =='
$langList = Get-WinUserLanguageList
$zh = $langList | Where-Object { $_.LanguageTag -eq 'zh-Hans-CN' } | Select-Object -First 1
if ($zh -eq $null) {
    $zhNew = New-WinUserLanguageList 'zh-Hans-CN'
    $langList.Add($zhNew[0])
    $zh = $langList | Where-Object { $_.LanguageTag -eq 'zh-Hans-CN' } | Select-Object -First 1
}
if ($zh -and ($zh.InputMethodTips -notcontains $tip)) { $zh.InputMethodTips.Add($tip) }
Set-WinUserLanguageList -LanguageList $langList -Force

Write-Host '== 5. clear old logs, test instantiate =='
Remove-Item 'C:\Users\Public\inklink-tsf-*.log' -Force -ErrorAction SilentlyContinue
$null = [R2]::LoadLibraryW($dll)
Start-Sleep -Milliseconds 500
$logs = Get-ChildItem 'C:\Users\Public\inklink-tsf-*.log' -ErrorAction SilentlyContinue
if ($logs) {
    foreach ($l in $logs) { Get-Content $l.FullName | ForEach-Object { Write-Host ("   log: " + $_) } }
} else {
    Write-Host '   no log from DllRegisterServer (expected: logging only in DllGetClassObject)'
}
Write-Host 'done'
