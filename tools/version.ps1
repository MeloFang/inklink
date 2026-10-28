# inklink 版本号工具 —— 单一真相源是仓库根目录的 VERSION 文件
#
# 版本格式：X.V.Z（沿用 SemVer 的「高位递增、低位归零」规则）
#   X = 里程碑序号     0 = M0（能打字）  1 = M1（个人词库）  2 = M2（Obsidian 语料）  3 = M3（AI）
#   V = 里程碑内阶段序号   0 起步，进入新阶段时 +1
#   Z = 修订号            每次发版/部署自增；**进入新阶段或新里程碑时归零**
#
# 归零规则（SemVer 2.0.0：主版本号递增时次版本号与修订号归零，次版本号递增时修订号归零）：
#   X 变 → V 和 Z 都归零     Set-InklinkVersion 1.0.0
#   V 变 → Z 归零            Step-InklinkStage
#   其余 → Z +1              Step-InklinkVersion
#
# 例：0.1.22 = M0 第 1 阶段的第 22 次修订（旧命名的 v22）。
#     M0 结束后打 M1 的第一个版本就是 1.0.0，而不是 1.0.23。
#
# 用法：
#   . .\tools\version.ps1
#   Get-InklinkVersion                 # 0.1.23
#   Set-InklinkVersion 1.0.0           # 手动设定（换里程碑）并同步 Cargo.toml
#   Step-InklinkStage                  # V+1、Z 归零
#   Step-InklinkVersion                # Z+1，返回 0.1.24

function Get-InklinkRepoRoot { Split-Path $PSScriptRoot -Parent }

function Get-InklinkVersion {
    $f = Join-Path (Get-InklinkRepoRoot) 'VERSION'
    if (-not (Test-Path $f)) { throw "找不到 $f —— 版本号必须先有 VERSION 文件" }
    $v = (Get-Content $f -Raw).Trim()
    if ($v -notmatch '^\d+\.\d+\.\d+$') { throw "VERSION 内容 '$v' 不是 X.V.Z 格式" }
    return $v
}

function Set-InklinkVersion {
    param([Parameter(Mandatory)][string]$Version)
    if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "版本号 '$Version' 不是 X.V.Z 格式" }
    $root = Get-InklinkRepoRoot
    [System.IO.File]::WriteAllText((Join-Path $root 'VERSION'), "$Version`n", (New-Object System.Text.ASCIIEncoding))

    # 同步各处声明的版本号，避免「DLL 0.1.23 / 安装包 0.1.22」这种错配
    # 只同步实际出货的组件；app/ 已退役，保持原版本号
    $targets = @(
        @{ Path = 'tsf/Cargo.toml'; Pattern = '^version\s*=\s*".*"'; Replacement = "version = `"$Version`"" },
        @{ Path = 'ui/Cargo.toml';  Pattern = '^version\s*=\s*".*"'; Replacement = "version = `"$Version`"" }
    )
    foreach ($t in $targets) {
        $p = Join-Path $root $t.Path
        if (-not (Test-Path $p)) { continue }
        $text = [System.IO.File]::ReadAllText($p)
        # 只替换**第一个**匹配（Cargo.toml / package.json 里第一个 version 才是包自身的）
        $re = [regex]::new($t.Pattern, 'Multiline')
        if ($re.IsMatch($text)) {
            $text = $re.Replace($text, $t.Replacement, 1)
            [System.IO.File]::WriteAllText($p, $text, (New-Object System.Text.UTF8Encoding($false)))
        }
    }
    Write-Host "版本号已设为 $Version" -ForegroundColor Green
    return $Version
}

function Step-InklinkVersion {
    param([int]$By = 1)
    $v = Get-InklinkVersion
    $parts = $v.Split('.')
    $parts[2] = [string]([int]$parts[2] + $By)
    return (Set-InklinkVersion ($parts -join '.'))
}

# 进入本里程碑的下一个阶段：V+1，Z 归零
function Step-InklinkStage {
    param([int]$By = 1)
    $v = Get-InklinkVersion
    $parts = $v.Split('.')
    $parts[1] = [string]([int]$parts[1] + $By)
    $parts[2] = '0'
    return (Set-InklinkVersion ($parts -join '.'))
}
