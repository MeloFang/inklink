# 编译 x86 的 rime-server.exe（承载 librime 的那个进程）。
#
# 为什么必须 x86：Weasel 分发的 rime.dll 只有 32 位，而 TSF DLL 必须 64 位，
# 所以引擎单独一个 x86 进程，靠命名管道跟 64 位前端说话（和 Weasel 自己一个思路）。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File tools\build-engine.ps1
#   powershell -ExecutionPolicy Bypass -File tools\build-engine.ps1 -Dest C:\inklink-engine
#
# 前置：Visual Studio 2022 的 C++ 工具链（vcvarsall.bat）与 rime-server\rime-server.cc。

param(
    # 编译产物拷到哪儿；留空 = 只留在 rime-server\ 下
    [string]$Dest,
    # 显式指定 vcvarsall.bat；留空则按 VS2022/2019 的常见路径找
    [string]$VcVarsAll
)

$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
$src  = Join-Path $repo 'rime-server'

function Resolve-VcVarsAll {
    param([string]$Explicit)
    if ($Explicit) {
        if (-not (Test-Path $Explicit)) { throw "指定的 vcvarsall.bat 不存在: $Explicit" }
        return $Explicit
    }
    $roots = @(
        "$env:ProgramFiles\Microsoft Visual Studio\2022",
        "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022",
        "$env:ProgramFiles\Microsoft Visual Studio\2019",
        "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2019"
    )
    foreach ($r in $roots) {
        foreach ($ed in 'Community', 'Professional', 'Enterprise', 'BuildTools') {
            $p = Join-Path $r "$ed\VC\Auxiliary\Build\vcvarsall.bat"
            if (Test-Path $p) { return $p }
        }
    }
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $vswhere) {
        $found = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find 'VC\Auxiliary\Build\vcvarsall.bat'
        if ($found -and (Test-Path $found)) { return $found }
    }
    throw '找不到 vcvarsall.bat：需要安装 VS 2022 的 C++ 桌面开发工作负载，或用 -VcVarsAll 指定'
}

Write-Host '== 编译 rime-server.exe (x86) ==' -ForegroundColor Cyan

$cc = Join-Path $src 'rime-server.cc'
if (-not (Test-Path $cc)) { throw "找不到源码: $cc" }

$vc = Resolve-VcVarsAll -Explicit $VcVarsAll
Write-Host "  vcvarsall: $vc" -ForegroundColor DarkGray

# 用 cmd 串起来跑：vcvarsall 是批处理，必须 call 之后在同一个 cmd 进程里再调 cl，
# 否则环境变量过期（这是 PowerShell 里跑 MSVC 的老坑）。
$cmd = 'call "{0}" x86 >nul && cd /d "{1}" && cl /nologo /utf-8 /EHsc /std:c++17 /O2 /MD rime-server.cc advapi32.lib winhttp.lib user32.lib /Fe:rime-server.exe' -f $vc, $src
& cmd.exe /c $cmd
if ($LASTEXITCODE -ne 0) { throw "编译失败 (exit $LASTEXITCODE)" }

$exe = Join-Path $src 'rime-server.exe'
if (-not (Test-Path $exe)) { throw '编译没产出 rime-server.exe' }
$kb = [int]((Get-Item $exe).Length / 1KB)
Write-Host "  rime-server.exe ($kb KB)" -ForegroundColor Green

if ($Dest) {
    New-Item -ItemType Directory -Force $Dest | Out-Null
    Copy-Item $exe (Join-Path $Dest 'rime-server.exe') -Force
    Write-Host "  -> $(Join-Path $Dest 'rime-server.exe')" -ForegroundColor Green
}

Remove-Item (Join-Path $src '*.obj') -Force -ErrorAction SilentlyContinue
