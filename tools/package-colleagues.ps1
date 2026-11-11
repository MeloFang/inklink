# 打「发给同事」的一体化压缩包：安装程序 + 使用说明 + 旧版清理工具 + 许可 + 校验清单
#
#   powershell -ExecutionPolicy Bypass -File tools\package-colleagues.ps1
#
# 产物：dist\inklink-<版本>-同事包.zip
# 前置：先跑过 tools\release.ps1（要有 dist\inklink-<版本>-setup.exe）

param(
    [string]$OutDir = ''
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\version.ps1"

$version = Get-InklinkVersion
$root    = Get-InklinkRepoRoot
$dist    = Join-Path $root 'dist'
$setup   = Join-Path $dist "inklink-$version-setup.exe"
if (-not (Test-Path $setup)) { throw "缺少 $setup —— 先跑 tools\release.ps1" }

# 工作目录：先铺临时目录，成功后再整体替换（和 release.ps1 同一个教训：
# 先删旧包再压缩，一旦压缩失败就把上一份产物弄丢了）
$stage = Join-Path $env:TEMP "inklink-colleagues-$version"
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Force $stage | Out-Null
New-Item -ItemType Directory -Force (Join-Path $stage '旧版清理工具') | Out-Null

Copy-Item $setup (Join-Path $stage (Split-Path $setup -Leaf)) -Force
foreach ($f in @('cleanup-old.cmd', 'cleanup-old.ps1')) {
    $src = Join-Path $root "installer\$f"
    if (-not (Test-Path $src)) { throw "缺少 $src" }
    Copy-Item $src (Join-Path $stage "旧版清理工具\$f") -Force
}
Copy-Item (Join-Path $root 'LICENSE') (Join-Path $stage 'LICENSE.txt') -Force

# 使用说明：把版本号占位符替换掉，UTF-8 无 BOM（记事本也能正常打开）
$readme = Join-Path $root 'installer\colleague-readme.txt'
$text = [System.IO.File]::ReadAllText($readme).Replace('__INKLINK_VERSION__', $version)
[System.IO.File]::WriteAllText((Join-Path $stage '使用说明.txt'), $text, (New-Object System.Text.UTF8Encoding($false)))

# 校验清单：覆盖包内每个文件
$sums = foreach ($f in (Get-ChildItem $stage -Recurse -File | Sort-Object FullName)) {
    $rel = $f.FullName.Substring($stage.Length + 1).Replace('\', '/')
    "{0}  {1}" -f (Get-FileHash $f.FullName -Algorithm SHA256).Hash, $rel
}
[System.IO.File]::WriteAllLines((Join-Path $stage 'SHA256SUMS.txt'), $sums, (New-Object System.Text.UTF8Encoding($false)))

$zipTmp = Join-Path $env:TEMP "inklink-$version-同事包.zip"
if (Test-Path $zipTmp) { Remove-Item $zipTmp -Force }
Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zipTmp -CompressionLevel Optimal

$out = if ($OutDir) { Join-Path $OutDir "inklink-$version-同事包.zip" } else { Join-Path $dist "inklink-$version-同事包.zip" }
Move-Item $zipTmp $out -Force

$item = Get-Item $out
$hash = (Get-FileHash $out -Algorithm SHA256).Hash
Write-Host ''
Write-Host "同事包：$out" -ForegroundColor Green
Write-Host ("  大小   : {0:N2} MB" -f ($item.Length / 1MB))
Write-Host "  SHA256 : $hash"
Write-Host '  内容   :'
Get-ChildItem $stage -Recurse -File | ForEach-Object {
    Write-Host ("           " + $_.FullName.Substring($stage.Length + 1))
}
