@echo off
rem inklink input method - diagnostics (read only)
rem ASCII only: cmd.exe reads this file in the OEM code page, non-ASCII breaks it.
setlocal
title inklink diagnose

echo ==========================================
echo   inklink input method - diagnostics
echo ==========================================
echo   Read only. A report will be saved on the Desktop.
echo.

powershell -NoProfile -ExecutionPolicy Bypass -Command "& '%~dp0diagnose.ps1'"
echo.
pause
exit /b
