# 回归探针：模拟老版本 DLL（< 1.0.0）的选词报文，确认引擎不会再死锁。
#
# 背景（2026-10-27，微信被系统当"未响应"杀掉那次）：协议 v3 的选词报文比 v2 多了
# code/word 三个字段。引擎当初直接 read_exact(&clen, 4) 死等，而老 DLL 发完
# [type][index] 五个字节就等响应 —— 两边互相等，调用方应用的 UI 线程永久挂起。
# 现在引擎先用 PeekNamedPipe 探测后续字节，100ms 内没有就走兼容路径立即回应。
#
# 用法：powershell -ExecutionPolicy Bypass -File tools\probe-legacy-select.ps1
# 退出码：0 = 收到响应（正常）；1 = 超时（说明引擎又死等老协议不会发来的字段）

param([int]$TimeoutMs = 3000)

$ErrorActionPreference = 'Stop'

$pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', 'inklink-engine', [System.IO.Pipes.PipeDirection]::InOut)
try {
    $pipe.Connect(2000)
    $writer = New-Object System.IO.BinaryWriter($pipe)
    # v2 报文：只有类型和下标，后面什么都没有
    $writer.Write([byte]2)
    $writer.Write([uint32]0)
    $writer.Flush()
    Write-Host '已发送老协议选词报文（5 字节），等待响应…'

    $buf = New-Object byte[] 4
    $task = $pipe.ReadAsync($buf, 0, 4)
    $started = Get-Date
    if ($task.Wait($TimeoutMs)) {
        $ms = [int]((Get-Date) - $started).TotalMilliseconds
        Write-Host ("OK：{0}ms 收到响应，{1} 字节（值 {2}）" -f $ms, $task.Result, ($buf -join ','))
        exit 0
    }
    Write-Host ("FAIL：{0}ms 没有响应 —— 引擎又卡在等 v3 字段了" -f $TimeoutMs) -ForegroundColor Red
    exit 1
}
finally {
    $pipe.Dispose()
}
