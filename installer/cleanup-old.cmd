@echo off
rem inklink old-version cleanup (2.2.4 and earlier had no uninstall entry)
rem ASCII only: cmd.exe reads this file in the OEM code page.
setlocal
title inklink old-version cleanup

fltmc >nul 2>&1
if errorlevel 1 (
  echo ==========================================================
  echo   Administrator rights required.
  echo ==========================================================
  echo   Asking for UAC elevation now. Please click "Yes".
  echo   If nothing happens, right-click this file and pick
  echo   "Run as administrator".
  echo.
  powershell -NoProfile -ExecutionPolicy Bypass -Command "try { Start-Process -FilePath '%~f0' -Verb RunAs -ErrorAction Stop } catch { Write-Host 'Elevation was cancelled or blocked.' -ForegroundColor Red; Start-Sleep 3 }"
  exit /b
)

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0cleanup-old.ps1"
set "RC=%errorlevel%"

echo.
if not "%RC%"=="0" (
  echo [WARNING] Some items could not be cleaned. See the list above.
  echo           Reboot and run this tool once more.
) else (
  echo [OK] Cleanup finished. You can install the new inklink now.
)
echo.
pause
exit /b %RC%
