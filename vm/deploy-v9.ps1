# inklink TSF deploy v9 - uppercase GUIDs, propagate enable error
# Run as Administrator: powershell -ExecutionPolicy Bypass -File .\deploy-v9.ps1

$ErrorActionPreference = 'Continue'
$clsid   = '{8F2A6C1E-4B3D-4F7A-9A21-5C6D7E8F9A0B}'
$profile = '{1B2C3D4E-5F60-4718-9A2B-3C4D5E6F7081}'
$tip     = "0804:$clsid$profile"
$dir     = 'C:\Program Files\inklink'
$dll     = Join-Path $dir 'inklink_tsf.dll'

Write-Host "== 1. copy DLL =="
New-Item -ItemType Directory -Force $dir | Out-Null
try {
  Copy-Item 'C:\Users\Public\inklink_tsf3.dll' $dll -Force -ErrorAction Stop
  Write-Host "   copied"
} catch {
  $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
  $dll = Join-Path $dir ("inklink_tsf_" + $stamp + ".dll")
  Copy-Item 'C:\Users\Public\inklink_tsf3.dll' $dll -Force -ErrorAction Stop
  Write-Host ("   locked, fallback: " + $dll)
}

Write-Host "== 2. register =="
Add-Type -TypeDefinition 'using System;using System.Runtime.InteropServices;public class R{[DllImport("kernel32.dll",CharSet=CharSet.Unicode)]public static extern IntPtr LoadLibraryW(string p);[DllImport("kernel32.dll",CharSet=CharSet.Ansi)]public static extern IntPtr GetProcAddress(IntPtr h,string n);[DllImport("kernel32.dll")]public static extern uint GetLastError();[UnmanagedFunctionPointer(CallingConvention.StdCall)]public delegate int F();}'
$h = [R]::LoadLibraryW($dll)
if ($h -eq [IntPtr]::Zero) { Write-Host ("   LOAD FAIL " + [R]::GetLastError()); exit 1 }
$pa = [R]::GetProcAddress($h, 'DllRegisterServer')
if ($pa -eq [IntPtr]::Zero) { Write-Host "   no export"; exit 1 }
$f = [System.Runtime.InteropServices.Marshal]::GetDelegateForFunctionPointer($pa, [R+F])
$hr = $f.Invoke()
Write-Host ("   HRESULT = 0x{0:X8}" -f $hr)
if ($hr -ne 0) { Write-Host "   register FAILED, aborting"; exit 1 }

Write-Host "== 3. ensure zh-Hans-CN language =="
$langList = Get-WinUserLanguageList
$zh = $langList | Where-Object { $_.LanguageTag -eq 'zh-Hans-CN' } | Select-Object -First 1
if ($zh -eq $null) {
  Write-Host "   no Chinese language, adding..."
  $zhNew = New-WinUserLanguageList 'zh-Hans-CN'
  $langList.Add($zhNew[0])
}

Write-Host "== 4. add TIP to zh-Hans-CN =="
$zh = $langList | Where-Object { $_.LanguageTag -eq 'zh-Hans-CN' } | Select-Object -First 1
if ($zh -ne $null) {
  if ($zh.InputMethodTips -notcontains $tip) { $zh.InputMethodTips.Add($tip) }
  Set-WinUserLanguageList -LanguageList $langList -Force
}

Write-Host "== 5. verify =="
Start-Sleep -Seconds 2
(Get-WinUserLanguageList) | ForEach-Object {
  Write-Host ("   lang " + $_.LanguageTag)
  $_.InputMethodTips | ForEach-Object { Write-Host ("      " + $_) }
}
if (((Get-WinUserLanguageList) | ForEach-Object { $_.InputMethodTips }) -contains $tip) {
  Write-Host "RESULT: OK in list"
} else {
  Write-Host "RESULT: still not in list"
}
Write-Host "done"
