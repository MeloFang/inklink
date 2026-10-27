# inklink 引擎管道客户端（诊断/验证用，可 dot-source 复用）
#
#   . "$PSScriptRoot\engine-pipe.ps1"
#   Invoke-InklinkQuery -Code 'xiangmu'      # -> 候选字符串数组
#   Invoke-InklinkEngineAlive                # -> $true/$false
#
# 协议：连接后写 [u8 type=1][u32 utf8 长度][utf8 拼音]，
#       读 [u32 候选数] + 候选数 × ([u16 长度][utf8 词])。

$script:InklinkPipeName = 'inklink-engine'

function Invoke-InklinkEngineAlive {
    param([int]$TimeoutMs = 300)
    try {
        $pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', $script:InklinkPipeName, [System.IO.Pipes.PipeDirection]::InOut)
        try {
            $pipe.Connect($TimeoutMs)
            return $true
        } finally { $pipe.Dispose() }
    } catch { return $false }
}

function Invoke-InklinkQuery {
    param(
        [Parameter(Mandatory = $true)][string]$Code,
        [int]$TimeoutMs = 3000
    )
    $pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', $script:InklinkPipeName, [System.IO.Pipes.PipeDirection]::InOut)
    try {
        $pipe.Connect($TimeoutMs)
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($Code)
        $writer = New-Object System.IO.BinaryWriter($pipe, [System.Text.Encoding]::UTF8, $true)
        $writer.Write([byte]1)
        $writer.Write([uint32]$bytes.Length)
        $writer.Write($bytes)
        $writer.Flush()

        $reader = New-Object System.IO.BinaryReader($pipe, [System.Text.Encoding]::UTF8, $true)
        $count = $reader.ReadUInt32()
        $cands = New-Object System.Collections.Generic.List[string]
        for ($i = 0; $i -lt $count; $i++) {
            $len = $reader.ReadUInt16()
            $buf = $reader.ReadBytes([int]$len)
            $cands.Add([System.Text.Encoding]::UTF8.GetString($buf))
        }
        return , $cands.ToArray()
    } finally {
        $pipe.Dispose()
    }
}

function Wait-InklinkEngine {
    param([int]$TimeoutSec = 90)
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    while ((Get-Date) -lt $deadline) {
        if (Invoke-InklinkEngineAlive) {
            # 管道通了还不够 —— 词库可能还在 deploy，用一次真实查询确认
            try {
                $c = Invoke-InklinkQuery -Code 'ni' -TimeoutMs 3000
                if ($c -and $c.Count -gt 0) { return $true }
            } catch { }
        }
        Start-Sleep -Milliseconds 400
    }
    return $false
}
