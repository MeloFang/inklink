# inklink 输入法诊断脚本（只读，不改任何东西）
#
# 用法：双击 diagnose.cmd（或右键 PowerShell 以管理员运行本脚本）
# 产物：桌面上 inklink-诊断-<时间>.txt，把整个文件发回来即可定位问题。

$ErrorActionPreference = 'Continue'
$CLSID   = '{8F2A6C1E-4B3D-4F7A-9A21-5C6D7E8F9A0B}'
$PF      = 'C:\Program Files\inklink'
$ENG     = 'C:\inklink-engine'
$out     = Join-Path ([Environment]::GetFolderPath('Desktop')) ("inklink-诊断-{0}.txt" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))
$lines   = New-Object System.Collections.Generic.List[string]

function Say($s) {
    $lines.Add($s)
    Write-Host $s
}
function Check($name, $ok, $detail = '') {
    Say ("{0} {1}{2}" -f ($(if ($ok) { '[OK]  ' } else { '[失败]' })), $name, $(if ($detail) { "  — $detail" } else { '' }))
}

Say '== inklink 诊断 =='
Say "时间      : $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')"
Say "系统      : $([Environment]::OSVersion.VersionString) / $env:PROCESSOR_ARCHITECTURE)"
Say "PowerShell: $($PSVersionTable.PSVersion)"
Say ''

Say '-- 1. 引擎路径配置 --'
$reg = Get-ItemProperty 'HKLM:\SOFTWARE\inklink' -ErrorAction SilentlyContinue
Say "EnginePath: $(if ($reg) { $reg.EnginePath } else { '(缺失)' })"
Say ''

Say '-- 2. 程序文件 --'
foreach ($f in @(
    "$PF\inklink_tsf.dll", "$PF\inklink_tsf32.dll", "$PF\inklink-settings.exe",
    "$PF\inklink.ico", "$PF\fonts\ZhuqueFangsong-Regular.ttf",
    "$ENG\rime-server.exe", "$ENG\rime.dll", "$ENG\en-words.txt",
    "$ENG\rime\data\default.yaml", "$ENG\rime\data\base.dict.yaml", "$ENG\rime\data\8105.dict.yaml"
)) {
    Check $f (Test-Path $f)
}
$dataCount = (Get-ChildItem "$ENG\rime\data" -File -ErrorAction SilentlyContinue | Measure-Object).Count
Say "rime\data 文件数 : $dataCount"
$userCount = (Get-ChildItem "$ENG\rime\user" -File -ErrorAction SilentlyContinue | Measure-Object).Count
Say "rime\user 文件数 : $userCount"
Say ''

Say '-- 3. 注册表 --'
$com64 = (Get-ItemProperty "HKLM:\SOFTWARE\Classes\CLSID\$CLSID\InprocServer32" -ErrorAction SilentlyContinue).'(default)'
$com32 = (Get-ItemProperty "HKLM:\SOFTWARE\Classes\Wow6432Node\CLSID\$CLSID\InprocServer32" -ErrorAction SilentlyContinue).'(default)'
Check 'COM 64 位指向' ($com64 -and (Test-Path $com64)) $com64
Check 'COM 32 位指向' ($com32 -and (Test-Path $com32)) $com32
$uninst = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\inklink' -ErrorAction SilentlyContinue
Check '卸载登记（应用列表里能看到）' ([bool]$uninst) $(if ($uninst) { $uninst.DisplayName } else { '缺这条 = 应用列表里找不到' })
Say ''

Say '-- 4. 进程 --'
$p = Get-Process rime-server -ErrorAction SilentlyContinue
if ($p) {
    foreach ($x in $p) {
        Say ("rime-server pid={0} 启动于 {1}" -f $x.Id, $(try { $x.StartTime } catch { '?' }))
        Say ("  路径: " + $(try { $x.Path } catch { '?' }))
    }
} else {
    Say 'rime-server: 没有运行'
}
Say ''

Say '-- 5. 管道实测（发一次真实拼音查询）--'
function Probe([string]$code) {
    $pipeName = 'inklink-engine'
    $c = New-Object System.IO.Pipes.NamedPipeClientStream('.', $pipeName, [System.IO.Pipes.PipeDirection]::InOut)
    try {
        $c.Connect(3000)
    } catch {
        return @{ ok = $false; why = "连不上管道：$($_.Exception.Message)"; count = -1 }
    }
    try {
        $bytes = [Text.Encoding]::UTF8.GetBytes($code)
        $w = New-Object System.IO.BinaryWriter($c)
        $w.Write([byte]1)                      # 类型 1 = 查询
        $w.Write([uint32]$bytes.Length)        # 码长度
        $w.Write($bytes)
        $w.Write([uint16]0)                    # app 长度 0
        $w.Flush()
        $r = New-Object System.IO.BinaryReader($c)
        $count = $r.ReadUInt32()
        $words = @()
        for ($i = 0; $i -lt [Math]::Min($count, 5); $i++) {
            $n = $r.ReadUInt16()
            $buf = $r.ReadBytes($n)
            $words += [Text.Encoding]::UTF8.GetString($buf)
        }
        return @{ ok = $true; count = $count; words = $words }
    } catch {
        return @{ ok = $false; why = "查询出错：$($_.Exception.Message)"; count = -1 }
    } finally {
        $c.Dispose()
    }
}
$r = Probe 'nihao'
if ($r.ok) {
    Check "查询 nihao" ($r.count -gt 0) ("候选 $($r.count) 个" + $(if ($r.words) { '：' + ($r.words -join ' / ') } else { '' }))
} else {
    Check '查询 nihao' $false $r.why
}
$r2 = Probe 'xiangmu'
if ($r2.ok) { Check "查询 xiangmu" ($r2.count -gt 0) ("候选 $($r2.count) 个") } else { Check '查询 xiangmu' $false $r2.why }
Say ''

Say '-- 6. 引擎日志（最后 40 行）--'
$engLog = Join-Path $ENG 'rime-server.log'
if (Test-Path $engLog) {
    Say ("日志: $engLog  ({0:N1} KB, 最后写入 {1})" -f ((Get-Item $engLog).Length / 1KB), (Get-Item $engLog).LastWriteTime)
    Get-Content $engLog -Tail 40 | ForEach-Object { $lines.Add('  ' + $_) }
} else {
    Say "没有引擎日志：$engLog（说明引擎一次都没起来过）"
}
Say ''

Say '-- 7. 安装日志（最后 20 行）--'
$insLog = Join-Path $env:PUBLIC 'inklink-install.log'
if (Test-Path $insLog) {
    Get-Content $insLog -Tail 20 | ForEach-Object { $lines.Add('  ' + $_) }
} else {
    Say "没有安装日志：$insLog"
}

[IO.File]::WriteAllLines($out, $lines, (New-Object Text.UTF8Encoding $false))
Write-Host ''
Write-Host "诊断报告已保存：$out" -ForegroundColor Green
Write-Host '把这个文件发回给开发者即可定位问题。' -ForegroundColor Green
