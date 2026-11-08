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

# 本机同时装着 GNU 和 MSVC 两套 Rust。不把 ~\.cargo\bin 放到最前面，cargo 会挑 GNU 那套，
# 编 windows-sys 时要 dlltool.exe（MSVC 工具链里没有）直接失败。
# deploy-local.ps1 早就有这行，release.ps1 是后来才开始编 Rust 的，漏了。
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

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
foreach ($p in @($dll64, $dll32, "$ENG\rime-server.exe", "$ENG\rime.dll", "$ENG\rime\data", "$ENG\en-words.txt")) {
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
# 随包字体（朱雀仿宋，OFL-1.1）：装到 Program Files\inklink\fonts，DLL 用 FR_PRIVATE 私有加载
$fontsSrc = Join-Path $root 'installer\fonts'
if (Test-Path $fontsSrc) {
    Copy-Item $fontsSrc $files -Recurse -Force
    Write-Host ('  字体 ' + ((Get-ChildItem $fontsSrc -File).Count) + ' 个') -ForegroundColor DarkGray
}
Copy-Item "$ENG\rime-server.exe" $files -Force
Copy-Item "$ENG\rime.dll"        $files -Force
Copy-Item "$ENG\en-words.txt"    $files -Force
Copy-Item (Join-Path $root 'installer\pinyin.tsv') (Join-Path $files 'pinyin.tsv') -Force
# 设置窗（P2a）：和 DLL 同目录，浮条菜单的「设置…」按同目录找它
$settingsExe = Join-Path $root 'settings\target\release\inklink-settings.exe'
if (-not (Test-Path $settingsExe)) { throw "没有设置窗产物：$settingsExe（先跑 tools\deploy-local.ps1 或 cargo build -p inklink-settings --release）" }
Copy-Item $settingsExe (Join-Path $files 'inklink-settings.exe') -Force
Write-Host '  DLL + 引擎 + 设置窗 OK'

# ── 3. rime 数据 ───────────────────────────────────────────────
# data 目录里的内容由 tools\fetch-librime.ps1 铺好（裁过的配置 + rime-ice 派生的 base.dict.yaml）。
# 这里先确认那份词库在——不确认的话会打出"能装但候选很差"的包（Weasel 原版繁体词库）。
$baseDict = Join-Path $ENG 'rime\data\base.dict.yaml'
if (-not (Test-Path $baseDict)) {
    throw "引擎目录里没有 base.dict.yaml —— 先用 tools\fetch-librime.ps1 把 data 铺好（否则打包会用 Weasel 原版繁体词库，候选质量明显变差）"
}
# 单字表同理：没有 8105.dict.yaml 的话，装出来的输入法打单个字（zi/wo/de）一个候选都没有。
$charsDict = Join-Path $ENG 'rime\data\8105.dict.yaml'
if (-not (Test-Path $charsDict)) {
    throw "引擎目录里没有 8105.dict.yaml —— 单音节（zi/wo/de）会查不到候选，先用 tools\fetch-librime.ps1 铺好 data"
}
# 目标路径必须**逐字是 rime\data**：直接 Copy-Item "$ENG\rime\data" "$files\rime"
# 在 files\rime 不存在时会把 data 的内容拍平到 rime\ 下（1.1.37 的包就是这么坏的：
# 装完 C:\inklink-engine\rime\data 是空的，打字一个候选都没有）。
$rimeDst = Join-Path $files 'rime\data'
New-Item -ItemType Directory -Force $rimeDst | Out-Null
Copy-Item "$ENG\rime\data\*" $rimeDst -Recurse -Force
$dataCount = (Get-ChildItem $rimeDst -Recurse -File | Measure-Object).Count
if ($dataCount -lt 10) { throw "rime 数据没打成：$rimeDst 只有 $dataCount 个文件" }
Write-Host "  rime 数据 $dataCount 个文件"
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

# ── 5b. 附带的两个文件：校验清单 + 发布说明 ─────────────────────
# 用户下载后能自己核对：`certutil -hashfile inklink-<版本>.zip SHA256` 对着看。
$distRoot = Join-Path $root 'dist'
$sums = Join-Path $distRoot 'SHA256SUMS.txt'
Set-Content -Path $sums -Value ("{0}  inklink-{1}.zip" -f $hash, $version) -Encoding ascii
Write-Host "  校验清单: $sums"



# ── 5c. 单文件安装器 ────────────────────────────────────────────
# 做法：编一个"只有解包 + 写注册表"的小 exe（installer\setup），
# 把 files\ 下的所有文件打成自定义档案追加到它末尾。
# 这样发布出去的就是一个 PE——SignTool 能签，SmartScreen 认得，用户也只下一个文件。
# 格式（与 installer\setup\src\main.rs 的 read_payload 一一对应）：
#   deflate( [u32 名字长度][名字 utf8][u64 数据长度][数据] × N )
#   [u32 N][u64 档案起始偏移][u64 解压后长度][8 字节 "INKLNK01"]
$setupStub = Join-Path $root 'installer\setup\target\release\inklink-setup.exe'
Write-Host '  构建安装器…'
# cargo 会往 stderr 打 "Compiling …"，而脚本开头设了 $ErrorActionPreference='Stop'
# —— 直接调用会被 PowerShell 包成 NativeCommandError 当场终止。所以临时放宽 + 全流重定向。
$prevEap = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
$cargoLog = Join-Path $env:TEMP 'inklink-setup-cargo.log'
& cargo build --release --manifest-path (Join-Path $root 'installer\setup\Cargo.toml') *> $cargoLog
$cargoExit = $LASTEXITCODE
$ErrorActionPreference = $prevEap
if ($cargoExit -ne 0) {
    if (Test-Path $cargoLog) { Get-Content $cargoLog | Select-Object -Last 15 | ForEach-Object { Write-Host "    $_" } }
    throw "安装器构建失败（cargo exit $cargoExit，日志 $cargoLog）"
}
if (-not (Test-Path $setupStub)) { throw "安装器构建失败：$setupStub 不存在" }

$stub   = [System.IO.File]::ReadAllBytes($setupStub)
$ms     = New-Object System.IO.MemoryStream
$bw     = New-Object System.IO.BinaryWriter($ms)
$nEntry = 0
foreach ($f in (Get-ChildItem $files -Recurse -File)) {
    $rel = $f.FullName.Substring($files.Length + 1).Replace('\', '/')
    # 装到哪：引擎那一坨（rime*）进 C:\inklink-engine，其余进 Program Files\inklink
    $prefix = if ($rel -like 'rime/*' -or $rel -eq 'rime-server.exe' -or $rel -eq 'rime.dll' -or $rel -eq 'en-words.txt') { 'eng/' } else { 'pf/' }
    $name = [System.Text.Encoding]::UTF8.GetBytes($prefix + $rel)
    $data = [System.IO.File]::ReadAllBytes($f.FullName)
    $bw.Write([uint32]$name.Length)
    $bw.Write($name)
    $bw.Write([uint64]$data.Length)
    $bw.Write($data)
    $nEntry++
}
$bw.Flush()
$rawBlob = $ms.ToArray()
$bw.Dispose(); $ms.Dispose()

# 整块 deflate：23 MB 裸数据压完 ≈9 MB，下载量差一倍多。
$packed = New-Object System.IO.MemoryStream
$ds = New-Object System.IO.Compression.DeflateStream($packed, [System.IO.Compression.CompressionMode]::Compress)
$ds.Write($rawBlob, 0, $rawBlob.Length)
$ds.Dispose()
$payload = $packed.ToArray()
$packed.Dispose()

$startOffset = [uint64]$stub.Length
$trailer = New-Object System.IO.MemoryStream
$tw = New-Object System.IO.BinaryWriter($trailer)
$tw.Write([uint32]$nEntry)
$tw.Write([uint64]$startOffset)
$tw.Write([uint64]$rawBlob.Length)
$tw.Write([System.Text.Encoding]::ASCII.GetBytes('INKLNK01'))
$tw.Flush()
$trailerBytes = $trailer.ToArray()
$tw.Dispose(); $trailer.Dispose()

$setupExe = Join-Path $distRoot "inklink-$version-setup.exe"
$final = New-Object System.IO.MemoryStream
$final.Write($stub, 0, $stub.Length)
$final.Write($payload, 0, $payload.Length)
$final.Write($trailerBytes, 0, $trailerBytes.Length)
[System.IO.File]::WriteAllBytes($setupExe, $final.ToArray())
$final.Dispose()

$setupItem = Get-Item $setupExe
$setupHash = (Get-FileHash $setupExe -Algorithm SHA256).Hash
Write-Host "  单文件安装器: $setupExe" -ForegroundColor Green
Write-Host ("    {0} 个文件 / {1:N2} MB / SHA256 {2}" -f $nEntry, ($setupItem.Length / 1MB), $setupHash)
Add-Content -Path $sums -Value ("{0}  inklink-{1}-setup.exe" -f $setupHash, $version) -Encoding ascii

# 发布说明从 VERSION 与最近一次 git log 生成，避免手写漏项。
# 放在安装器打包之后：说明里的哈希必须覆盖用户真正会下载的那两个文件。
# git 的输出是 UTF-8，而 Windows PowerShell 5.1 按控制台代码页解码 —— 不临时改
# OutputEncoding，中文提交信息在说明里就是一堆乱码（1.1.37 踩过）。
$notes = Join-Path $distRoot 'RELEASE-NOTES.md'
$prevEnc = [Console]::OutputEncoding
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false
$recent = & git -C $root log --oneline -12 --no-decorate 2>$null
[Console]::OutputEncoding = $prevEnc
$notesBody = @"
# inklink $version

Windows 中文输入法（TSF）。

## 装哪个

- **``inklink-$version-setup.exe``** —— 推荐。单文件，双击（会弹 UAC）就装完，不用解压。
- ``inklink-$version.zip`` —— 绿色版，解压后右键 ``install.cmd`` → 以管理员身份运行。

## 安装前

- Windows 10 1809+ / Windows 11，64 位。
- 安装器会往 ``C:\Program Files\inklink`` 与 ``C:\inklink-engine`` 写文件、注册 TSF 输入法、
  把引擎路径写进 ``HKLM\SOFTWARE\inklink\EnginePath``，所以需要管理员权限。
- 装完在「设置 → 时间和语言 → 语言和区域」里添加键盘 → inklink 输入法
  （安装器能写注册表，但"把输入法挂进你的语言列表"只有系统设置能做）。

## 校验

``````
certutil -hashfile inklink-$version-setup.exe SHA256
certutil -hashfile inklink-$version.zip SHA256
``````

应当是（也写在 ``SHA256SUMS.txt`` 里）：

``````
$setupHash  inklink-$version-setup.exe
$hash  inklink-$version.zip
``````

## 本版内容

$($recent -join "`n")

## 代码签名

本项目使用 [SignPath Foundation](https://signpath.org/) 为开源项目提供的免费代码签名服务
（申请中，见 [签名策略](https://github.com/MeloFang/inklink/blob/main/docs/code-signing-policy.md)）。
在签名生效前请用 ``SHA256SUMS.txt`` 校验下载的包。

## 许可

GPL-3.0。词库衍生自 [rime-ice](https://github.com/iDvel/rime-ice)（GPL-3.0），
librime 来自 [Weasel](https://github.com/rime/weasel) 的发行包。
"@
Set-Content -Path $notes -Value $notesBody -Encoding utf8
Write-Host "  发布说明: $notes"

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