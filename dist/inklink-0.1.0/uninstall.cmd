@echo off
rem inklink input method - uninstaller launcher
rem ASCII only: cmd.exe reads this file in the OEM code page, non-ASCII breaks it.
setlocal
title inklink uninstaller
set "LOG=%PUBLIC%\inklink-uninstall.log"

>>"%LOG%" echo.
>>"%LOG%" echo ================== %DATE% %TIME% ==================

echo ==========================================
echo   inklink input method - uninstaller
echo ==========================================
echo   log file: %LOG%
echo.

fltmc >nul 2>&1
if errorlevel 1 goto needs_admin

powershell -NoProfile -ExecutionPolicy Bypass -Command "$ErrorActionPreference='Continue'; try { Start-Transcript -Path '%LOG%' -Append | Out-Null } catch {}; $code=0; try { & '%~dp0uninstall.ps1' } catch { $code=1; Write-Host ('!!! UNINSTALL FAILED: ' + $_.Exception.Message) -ForegroundColor Red; if ($_.ScriptStackTrace) { Write-Host $_.ScriptStackTrace -ForegroundColor DarkGray } }; try { Stop-Transcript | Out-Null } catch {}; exit $code"
set "RC=%errorlevel%"

echo.
echo [done] log saved to: %LOG%
echo.
pause
exit /b %RC%

:needs_admin
echo Administrator rights required. Asking for UAC elevation...
echo.
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
echo.
echo If no UAC prompt appeared, right-click uninstall.cmd and pick "Run as administrator".
echo.
pause
exit /b
