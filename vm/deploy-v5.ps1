# inklink TSF deploy (self-diagnosing, ASCII-only)
# Run as Administrator: powershell -ExecutionPolicy Bypass -File .\deploy-v5.ps1

$ErrorActionPreference = 'Continue'
$clsid   = '{8f2a6c1e-4b3d-4f7a-9a21-5c6d7e8f9a0b}'
$profile = '{1b2c3d4e-5f60-4718-9a2b-3c4d5e6f7081}'
$tip     = "0804:$clsid$profile"
$dir     = 'C:\Program Files\inklink'
$dll     = Join-Path $dir 'inklink_tsf.dll'

Write-Host "== 0. process bits =="
Write-Host ("   Is64BitProcess = " + [Environment]::Is64BitProcess)

Write-Host "== 1. copy DLL =="
New-Item -ItemType Directory -Force $dir | Out-Null
try {
  Copy-Item 'C:\Users\Public\inklink_tsf2.dll' $dll -Force -ErrorAction Stop
  Write-Host "   copied"
} catch {
  $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
  $dll = Join-Path $dir ("inklink_tsf_" + $stamp + ".dll")
  Copy-Item 'C:\Users\Public\inklink_tsf2.dll' $dll -Force -ErrorAction Stop
  Write-Host ("   locked, used fallback name: " + $dll)
}

Write-Host "== 2. register (direct call) =="
Add-Type -TypeDefinition 'using System;using System.Runtime.InteropServices;public class R{[DllImport("kernel32.dll",CharSet=CharSet.Unicode)]public static extern IntPtr LoadLibraryW(string p);[DllImport("kernel32.dll",CharSet=CharSet.Ansi)]public static extern IntPtr GetProcAddress(IntPtr h,string n);[DllImport("kernel32.dll")]public static extern uint GetLastError();[UnmanagedFunctionPointer(CallingConvention.StdCall)]public delegate int F();}'
$h = [R]::LoadLibraryW($dll)
if ($h -eq [IntPtr]::Zero) {
  Write-Host ("   LOAD FAIL, GetLastError = " + [R]::GetLastError())
  exit 1
}
Write-Host ("   loaded, handle = " + $h)
$pa = [R]::GetProcAddress($h, 'DllRegisterServer')
if ($pa -eq [IntPtr]::Zero) {
  Write-Host "   export DllRegisterServer NOT FOUND"
  exit 1
}
$f = [System.Runtime.InteropServices.Marshal]::GetDelegateForFunctionPointer($pa, [R+F])
$hr = $f.Invoke()
Write-Host ("   HRESULT = 0x{0:X8}" -f $hr)

Write-Host "== 3. add to language list =="
$list = Get-WinUserLanguageList
if ($list[0].InputMethodTips -notcontains $tip) {
  $list[0].InputMethodTips.Add($tip)
  Set-WinUserLanguageList -LanguageList $list -Force
}

Write-Host "== 4. verify =="
Start-Sleep -Seconds 1
$now = (Get-WinUserLanguageList)[0].InputMethodTips
if ($now -contains $tip) {
  Write-Host "   OK in list"
} else {
  Write-Host "   NOT in list. current:"
  $now | ForEach-Object { Write-Host ("     " + $_) }
}
Write-Host "done"
