# VM 测试环境

inklink 的 TSF 输入法在 Hyper-V 虚拟机「Windows 11 开发环境」里测试（本机是日常使用机，不注册实验性输入法）。

## 脚本

| 文件 | 作用 |
|:---|:---|
| `bootstrap.cmd` | 一次性：在 VM 里建测试账号 `inklink`（密码 `Ink!2026test`），供宿主机 PowerShell Direct 连接 |
| `deploy.ps1` | 部署新 DLL：关记事本 → 复制 DLL 到唯一文件名 → 改注册表 → 确保引擎在跑 |
| `uninstall.ps1` | 卸载：摘语言配置 + 反注册 + 清注册表 |
| `diag.ps1` | 诊断：查注册表指向、枚举语言配置、测试 DLL 能否实例化 |

## 从宿主机推送文件到 VM

```powershell
$vm = 'Windows 11 开发环境'
# Copy-VMFile 只能 Host→Guest，同名文件会报错，必须用新文件名
Copy-VMFile -VMName $vm -SourcePath '...\inklink_tsf.dll' -DestinationPath 'C:\Users\Public\inklink_tsf.dll' -CreateFullPath -FileSource Host
```

## 用 PowerShell Direct 直接控制 VM

```powershell
$vm = 'Windows 11 开发环境'
$pw = ConvertTo-SecureString 'Ink!2026test' -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential('inklink', $pw)
Invoke-Command -VMName $vm -Credential $cred -ScriptBlock { ... }
```

注意：PowerShell Direct 跑在 session 0，**无法**做 GUI 操作（切输入法、往记事本打字），
所以最终的"切到 inklink 打字"这一步仍需人在 VM 里做；但部署、看日志、查进程都能自动化。

## 回滚

VM 有快照 `tsf-base`，出问题 `Restore-VMSnapshot -VMName $vm -Name 'tsf-base'` 即可。
