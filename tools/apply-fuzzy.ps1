# inklink 模糊音生成器
#
# 读 rime-config\fuzzy.yaml 的开关，生成引擎的 luna_pinyin_simp.custom.yaml，
# 然后重启引擎让 librime 重新 deploy（约 10-60 秒，取决于词库大小）。
#
# 用法：
#   powershell -ExecutionPolicy Bypass -File tools\apply-fuzzy.ps1
#   powershell -ExecutionPolicy Bypass -File tools\apply-fuzzy.ps1 -NoRestart      # 只生成文件
#   powershell -ExecutionPolicy Bypass -File tools\apply-fuzzy.ps1 -Destination dist\inklink-0.1.27\files\rime\user -NoRestart  # 打包用
#
# 原理：librime 的 speller/algebra 是一串「拼写代数」规则。模糊音片段（zh_z_bufen 之类）
# 定义在引擎数据目录的 pinyin.yaml 里，用 __patch 拉进 speller/algebra 即可生效。
# 顺序很重要：模糊音片段必须排在 abbreviation（简拼）之前，否则简拼不吃模糊音。

[CmdletBinding()]
param(
    [string]$Destination = 'C:\inklink-engine\rime\user',
    [switch]$NoRestart,
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'version.ps1')
. (Join-Path $PSScriptRoot 'engine-pipe.ps1')

$repoRoot = Get-InklinkRepoRoot
$configPath = Join-Path $repoRoot 'rime-config\fuzzy.yaml'
$engineExe = 'C:\inklink-engine\rime-server.exe'

# 组名 → pinyin.yaml 里的片段名。顺序即生成顺序（决定候选混排的细节）。
$GroupFragments = [ordered]@{
    zh_z    = @{ Fragment = 'zh_z_bufen';    Desc = 'zh/z  ch/c  sh/s' }
    n_l     = @{ Fragment = 'n_l_bufen';     Desc = 'n/l' }
    r_l     = @{ Fragment = 'r_l_bufen';     Desc = 'r/l' }
    r_y     = @{ Fragment = 'r_y_bufen';     Desc = 'ren->yin 等' }
    f_h     = @{ Fragment = 'hu_f_buhun';    Desc = 'fu/hu、fei/hui 等' }
    eng_ong = @{ Fragment = 'eng_ong_bufen'; Desc = '(b p m f)+eng/ong' }
    en_eng  = @{ Fragment = 'en_eng_bufen';  Desc = 'en/eng  in/ing' }
}

function Read-FuzzyConfig {
    param([string]$Path)
    if (-not (Test-Path $Path)) { throw "找不到模糊音配置：$Path" }
    $cfg = @{}
    foreach ($line in (Get-Content $Path -Encoding UTF8)) {
        $t = $line.Trim()
        if ($t -eq '' -or $t.StartsWith('#')) { continue }
        if ($t -notmatch '^([A-Za-z_][A-Za-z0-9_]*)\s*:\s*(true|false)\s*(#.*)?$') {
            throw "模糊音配置第 $($cfg.Count) 行看不懂：$t（只接受「组名: true/false」）"
        }
        $name = $Matches[1].ToLower()
        if (-not $GroupFragments.Contains($name)) {
            throw "未知的模糊音组「$name」。可用的有：$($GroupFragments.Keys -join '、')"
        }
        $cfg[$name] = ($Matches[2].ToLower() -eq 'true')
    }
    return $cfg
}

function New-AlgebraPatchText {
    param([hashtable]$Config)

    $enabled = @()
    foreach ($name in $GroupFragments.Keys) {
        if ($Config.ContainsKey($name) -and $Config[$name]) {
            $enabled += $GroupFragments[$name].Fragment
        }
    }

    $desc = '（无，全部按标准音）'
    if ($enabled.Count -gt 0) {
        $desc = (($GroupFragments.Keys | Where-Object { $Config[$_] }) | ForEach-Object { $GroupFragments[$_].Desc }) -join ' / '
    }

    $lines = @(
        '# 由 tools\apply-fuzzy.ps1 根据 rime-config\fuzzy.yaml 生成，请勿手改。'
        '#'
        "# 生成时间：$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')"
        "# 启用的模糊音：$desc"
        '#'
        '# 想改模糊音：编辑 rime-config\fuzzy.yaml，再执行 tools\apply-fuzzy.ps1'
        'patch:'
        '  speller/algebra:'
        '    __patch:'
    )
    foreach ($f in $enabled) { $lines += "      - pinyin:/$f" }
    # 这三段是原 schema 就有的，模糊音之后必须原样接着
    $lines += '      - pinyin:/abbreviation'
    $lines += '      - pinyin:/spelling_correction'
    $lines += '      - pinyin:/key_correction'
    return (($lines -join "`r`n") + "`r`n")
}

function Restart-InklinkEngine {
    if (-not (Test-Path $engineExe)) { throw "找不到引擎：$engineExe" }
    Get-Process -Name 'rime-server' -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 400
    Start-Process -FilePath $engineExe -WorkingDirectory (Split-Path $engineExe -Parent) -WindowStyle Hidden
}

function Test-FuzzyEffective {
    param([hashtable]$Config)
    # 用通道里真实查一遍：开了 zh_z 后，打 zi 应该能出 zh- 声母的字；关着就不该有。
    $zhiOnly = @('知', '之', '只', '支', '直', '值', '质', '制', '指', '至')
    $ziCands = Invoke-InklinkQuery -Code 'zi' -TimeoutMs 5000
    $hit = $ziCands | Where-Object { $zhiOnly -contains $_ }
    $expect = $Config['zh_z'] -eq $true
    $ok = if ($expect) { $hit.Count -gt 0 } else { $hit.Count -eq 0 }
    return [pscustomobject]@{
        Ok        = $ok
        Check     = "zi 的候选里是否出现 zh- 声母的字（期望：$(if ($expect) { '有' } else { '无' })）"
        Hits      = ($hit -join ' ')
        Candidate = ($ziCands | Select-Object -First 8) -join ' '
    }
}

# ---- 主流程 ----
$config = Read-FuzzyConfig -Path $configPath
$text = New-AlgebraPatchText -Config $config
$target = Join-Path $Destination 'luna_pinyin_simp.custom.yaml'

$changed = $true
if (Test-Path $target) {
    $old = [System.IO.File]::ReadAllText($target)
    if ($old -eq $text -and -not $Force) { $changed = $false }
}

if ($changed) {
    if (-not (Test-Path $Destination)) { New-Item -ItemType Directory -Path $Destination -Force | Out-Null }
    [System.IO.File]::WriteAllText($target, $text, (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "已写入 $target" -ForegroundColor Green
} else {
    Write-Host "配置未变化，跳过写入：$target" -ForegroundColor DarkGray
}

if ($NoRestart) {
    Write-Host '（-NoRestart：没有重启引擎，要生效请手动重启 rime-server）' -ForegroundColor DarkGray
    return
}

if (-not $changed -and (Invoke-InklinkEngineAlive)) {
    Write-Host '引擎在跑，无需重启。' -ForegroundColor DarkGray
} else {
    Write-Host '重启引擎并等待 librime deploy（可能要几十秒）……' -ForegroundColor Cyan
    Restart-InklinkEngine
    if (-not (Wait-InklinkEngine -TimeoutSec 180)) { throw '引擎在 180 秒内没有就绪，看 C:\inklink-engine\rime-server.log' }
}

$check = Test-FuzzyEffective -Config $config
Write-Host ''
Write-Host "验证：$($check.Check)" -ForegroundColor Cyan
Write-Host "  命中：$(if ($check.Hits) { $check.Hits } else { '（无）' })"
Write-Host "  zi 前 8 个候选：$($check.Candidate)"
if ($check.Ok) {
    Write-Host '模糊音已生效 ✔' -ForegroundColor Green
} else {
    Write-Host '模糊音没生效 ✘ —— 检查 C:\inklink-engine\rime\user\build 是否重新 deploy 过' -ForegroundColor Red
    exit 1
}
