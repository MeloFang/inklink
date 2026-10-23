# inklink TSF 部署到测试机（Hyper-V VM 内，管理员 PowerShell 运行）
#
# 用法：先把编译产物推进 VM 的 C:\Users\Public\ 下，再跑本脚本：
#   powershell -ExecutionPolicy Bypass -File .\deploy.ps1
#
# 关键点（都是踩过的坑，见 HANDOFF.md 第四节）：
#   1. TIP 已注册（首次用 regsvr32 注册过一次），日常迭代只需改 InprocServer32 指向新 DLL
#   2. DLL 用唯一时间戳文件名，避免被已加载的旧 DLL 锁死
#   3. 引擎是独立进程，崩了要能拉起来

param(
    [string]$SourceDll = 'C:\Users\Public\inklink_tsf.dll',
    [string]$EngineExe = 'C:\inklink-engine\inklink.exe',
    [string]$EngineDir = 'C:\inklink-engine'
)

$ErrorActionPreference = 'Stop'
$clsid = '{8F2A6C1E-4B3D-4F7A-9A21-5C6D7E8F9A0B}'

# 1. 关掉记事本，让它下次加载新 DLL（DllCanUnloadNow=S_FALSE，进程不重启就还是旧 DLL）
Get-Process notepad -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 1

# 2. DLL 复制到唯一文件名
if (-not (Test-Path $SourceDll)) { throw "找不到源 DLL: $SourceDll" }
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$dll = "C:\Program Files\inklink\inklink_tsf_$stamp.dll"
Copy-Item $SourceDll $dll -Force
Write-Host "DLL -> $dll"

# 3. 注册表直接指向新 DLL（TIP 已注册，不用再走 DllRegisterServer）
Set-ItemProperty "HKLM:\SOFTWARE\Classes\CLSID\$clsid\InprocServer32" -Name '(default)' -Value $dll
Write-Host "注册表已更新"

# 4. 确保引擎进程在跑
if (-not (Get-Process inklink -ErrorAction SilentlyContinue)) {
    Start-Process -FilePath $EngineExe -ArgumentList '--serve' -WorkingDirectory $EngineDir -WindowStyle Hidden
    Start-Sleep -Seconds 4
    Write-Host "引擎已启动"
} else {
    Write-Host "引擎已在运行"
}

Write-Host "完成。关掉并重开记事本，Win+Space 切到 inklink 测试。"
