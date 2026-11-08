# 把 Markdown 笔记目录导成"个人语料表"（M2 切片 1）
#
#   powershell -ExecutionPolicy Bypass -File tools\import-corpus.ps1 -Notes C:\dsh\work\notes
#
# 做两件事：
#   1. extractor\scan_vault.py：扫目录（标题/双链/标签/粗体/重复都算权重）-> extractor\terms.json
#   2. extractor\build_corpus_tsv.py：terms.json + 8105 字表 -> corpus.tsv（码 \t 词 \t 权重）
#
# 语料表默认写到 %LOCALAPPDATA%\inklink\corpus.tsv，引擎启动时读入。
# **改完要重启引擎**（rime-server.exe）才生效：把正在跑的进程结束掉，下次打字会自动拉起。

param(
    # Markdown 目录（Obsidian vault 根目录或任意笔记目录），可以给多个：-Notes A,B
    [string[]]$Notes = @('E:\Obsidian\News Releases'),
    # 输出文件；默认 %LOCALAPPDATA%\inklink\corpus.tsv
    [string]$Out = (Join-Path $env:LOCALAPPDATA 'inklink\corpus.tsv'),
    # 最多保留多少条（按权重截断）
    [int]$Max = 5000,
    # 跳过的目录/文件名关键字（Obsidian vault 里这些别扫）
    [string[]]$Exclude = @('.obsidian', '.trash', '.git', '模板', '_archive', '_docx原件', 'assets')
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

# `-File` 调用时 `-Notes A,B` 会当成一个字符串，这里按逗号拆开（`-Command` 下本来就是数组）
$Notes = @($Notes | ForEach-Object { $_ -split ',' } | ForEach-Object { $_.Trim() } | Where-Object { $_ })

foreach ($n in $Notes) {
    if (-not (Test-Path $n)) { throw "笔记目录不存在：$n" }
}

Write-Host "== 扫描笔记 ==" -ForegroundColor Cyan
$first = $true
foreach ($n in $Notes) {
    Write-Host ("  扫描: " + $n) -ForegroundColor DarkGray
    $argv = @((Join-Path $root 'extractor\scan_vault.py'), $n)
    if ($Exclude.Count -gt 0) { $argv += '--exclude'; $argv += $Exclude }
    if (-not $first) { $argv += '--merge' }
    python @argv
    if ($LASTEXITCODE -ne 0) { throw "scan_vault.py 失败（退出码 $LASTEXITCODE）" }
    $first = $false
}

Write-Host ''
Write-Host "== 生成语料表 ==" -ForegroundColor Cyan
python (Join-Path $root 'extractor\build_corpus_tsv.py') -Out $Out -Max $Max
if ($LASTEXITCODE -ne 0) { throw "build_corpus_tsv.py 失败（退出码 $LASTEXITCODE）" }

Write-Host ''
Write-Host "语料表已写入：$Out" -ForegroundColor Green
Write-Host '重启引擎后生效：结束 rime-server.exe，或者注销重来（下次打字会自动拉起）。' -ForegroundColor Yellow
