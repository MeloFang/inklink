# Probe the AI plumbing end to end, without any UI:
#   type 6 (start) -> engine background thread -> fake Ollama -> type 7 (result)
#
#   python tools\mock-ollama.py 11435
#   powershell -ExecutionPolicy Bypass -File tools\probe-ai.ps1 -Action 0 -Text hello
#
# Action: 0 = translate, 1 = rewrite, 2 = continue (same numbers as the engine).
# Exit code 0 only when the engine reports state 1 (done) with a non-empty answer.
#
# Why bypass the UI: this checks the protocol and the HTTP client. The UI path
# (selection -> menu -> write back) has to be checked by hand in Notepad.

param(
    [int]$Action = 0,
    [string]$Text = 'hello',
    [int]$TimeoutSec = 15
)

$ErrorActionPreference = 'Stop'
$pipeName = 'inklink-engine'

function Connect-Pipe {
    $pipe = New-Object System.IO.Pipes.NamedPipeClientStream('.', $pipeName, [System.IO.Pipes.PipeDirection]::InOut)
    $pipe.Connect(2000)
    return $pipe
}

# Sequence number: unique enough for a probe; the engine keys results by it.
$seq = [uint32]([DateTime]::Now.Ticks -band 0x7FFFFFFF)
$bytes = [System.Text.Encoding]::UTF8.GetBytes($Text)

$pipe = Connect-Pipe
try {
    $w = New-Object System.IO.BinaryWriter($pipe, [System.Text.Encoding]::UTF8, $true)
    $w.Write([byte]6)                 # type 6 = start
    $w.Write([uint32]$seq)
    $w.Write([uint32]0)               # hwnd = 0: no PostMessage notification needed
    $w.Write([byte]$Action)
    $w.Write([uint16]$bytes.Length)
    $w.Write($bytes)
    $w.Flush()
    $r = New-Object System.IO.BinaryReader($pipe, [System.Text.Encoding]::UTF8, $true)
    $null = $r.ReadUInt32()           # empty response (count = 0)
} finally { $pipe.Dispose() }

$deadline = (Get-Date).AddSeconds($TimeoutSec)
$state = 3
$answer = ''
while ((Get-Date) -lt $deadline) {
    Start-Sleep -Milliseconds 200
    $pipe = Connect-Pipe
    try {
        $w = New-Object System.IO.BinaryWriter($pipe, [System.Text.Encoding]::UTF8, $true)
        $w.Write([byte]7)             # type 7 = fetch result
        $w.Write([uint32]$seq)
        $w.Flush()
        $r = New-Object System.IO.BinaryReader($pipe, [System.Text.Encoding]::UTF8, $true)
        $state = $r.ReadByte()
        $len = $r.ReadUInt16()
        if ($len -gt 0) {
            $buf = $r.ReadBytes([int]$len)
            $answer = [System.Text.Encoding]::UTF8.GetString($buf)
        }
    } finally { $pipe.Dispose() }
    if ($state -ne 0) { break }
}

# Console codepage is unreliable for CJK; write the answer to a UTF-8 file and only
# print ASCII status, the same convention the engine's --selftest follows.
$out = Join-Path $env:TEMP 'inklink-probe-ai-answer.txt'
[IO.File]::WriteAllText($out, $answer, (New-Object System.Text.UTF8Encoding $false))
Write-Host "state=$state chars=$($answer.Length) answer_file=$out"
if ($state -eq 1 -and $answer.Length -gt 0) { exit 0 } else { exit 1 }
