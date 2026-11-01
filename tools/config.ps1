# inklink user config helper (P3).
#
# Reads / writes HKCU\Software\inklink\Config. The TSF DLL reads these values on
# every keystroke, so changes take effect immediately - no restart needed.
#
# Usage:
#   .\config.ps1                       # show current values (with defaults)
#   .\config.ps1 -Dark 1               # dark candidate window
#   .\config.ps1 -Cols 3 -Rows 3       # 3x3 grid (collapsed = 3, expanded = 9)
#   .\config.ps1 -UndoMs 0             # disable "wrong pick" undo
#   .\config.ps1 -Learn 0              # stop learning new words
#   .\config.ps1 -Reset                # delete the whole key (back to defaults)
#
# NOTE: this file is deliberately ASCII-only. PowerShell 5.1 reads a UTF-8 file
# without BOM as GB18030 and mangles non-ASCII text, which has bitten us before.

[CmdletBinding()]
param(
    [ValidateSet('0', '1')] [string]$Dark,
    [ValidateRange(3, 9)]   [int]$Cols,
    [ValidateRange(1, 9)]   [int]$Rows,
    [ValidateRange(0, 10000)] [int]$UndoMs,
    [ValidateSet('0', '1')] [string]$Learn,
    [switch]$Reset
)

$ErrorActionPreference = 'Stop'
$key = 'HKCU:\Software\inklink\Config'

if ($Reset) {
    if (Test-Path $key) { Remove-Item $key -Recurse -Force }
    Write-Host 'config reset to defaults'
}

if (-not (Test-Path $key)) {
    New-Item -Path $key -Force | Out-Null
}

if ($PSBoundParameters.ContainsKey('Dark'))   { Set-ItemProperty $key -Name Dark   -Value ([int]$Dark)   -Type DWord }
if ($PSBoundParameters.ContainsKey('Cols'))   { Set-ItemProperty $key -Name Cols   -Value $Cols          -Type DWord }
if ($PSBoundParameters.ContainsKey('Rows'))   { Set-ItemProperty $key -Name Rows   -Value $Rows          -Type DWord }
if ($PSBoundParameters.ContainsKey('UndoMs')) { Set-ItemProperty $key -Name UndoMs -Value $UndoMs        -Type DWord }
if ($PSBoundParameters.ContainsKey('Learn'))  { Set-ItemProperty $key -Name Learn  -Value ([int]$Learn)  -Type DWord }

$item = Get-ItemProperty $key
function Show($name, $default, $note) {
    $v = $item.$name
    if ($null -eq $v) { $v = "$default (default)" }
    Write-Host ("  {0,-8} {1,-16} {2}" -f $name, $v, $note)
}

Write-Host ''
Write-Host 'inklink config (HKCU\Software\inklink\Config)'
Show 'Dark'   0    '0 = light candidate window, 1 = dark'
Show 'Cols'   5    'candidate grid columns (3-9)'
Show 'Rows'   5    'rows when expanded (1-9); collapsed is always 1 row'
Show 'UndoMs' 2000 'undo window in ms (0 = off); backspace within it un-learns'
Show 'Learn'  1    '0 = query only, never learn new words'
$cols = if ($null -eq $item.Cols) { 5 } else { [int]$item.Cols }
$rows = if ($null -eq $item.Rows) { 5 } else { [int]$item.Rows }
Write-Host ''
Write-Host ("  page size: collapsed = {0}, expanded = {1}" -f $cols, ($cols * $rows))
Write-Host ''
