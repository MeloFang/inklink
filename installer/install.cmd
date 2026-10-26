@echo off
rem inklink input method - installer launcher
rem ASCII only: cmd.exe reads this file in the OEM code page, non-ASCII breaks it.
setlocal
title inklink installer
set "LOG=%PUBLIC%\inklink-install.log"

>>"%LOG%" echo.
>>"%LOG%" echo ================== %DATE% %TIME% ==================

echo ==========================================
echo   inklink input method - installer
echo ==========================================
echo   log file: %LOG%
echo.

rem fltmc needs admin; more reliable than "net session" (the Server service
rem may be disabled on client Windows, which made the old check loop forever).
fltmc >nul 2>&1
if errorlevel 1 goto needs_admin

powershell -NoProfile -ExecutionPolicy Bypass -Command "$ErrorActionPreference='Stop'; try { Start-Transcript -Path '%LOG%' -Append | Out-Null } catch {}; $code=0; try { & '%~dp0install.ps1' } catch { $code=1; Write-Host ''; Write-Host ('!!! INSTALL FAILED: ' + $_.Exception.Message) -ForegroundColor Red; if ($_.ScriptStackTrace) { Write-Host $_.ScriptStackTrace -ForegroundColor DarkGray } }; try { Stop-Transcript | Out-Null } catch {}; exit $code"
set "RC=%errorlevel%"

echo.
if not "%RC%"=="0" (
    echo [FAILED] exit code %RC%
    echo Send this file to the developer: %LOG%
) else (
    echo [OK] log saved to: %LOG%
)
echo.
pause
exit /b %RC%

:needs_admin
echo Administrator rights required. Asking for UAC elevation...
echo.
powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
echo.
echo If no UAC prompt appeared, right-click install.cmd and pick "Run as administrator".
echo.
pause
exit /b
