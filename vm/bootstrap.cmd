@echo off
chcp 65001 >nul
echo === inklink TSF 测试环境准备 ===
echo.

echo [1/2] 创建本地测试账号 inklink
net user inklink "Ink!2026test" /add >nul 2>&1
if errorlevel 1 (
  echo     账号已存在，跳过
) else (
  echo     已创建
)

echo [2/2] 加入管理员组
net localgroup administrators inklink /add >nul 2>&1
echo     完成

echo.
echo 准备完毕。现在可以在宿主机上用 PowerShell Direct 控制这台虚拟机。
echo 测试时请只在本虚拟机里启用输入法，验证完立刻用 uninstall 脚本卸载。
echo.
pause
