# 取 librime：从 Weasel 安装包里抽出 rime.dll 与 rime\data，并按白名单裁掉用不到的东西。
#
# 为什么要从 Weasel 里抽：librime 官方不发 Windows 二进制；Weasel 是官方 Windows 前端，
# 它的安装包里那份 rime.dll 就是 librime 本体（x86，正是我们需要的那份）。
#
# 用法（把东西铺到一个引擎目录）：
#   tools\fetch-librime.ps1                      # 下载 + 校验 + 解包 + 裁 data，铺到 C:\inklink-engine
#   tools\fetch-librime.ps1 -Dest D:\tmp\eng     # 铺到别处（CI 用）
#   tools\fetch-librime.ps1 -NoPrune             # 不裁，全量铺（调试用）
#   tools\fetch-librime.ps1 -PruneOnly -Dest D:\x# 只对已有目录做裁剪
#
# 只裁 data，不碰 build：build 是 librime 首次运行时自己编译出来的（见 README 的说明）。
#
# 词库：Weasel 自带的 luna_pinyin.dict.yaml 是**繁体原版**（項目 在、项目 不在，简体靠
# simplifier 过滤），而且它把萌典里的古籍字排得很前——实测 rebuild 之后打 xiangmu 出的
# 是 㐮㙁，項目 连前 30 都进不去。所以默认换成 rime-ice 派生的 base.dict.yaml
# （data\upstream\ice\base.dict.yaml，GPL-3.0，来源与哈希见 data\manifest.json），
# 再用一个只写 `import_tables: [base]` 的壳把它挂成 luna_pinyin。加 -NoRimeIce 可退回原版。

[CmdletBinding()]
param(
    # 固定版本 + 固定哈希，改版本时必须同时改这三个值，否则校验会拦下来。
    [string]$Version = '0.17.4',
    [string]$AssetName = 'weasel-0.17.4.0-installer.exe',
    [string]$AssetSha256 = 'cf509534a8f5f8af9c98ed7cbb8f135439f145a8cbe7e50ede42bb5b5ab45c29',

    # 缓存目录（下载、解包都放这儿）；留空 = <仓库>\vendor。不能在 param 默认值里用
    # $PSScriptRoot —— PS 5.1 绑参数时它还是空的，所以放到下面再补齐。
    [string]$CacheDir,
    [string]$Dest = 'C:\inklink-engine',

    # rime-ice 词库目录；留空 = <仓库>\data\upstream\ice
    [string]$IceDir,

    # 7-Zip 主程序；默认自动找 Program Files 和 PATH。
    [string]$SevenZip,

    [switch]$NoPrune,
    [switch]$NoRimeIce,
    [switch]$PruneOnly,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

$repo = Split-Path -Parent $PSScriptRoot
if (-not $CacheDir) { $CacheDir = Join-Path $repo 'vendor' }
if (-not $IceDir) { $IceDir = Join-Path $repo 'data\upstream\ice' }

# ---------------------------------------------------------------------------
# 白名单：只留 luna_pinyin_simp（简体朙月拼音）真正读到的文件。
#
# 依据是 data 里的 schema 引用链：
#   default.yaml 的 schema_list → luna_pinyin_simp
#   luna_pinyin_simp.schema.yaml __include luna_pinyin.schema:/
#   luna_pinyin.schema.yaml：dictionary=luna_pinyin、dependencies=[stroke]（反查要用）
#                            import_preset: symbols / default（标点、按键绑定）
#                            simplifier 滤镜（t2s.json + opencc 词典）
# pinyin.yaml 被 luna_pinyin.schema 的 speller.__patch 引用（简拼/纠错规则）。
# essay.txt / terra_pinyin / cangjie5 / bopomofo / zhuyin / preview\*.png 与本方案无关。
# ---------------------------------------------------------------------------
$KeepData = @(
    'default.yaml'
    'luna_pinyin.dict.yaml'     # Weasel 原版；下面会被 rime-ice 派生的壳覆盖（-NoRimeIce 时才留它）
    'luna_pinyin.schema.yaml'
    'luna_pinyin_simp.schema.yaml'
    'stroke.dict.yaml'          # luna_pinyin 依赖 stroke（` 反查），删了 librime 会报缺依赖
    'stroke.schema.yaml'
    'pinyin.yaml'
    'key_bindings.yaml'
    'punctuation.yaml'
    'symbols.yaml'
    'opencc\t2s.json'           # simplifier 默认用的就是它
    'opencc\STCharacters.ocd2'
    'opencc\STPhrases.ocd2'
    'opencc\TSCharacters.ocd2'
    'opencc\TSPhrases.ocd2'
)

function Install-Lexicon {
    # 把 rime-ice 的 base.dict.yaml 铺进去，再写一个只做挂载的 luna_pinyin.dict.yaml。
    # 为什么不直接用 base.dict.yaml 顶替 luna_pinyin：luna_pinyin.schema.yaml 里写的是
    # `dictionary: luna_pinyin`，librime 按名字找文件，文件名和里面的 name: 都得对得上。
    param([string]$DataDir, [string]$Ice, [switch]$KeepWeasel)
    if ($KeepWeasel) {
        Write-Host "  -NoRimeIce：保留 Weasel 原版词库（繁体，候选质量会明显变差）" -ForegroundColor Yellow
        return
    }
    $base = Join-Path $Ice 'base.dict.yaml'
    if (-not (Test-Path $base)) {
        throw "没有 rime-ice 词库：$base`n先按 data\manifest.json 里的 url 下载到 $Ice"
    }
    Copy-Item $base (Join-Path $DataDir 'base.dict.yaml') -Force
    $stub = @"
# Rime dictionary
# encoding: utf-8
#
# inklink 主词库挂载点：词条全在 base.dict.yaml（rime-ice 派生，GPL-3.0）。
# 这个壳只负责让 luna_pinyin.schema.yaml 的 `dictionary: luna_pinyin` 找到东西。
#
# use_preset_vocabulary 关掉：它会把 essay.txt 一起编进表，而发布包里没有 essay.txt。
name: luna_pinyin
version: "1.0"
sort: by_weight
use_preset_vocabulary: false
import_tables:
  - base
...
"@
    # 必须无 BOM：librime 解析 yaml 时 BOM 会被当成内容的一部分。
    [System.IO.File]::WriteAllText((Join-Path $DataDir 'luna_pinyin.dict.yaml'), $stub, (New-Object System.Text.UTF8Encoding($false)))
    $kb = [int]((Get-Item $base).Length / 1KB)
    Write-Host "  词库 -> base.dict.yaml ($kb KB) + luna_pinyin 挂载壳" -ForegroundColor Green
}

function Resolve-SevenZip {
    param([string]$Explicit)
    if ($Explicit) {
        if (-not (Test-Path $Explicit)) { throw "指定的 7-Zip 不存在：$Explicit" }
        return $Explicit
    }
    foreach ($p in @(
            'C:\Program Files\7-Zip\7z.exe',
            'C:\Program Files (x86)\7-Zip\7z.exe',
            (Join-Path $env:LOCALAPPDATA 'Programs\7-Zip\7z.exe'))) {
        if (Test-Path $p) { return $p }
    }
    $cmd = Get-Command 7z -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    throw "找不到 7-Zip。装一个（winget install 7zip.7zip）或用 -SevenZip 指定 7z.exe 的路径。"
}

function Get-Installer {
    param([string]$Name, [string]$Sha, [string]$Dir, [switch]$Fresh)
    New-Item -ItemType Directory -Force -Path $Dir | Out-Null
    $path = Join-Path $Dir $Name
    if ((Test-Path $path) -and -not $Fresh) {
        $have = (Get-FileHash $path -Algorithm SHA256).Hash.ToLower()
        if ($have -eq $Sha) {
            Write-Host "  已缓存：$path" -ForegroundColor DarkGray
            return $path
        }
        Write-Host "  缓存哈希不对，重新下载" -ForegroundColor Yellow
    }
    $url = "https://github.com/rime/weasel/releases/download/$Version/$Name"
    Write-Host "  下载 $url" -ForegroundColor Cyan
    & curl.exe -sSL --retry 3 --retry-delay 2 -o $path $url
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $path)) { throw "下载失败：$url" }
    $got = (Get-FileHash $path -Algorithm SHA256).Hash.ToLower()
    if ($got -ne $Sha) {
        Remove-Item $path -Force -ErrorAction SilentlyContinue
        throw "SHA256 不匹配（拿到 $got，期望 $Sha）。已删掉这个文件。"
    }
    "  校验通过：SHA256 $got" | Write-Host -ForegroundColor Green
    return $path
}

function Limit-Data {
    # 注意：PowerShell 变量不区分大小写，所以这里的局部集合不能叫 $keep（会和参数 $Keep 撞名，
    # 撞上之后参数被覆盖成哈希表，后面按字符串下标取值就会炸成"hashtable 转 Int32"）。
    param([string]$DataDir, [string[]]$Whitelist)
    if (-not (Test-Path $DataDir)) { throw "没有 data 目录：$DataDir" }
    $allowed = @{}
    foreach ($k in $Whitelist) { $allowed[$k.Replace('/', '\').ToLower()] = $true }

    $removedFiles = 0
    $removedBytes = 0
    Get-ChildItem $DataDir -Recurse -File | ForEach-Object {
        $rel = $_.FullName.Substring($DataDir.Length + 1)
        if (-not $allowed.ContainsKey($rel.ToLower())) {
            $removedBytes += $_.Length
            $removedFiles++
            Remove-Item $_.FullName -Force
        }
    }
    # 删完文件后把空目录收掉（opencc 里可能还有别的 json 被删光了的情况不会发生，但 preview 一定会空）
    Get-ChildItem $DataDir -Recurse -Directory | Sort-Object { $_.FullName.Length } -Descending | ForEach-Object {
        if (-not (Get-ChildItem $_.FullName -Force)) { Remove-Item $_.FullName -Force }
    }

    $left = Get-ChildItem $DataDir -Recurse -File
    $size = if ($left.Count) { ($left | Measure-Object Length -Sum).Sum } else { 0 }
    "  裁掉 $removedFiles 个文件（$([math]::Round($removedBytes / 1MB, 1)) MB），剩 $($left.Count) 个 / $([math]::Round($size / 1MB, 2)) MB" |
        Write-Host -ForegroundColor Green
}

Write-Host "== fetch-librime $Version ==" -ForegroundColor Cyan

$dataDest = Join-Path $Dest 'rime\data'

if (-not $PruneOnly) {
    $sz = Resolve-SevenZip -Explicit $SevenZip
    Write-Host "  7-Zip: $sz" -ForegroundColor DarkGray

    $installer = Get-Installer -Name $AssetName -Sha $AssetSha256 -Dir (Join-Path $CacheDir 'downloads') -Fresh:$Force
    $extractDir = Join-Path $CacheDir "weasel-$Version"
    if ($Force -or -not (Test-Path (Join-Path $extractDir 'rime.dll'))) {
        Write-Host "  解包到 $extractDir" -ForegroundColor Cyan
        if (Test-Path $extractDir) { Remove-Item $extractDir -Recurse -Force }
        & $sz x -y "-o$extractDir" $installer | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "7-Zip 解包失败" }
    }

    $dllSrc = Join-Path $extractDir 'rime.dll'
    $dataSrc = Join-Path $extractDir 'data'
    if (-not (Test-Path $dllSrc)) { throw "安装包里没找到 rime.dll（结构变了？）" }
    if (-not (Test-Path $dataSrc)) { throw "安装包里没找到 data 目录（结构变了？）" }

    New-Item -ItemType Directory -Force -Path $Dest, $dataDest | Out-Null
    Copy-Item $dllSrc (Join-Path $Dest 'rime.dll') -Force
    $dllKb = [int]((Get-Item $dllSrc).Length / 1KB)
    Write-Host "  rime.dll -> $(Join-Path $Dest 'rime.dll')  ($dllKb KB)" -ForegroundColor Green

    # data 是整份拷过来再裁，这样白名单漏了谁一眼能看出来（裁完的文件清单会打出来）。
    Get-ChildItem $dataDest -Force | Remove-Item -Recurse -Force
    Copy-Item (Join-Path $dataSrc '*') $dataDest -Recurse -Force
    Write-Host "  data -> $dataDest（全量拷贝完成）" -ForegroundColor Green
}

if (-not $NoPrune) {
    Limit-Data -DataDir $dataDest -Whitelist $KeepData
} else {
    Write-Host "  -NoPrune：data 没裁" -ForegroundColor Yellow
}

Install-Lexicon -DataDir $dataDest -Ice $IceDir -KeepWeasel:$NoRimeIce

# 用我们自己的 default.yaml 覆盖 Weasel 那份：它的 schema_list 列了五六套方案，
# 那些方案的数据我们已经裁掉了，留着会导致
#   ① 每次启动都重新 deploy，刷一屏 "missing input schema: bopomofo / cangjie5 / ..."；
#   ② 更要命的是**默认方案会落到列表第一个**（实测是 bopomofo），
#      于是打 xiangmu 出的是 㐮㙁 这种带注音的怪候选。
# 我们这份只留 luna_pinyin_simp，其余配置（标点、按键、page_size）与 Weasel 一致。
$defaultYaml = Join-Path (Split-Path -Parent $PSScriptRoot) 'rime-config\default.yaml'
if (Test-Path $defaultYaml) {
    Copy-Item $defaultYaml (Join-Path $dataDest 'default.yaml') -Force
    Write-Host "  default.yaml -> 用 rime-config\default.yaml（只留 luna_pinyin_simp）" -ForegroundColor Green
} else {
    Write-Host "  警告：没找到 rime-config\default.yaml，保留 Weasel 那份（多方案会变慢）" -ForegroundColor Yellow
}

$left = Get-ChildItem $dataDest -Recurse -File
Write-Host "== 完成 ==" -ForegroundColor Cyan
$left | Sort-Object FullName | ForEach-Object {
    $rel = $_.FullName.Substring($dataDest.Length + 1)
    "  {0,9:N0}  {1}" -f $_.Length, $rel
}
