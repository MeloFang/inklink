# inklink TSF diagnostics - one shot. Run as Administrator.
$ErrorActionPreference = 'Continue'
$clsid = '{8F2A6C1E-4B3D-4F7A-9A21-5C6D7E8F9A0B}'
$key = "HKLM:\SOFTWARE\Classes\CLSID\$clsid\InprocServer32"

Write-Host "=== 1. registered InprocServer32 ==="
if (Test-Path $key) {
    $p = (Get-ItemProperty $key).'(default)'
    Write-Host ("   path = " + $p)
    Write-Host ("   exists = " + (Test-Path $p))
    if (Test-Path $p) {
        $f = Get-Item $p
        Write-Host ("   size = " + $f.Length + "  mtime = " + $f.LastWriteTime)
    }
} else {
    Write-Host "   KEY MISSING - not registered!"
}

Write-Host "=== 2. files in Program Files\inklink ==="
if (Test-Path 'C:\Program Files\inklink') {
    Get-ChildItem 'C:\Program Files\inklink' -File | ForEach-Object {
        Write-Host ("   " + $_.Name + "  " + $_.Length + "  " + $_.LastWriteTime)
    }
} else {
    Write-Host "   directory missing"
}

Write-Host "=== 3. CTF TIP registry ==="
$tip = "HKLM:\SOFTWARE\Microsoft\CTF\TIP\$clsid"
Write-Host ("   TIP key exists = " + (Test-Path $tip))
if (Test-Path $tip) {
    Get-ChildItem $tip -Recurse | ForEach-Object { Write-Host ("   " + $_.Name.Replace('HKEY_LOCAL_MACHINE','HKLM')) }
}

Write-Host "=== 4. clear old logs and run probe (this loads the DLL) ==="
Remove-Item 'C:\Users\Public\inklink-tsf-*.log' -Force -ErrorAction SilentlyContinue
& 'C:\Users\Public\probe.exe'
Start-Sleep -Seconds 1
$logs = Get-ChildItem 'C:\Users\Public\inklink-tsf-*.log' -ErrorAction SilentlyContinue
if ($logs) {
    foreach ($l in $logs) {
        Write-Host ("=== log " + $l.Name + " ===")
        Get-Content $l.FullName | ForEach-Object { Write-Host ("   " + $_) }
    }
} else {
    Write-Host "   NO LOG created by probe -> DLL never executed code"
}

Write-Host "=== 5. language list ==="
(Get-WinUserLanguageList) | ForEach-Object {
    Write-Host ("   lang " + $_.LanguageTag)
    $_.InputMethodTips | ForEach-Object { Write-Host ("      " + $_) }
}
Write-Host "done"
