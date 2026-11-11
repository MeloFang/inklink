@echo off
rem inklink old-version cleanup (2.2.3 and earlier had no uninstall entry)
rem ASCII only: cmd.exe reads this file in the OEM code page.
setlocal
title inklink old-version cleanup

fltmc >nul 2>&1
if errorlevel 1 (
  echo Administrator rights required. Asking for UAC elevation...
  powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
  exit /b
)

powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0cleanup-old.ps1"
echo.
pause
exit /b
