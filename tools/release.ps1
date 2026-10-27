# 打发布包：dist\inklink-<版本>\ + dist\inklink-<版本>.zip
#
#   powershell -ExecutionPolicy Bypass -File tools\release.ps1
#
# 前置条件：先跑过 tools\deploy-local.ps1（或至少构建过 tsf\target-static），
# 并且 C:\inklink-engine\ 里是完整的引擎 + rime 数据。

param(
    [switch]$SkipZip
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\version.ps1"

$version = Get-InklinkVersion
$root    = Get-InklinkRepoRoot
$tsf     = Join-Path $root 'tsf'
$ENG     = 'C:\inklink-engine'
$out     = Join-Path $root "dist\inklink-$version"
$files   = Join-Path $out 'files'

Write-Host "== 打包 inklink $version ==" -ForegroundColor Cyan

# ── 前置检查 ───────────────────────────────────────────────────
$dll64 = Join-Path $tsf 'target-static\release\inklink_tsf.dll'
$dll32 = Join-Path $tsf 'target-static\i686-pc-windows-msvc\release\inklink_tsf.dll'
foreach ($p in @($dll64, $dll32, "$ENG\rime-server.exe", "$ENG\rime.dll", "$ENG\rime\data")) {
    if (-not (Test-Path $p)) { throw "缺少 $p —— 先构建 DLL（tools\deploy-local.ps1）或确认引擎目录完整" }
}

# ── 清空重建输出目录 ───────────────────────────────────────────
if (Test-Path $out) { Remove-Item $out -Recurse -Force }
New-Item -ItemType Directory -Force $files | Out-Null

# ── 1. 安装脚本（替换版本号占位符） ────────────────────────────
$inst = Join-Path $root 'installer'
foreach ($f in (Get-ChildItem $inst -File)) {
    $text = [System.IO.File]::ReadAllText($f.FullName)
    $text = $text.Replace('__INKLINK_VERSION__', $version)
    # .cmd 保持纯 ASCII + CRLF（cmd.exe 按 OEM 代码页读文件）；.ps1 带 BOM；.txt 无 BOM
    switch ($f.Extension) {
        '.cmd'  { [System.IO.File]::WriteAllText((Join-Path $out $f.Name), $text, (New-Object System.Text.ASCIIEncoding)) }
        '.ps1'  { [System.IO.File]::WriteAllText((Join-Path $out $f.Name), $text, (New-Object System.Text.UTF8Encoding($true))) }
        default { [System.IO.File]::WriteAllText((Join-Path $out $f.Name), $text, (New-Object System.Text.UTF8Encoding($false))) }
    }
}
Write-Host '  安装脚本 OK'

# ── 2. 二进制 ──────────────────────────────────────────────────
Copy-Item $dll64 (Join-Path $files 'inklink_tsf.dll')   -Force
Copy-Item $dll32 (Join-Path $files 'inklink_tsf32.dll') -Force
Copy-Item "$ENG\rime-server.exe" $files -Force
Copy-Item "$ENG\rime.dll"        $files -Force
Write-Host '  DLL + 引擎 OK'

# ── 3. rime 数据（排除个人词库与锁文件） ───────────────────────
Copy-Item "$ENG\rime\data" (Join-Path $files 'rime') -Recurse -Force
$userDst = Join-Path $files 'rime\user'
New-Item -ItemType Directory -Force $userDst | Out-Null
# 只带构建产物和 identity，不把开发机的个人词库发给别人
Copy-Item "$ENG\rime\user\build" $userDst -Recurse -Force -ErrorAction SilentlyContinue
foreach ($f in 'installation.yaml', 'user.yaml') {
    $src = Join-Path $ENG "rime\user\$f"
    if (Test-Path $src) { Copy-Item $src $userDst -Force }
}
Get-ChildItem $userDst -Recurse -File -Include *.userdb, *.lock -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue
Write-Host '  rime 数据 OK'

# ── 4. 模糊音配置（和引擎配置同源，装机即生效，用户不用手跑脚本） ──
& (Join-Path $PSScriptRoot 'apply-fuzzy.ps1') -Destination $userDst -NoRestart | Out-Null
Write-Host '  模糊音配置 OK'

# ── 5. 打包 ────────────────────────────────────────────────────
if ($SkipZip) {
    Write-Host "已生成目录 $out（未打包）" -ForegroundColor Green
    return
}
$zip = Join-Path $root "dist\inklink-$version.zip"
if (Test-Path $zip) { Remove-Item $zip -Force }
Compress-Archive -Path $out -DestinationPath $zip -CompressionLevel Optimal
$item = Get-Item $zip
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash

Write-Host ''
Write-Host "发布包：$zip" -ForegroundColor Green
Write-Host ("  大小   : {0:N2} MB" -f ($item.Length / 1MB))
Write-Host "  SHA256 : $hash"
Write-Host "  解压后 : inklink-$version\install.cmd"