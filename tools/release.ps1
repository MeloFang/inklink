# 打发布包：dist\inklink-<版本>\ + dist\inklink-<版本>.zip
#
#   powershell -ExecutionPolicy Bypass -File tools\release.ps1
#
# 前置条件：先跑过 tools\deploy-local.ps1（或至少构建过 tsf\target-static），
# 并且 C:\inklink-engine\ 里是完整的引擎 + rime 数据。

param(
    [switch]$SkipZip,
    # 打完包后保留几个历史版本（1 = 只留刚打出来的这个；想留上一个回滚就传 2）
    [int]$KeepOld = 1
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
# 只处理文本文件：installer\ 里还有 inklink.ico / inklink.png 这类二进制，
# 之前用 ReadAllText 逐个"改写"会把图标写成乱码（压缩包里出现 73KB 的坏 ico）。
$inst = Join-Path $root 'installer'
$textExt = @('.cmd', '.ps1', '.txt', '.md')
foreach ($f in (Get-ChildItem $inst -File | Where-Object { $textExt -contains $_.Extension })) {
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
Copy-Item (Join-Path $root 'installer\inklink.ico') (Join-Path $files 'inklink.ico') -Force
Copy-Item (Join-Path $root 'installer\inklink-badge.ico') (Join-Path $files 'inklink-badge.ico') -Force
Copy-Item "$ENG\rime-server.exe" $files -Force
Copy-Item "$ENG\rime.dll"        $files -Force
Write-Host '  DLL + 引擎 OK'

# ── 3. rime 数据 ───────────────────────────────────────────────
# data 目录里的内容由 tools\fetch-librime.ps1 铺好（裁过的配置 + rime-ice 派生的 base.dict.yaml）。
# 这里先确认那份词库在——不确认的话会打出"能装但候选很差"的包（Weasel 原版繁体词库）。
$baseDict = Join-Path $ENG 'rime\data\base.dict.yaml'
if (-not (Test-Path $baseDict)) {
    throw "引擎目录里没有 base.dict.yaml —— 先用 tools\fetch-librime.ps1 把 data 铺好（否则打包会用 Weasel 原版繁体词库，候选质量明显变差）"
}
Copy-Item "$ENG\rime\data" (Join-Path $files 'rime') -Recurse -Force
$userDst = Join-Path $files 'rime\user'
New-Item -ItemType Directory -Force $userDst | Out-Null
# 不带 build：那是 librime 按本机词库编译出来的产物（22 MB）。实测全新编译只要 4.4 秒，
# 带上它既让包大一倍，又会像 1.1.x 那样出现"表比词库新"的悬空状态。
# 也不带 installation.yaml / user.yaml：里面有本机的 identity，不该发给别人。
Write-Host '  rime 数据 OK（不含 build，首次启动自动编译 ≈4 秒）'

# ── 4. 模糊音配置（和引擎配置同源，装机即生效，用户不用手跑脚本） ──
$fuzzyDefault = Join-Path $root 'rime-config\fuzzy.yaml'
    & (Join-Path $PSScriptRoot 'apply-fuzzy.ps1') -Destination $userDst -ConfigPath $fuzzyDefault -NoRestart | Out-Null
Write-Host '  模糊音配置 OK'

# ── 5. 打包 ────────────────────────────────────────────────────
if ($SkipZip) {
    Write-Host "已生成目录 $out（未打包）" -ForegroundColor Green
    return
}
$zip = Join-Path $root "dist\inklink-$version.zip"
# 先压到临时目录，成功后再整体替换。
# 教训：老写法是「先删旧 zip 再压」，一旦压缩被拒（Explorer 预览 / 杀软 / 网盘同步占着目标文件），
# 旧包已经删了、新包也没生成，等于把发布包弄丢（2026-10-27 真踩过）。
$zipTmp = Join-Path $env:TEMP "inklink-$version.zip"
if (Test-Path $zipTmp) { Remove-Item $zipTmp -Force }
Compress-Archive -Path $out -DestinationPath $zipTmp -CompressionLevel Optimal
Move-Item $zipTmp $zip -Force
$item = Get-Item $zip
$hash = (Get-FileHash $zip -Algorithm SHA256).Hash

Write-Host ''
Write-Host "发布包：$zip" -ForegroundColor Green
Write-Host ("  大小   : {0:N2} MB" -f ($item.Length / 1MB))
Write-Host "  SHA256 : $hash"
Write-Host "  解压后 : inklink-$version\install.cmd"

# ── 6. 清理旧产物（dist 每次 60MB 目录 + 27MB zip，不清理会一直堆）──
# 每个版本是「目录 + zip」一对，所以按版本分组删，不能按文件数删（否则会剩半个版本）。
# 保留：刚打出来的这个（一定保留）+ 最近 $KeepOld 个历史版本（默认 1，方便回滚上一个）。
# 排序按时间不按版本号字符串，因为 0.1.30 和 1.0.0 这类跨里程碑版本号不能直接比大小。
$distRoot = Join-Path $root 'dist'
$staleAll = Get-ChildItem $distRoot -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -like 'inklink-*' -and $_.FullName -ne $out -and $_.FullName -ne $zip }
$stale = @()
if ($staleAll) {
    # 从名字里取版本号：inklink-1.0.0 / inklink-1.0.0.zip -> 1.0.0
    $groups = $staleAll | Group-Object { ($_.Name -replace '^inklink-', '') -replace '\.zip$', '' } |
        Sort-Object { ($_.Group | Measure-Object LastWriteTime -Maximum).Maximum } -Descending
    $groups | Select-Object -Skip ([Math]::Max(0, $KeepOld)) | ForEach-Object { $stale += $_.Group }
}
if ($stale.Count -gt 0) {
    $freed = 0
    foreach ($s in $stale) {
        if (-not $s.FullName.StartsWith($distRoot + '\', [StringComparison]::OrdinalIgnoreCase)) { continue }
        $sz = if ($s.PSIsContainer) {
            (Get-ChildItem $s.FullName -Recurse -File -ErrorAction SilentlyContinue | Measure-Object Length -Sum).Sum
        } else { $s.Length }
        Remove-Item $s.FullName -Recurse -Force -ErrorAction SilentlyContinue
        if (-not (Test-Path $s.FullName)) { $freed += $sz }
    }
    Write-Host ("  已清理旧产物 {0} 项，释放 {1:N1} MB（保留刚打出来的 + 最近 {2} 个历史版本）" -f $stale.Count, ($freed / 1MB), $KeepOld)
}