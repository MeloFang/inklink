# 交接：Windows Terminal / PowerShell 里打不出中文（2026-11-22，**已解决**）

> 这份文档现在有两个用途：
> 1. 记录**根因和修法**（下面「根因」一节），改了 `tsf/src/lib.rs` 和 `installer/setup/src/main.rs`；
> 2. 留一份排查过程，以后遇到"输入法某个宿主里完全不出现"这类问题可以照着走一遍。

## 根因（一句话）

我们注册 TIP 时**漏了 `GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT` 分类**，而且
`LanguageProfile` 里**没有具名 `Description` 值**。Windows Terminal 走 CoreText
（`Windows.UI.Core.TextInput` → `TextInputHost.exe`），只从带沉浸式分类的 TIP 里挑输入法；
没有具名 `Description` 的那条又会被 `Set-WinUserLanguageList` **静默丢弃**，
所以 Win+Space 里永远看不到 inklink —— 于是"PowerShell 里打不出中文"。

补齐这两处（`Category\Category\{13A016DF-…}\{CLSID}` + `LanguageProfile\…\Description`）之后，
WT 窗口关掉重开，Win+Space 里出现「inklink 输入法」，选中即可正常打中文（2026-11-22 用户确认）。

**代码修复**：

| 文件 | 改动 |
|:---|:---|
| `tsf/src/lib.rs` | 新增 `GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT` 常量；`register_with_tsf()` 里多注册一个分类；`registry::register()` 增加第 4 步，补写具名 `Description` |
| `installer/setup/src/main.rs` | 多写两棵沉浸式分类键（`Category\Category\…` 与 `Category\Item\…`）；`LanguageProfile` 补具名 `Description` |
| `installer/install.ps1` | 第 6 步改成**回读校验**：`Set-WinUserLanguageList` 之后重新读一遍列表，没进去就告警，不再只打印"已加入" |

## 排查过程（还会用到的方法）

**能确定**：用户的"PowerShell"是 **Windows Terminal**，走 CoreText 输入栈
（`Windows.UI.Core.TextInput.dll` + `NInput.dll` → `TextInputHost.exe`），
而 `inklink_tsf*.dll` **从来没被加载进那条链**（WT 进程、TextInputHost 进程里都是 0 个 inklink 模块）。

**关键对照法**：拿一个**同类但能用**的第三方输入法（这台机器上是搜狗）逐棵树 diff 注册表，
最后 diff 出来只差「沉浸式分类」和「具名 Description」。这比读文档猜快得多。

**没被影响**：同一台机器上微信、Chrome、Edge、DeepSeek Harness（Electron）、记事本里 inklink 都正常
（日志里有组合串和选词）。所以不是输入法核心坏了，是那类宿主看不到我们。

> 下面「试过什么、结果如何」和「下一步该做的」两节是排查过程留档，
> 其中第 2、3 条（`EnableLanguageProfile`/`EnumProfiles`、`ThreadingModel=Both`）**已经不需要做了**。

## 症状

- 在 PowerShell 窗口（实为 Windows Terminal）打 `nihao`，出来的是英文字母，没有候选窗。
- 版本 2.2.17 和 2.2.18 都一样；与 2.2.18 的 Ctrl+C 修复无关。
- 其它应用正常。

## 已收集的证据（都在 `C:\Users\Public\`）

1. 用户窗口诊断（在用户机器上跑的 `inklink-diag.ps1` → `inklink-diag.txt`）：

   ```
   pid=28516 name=powershell session=2 is64=True
   inklink_modules=NONE                 ← DLL 没进这个进程
   console_class=PseudoConsoleWindow    ← ConPTY，不是 conhost
   ```

2. `WindowsTerminal.exe`（pid 29796，110 个模块）里有：
   `Windows.UI.Core.TextInput.dll`、`NInput.dll` —— **CoreText 输入栈**，没有 inklink。
3. `TextInputHost.exe`（"Windows 输入体验"，pid 33332）里有：
   `TextInput.dll`、`windows.ui.core.textinput.dll`、`ninput.dll`、`VCRUNTIME140_APP.dll` ——
   **也没有 inklink，甚至没加载任何第三方 IME DLL（搜狗也没有）**。
4. 用户的输入法列表原本只有两个：

   ```
   zh-Hans-CN : 0804:{81D4E9C9-1D3B-41BC-9E6C-4B40BF79E35E}{FA550B04-5AD7-411F-A5AC-CA038EC515D7}   微软拼音
              : 0804:{E7EA138E-69F8-11D7-A6EA-00065B844310}{E7EA138F-69F8-11D7-A6EA-00065B844311}   另一个中文 IME
   ```

   原始值已备份：`C:\Users\Public\inklink-langlist-before.txt`。

## 我们的注册现状（关键 ID）

| 项 | 值 |
|:---|:---|
| TIP CLSID | `{8F2A6C1E-4B3D-4F7A-9A21-5C6D7E8F9A0B}` |
| Profile GUID | `{1B2C3D4E-5F60-4718-9A2B-3C4D5E6F7081}` |
| LANGID | `0x0804` |
| 64 位 DLL | `C:\Program Files\inklink\inklink_tsf_2.2.18.dll` |
| 32 位 DLL | `...\inklink_tsf32_2.2.18.dll`（WOW6432Node） |
| InprocServer32\ThreadingModel | **Apartment**（微软拼音/搜狗这批第三方 IME 常见的是 `Both`） |

注册代码在 `tsf/src/lib.rs` 的 `register_with_tsf()`（约 3738 行）：
`Register` → `RegisterCategory(KEYBOARD)` → `RegisterCategory(DISPLAYATTRIBUTEPROVIDER)`
→ `AddLanguageProfile` → `EnableLanguageProfile(TRUE)`。

## 试过什么、结果如何（按时间顺序）

| 尝试 | 结果 |
|:---|:---|
| `InstallLayoutOrTip("0804:{clsid}{prof}", 0)` | 返回 `S_FALSE(1)`，列表无变化 |
| `Set-WinUserLanguageList` + `$zh.InputMethodTips.Add(tip)` | 报成功，但**被静默丢弃**（`Get-WinUserLanguageList` 里没有） |
| 手写 `HKLM...\LanguageProfile\0x804\{prof}\Description`（REG_SZ"inklink 输入法"） | **有变化**：之后 `Set-WinUserLanguageList` 接受了我们的 tip |
| 手写分类键 `{13A016DF-560B-46CD-947A-4C3AF1E0E35D}`(IMMERSIVESUPPORT) 与 `{49D2F9CE-01A7-11D7-A6EA-00065B844310}`(UIELEMENTENABLED) | 同上（两者一起做的，未能分辨哪一条起的作用） |
| 手写 `HKCU\Software\Microsoft\CTF\TIP\{clsid}\LanguageProfile\0x804\{prof}\Enable=1` | 写了就被 `Set-WinUserLanguageList` 删掉；再写回、`taskkill /f /im ctfmon.exe` 让它重读 |
| 用户按 `Win+Space` | **两次都看不到 inklink**，也打不出中文 |

当前（手改后）注册表状态：

- `HKCU\Control Panel\International\User Profile\zh-Hans-CN\0804:{clsid}{prof}` **有**
- `HKCU\Software\Microsoft\CTF\SortOrder\AssemblyItem\0x0804\{34745C63-B2F0-4784-8B67-5E12C8701A31}\00000002` **有**
- `HKCU\Software\Microsoft\CTF\TIP\{clsid}\LanguageProfile\0x0804\{prof}\Enable=1` **有**（手写）
- `HKLM\...\CTF\TIP\{clsid}\LanguageProfile\0x0804\{prof}\Description` **有**（手写）
- 以上手改**都还没进代码**，只是这台机器上的手工状态

## 别做什么（踩过的坑）

1. **别在 DSH 会话里做 GUI 自动化验证**。新建记事本 / WinForms 窗口抢不到前台，`Start-Process cmd` 拿不到可见窗口，
   键盘事件会打到别的窗口。已经浪费过两小时。GUI 验证只能请人配合。
2. **别继续往注册表上叠猜测**。`Set-WinUserLanguageList` 会把 `HKCU CTF TIP` 下的 Enable 键删掉，
   手写和系统改写互相打架，很难判断谁生效了。
3. **别拿搜狗的 HKCU 结构当模板**：搜狗在 `HKCU\Software\Microsoft\CTF\TIP` 下**没有**子树，
   照样在 Win+Space 里可用 —— 说明 per-user `Enable` 不是唯一条件。
4. **别在 DSH 里跑 `setup.exe` 做特权写**（沙盒拒绝）；要验证装包用 `--extract` + 人工双击。
5. 部署后**必须重启宿主进程**才加载新 DLL（`DllCanUnloadNow` 恒 `S_FALSE`，不卸载）。

## 下一步该做的（按性价比排序）

### 1. 用真正的 TSF 接口问系统，别猜（最高优先级）

写个 20 行的小工具（C++/Rust/C# 都行），做两件事并打印结果：

```cpp
// (a) 启用并看返回值
ITfInputProcessorProfiles* p;
CoCreateInstance(CLSID_TF_InputProcessorProfiles, ..., &p);
HRESULT hr = p->EnableLanguageProfile({8F2A6C1E-...}, 0x0804, {1B2C3D4E-...}, TRUE);

// (b) 枚举，看系统认为我们是什么状态
ITfInputProcessorProfileMgr* m;  // CLSID_TF_InputProcessorProfiles 也实现了这个接口
IEnumTfLanguageProfiles* e;
m->EnumProfiles(0x0804, &e);     // 逐条打印 clsid / profile / flags / desc
```

`EnumProfiles` 的结果是权威答案：里面有没有我们、`TF_LANGUAGEPROFILE_*` 的 flags 是什么
（`ENABLED` / `INSTALLED` / `ACTIVE` / `DEFAULT`）。`EnableLanguageProfile` 的 `HRESULT` 会直接告诉我们为什么失败。

### 2. 对照 `ThreadingModel`

我们是 `Apartment`。CoreText 宿主（TextInputHost）在线程模型上可能只接受 `Both`。
**实验**：把
`HKLM\SOFTWARE\Classes\CLSID\{clsid}\InprocServer32\ThreadingModel` 改成 `Both`，
（32 位同样改 `WOW6432Node` 那份），然后重启 `TextInputHost.exe` 和 Windows Terminal，
再看模块。

**判定标准只有一个**：`C:\Users\Public\inklink-tsf-<TextInputHost 的 pid>.log` 出现。
我们 DLL 每次被激活都会写这个日志（`activate ... version=2.2.18`），没有日志 = 根本没被加载。

### 3. 如果确认 CoreText 不加载纯 TSF 输入法 → 认下来，做两件事

- **产品文案**：写明「Windows Terminal 与 UWP/Store 应用内暂不支持」，README/使用说明里给出替代方案。
- **conhost 兜底**：把默认终端改回传统控制台（可随时改回）：

  ```
  HKCU\Console\%%Startup\DelegationConsole  = {B23D10C0-E52E-411E-9D5B-C09FDF709C7D}
  HKCU\Console\%%Startup\DelegationTerminal = {B23D10C0-E52E-411E-9D5B-C09FDF709C7D}
  ```

  或者让用户在 Windows Terminal → 设置 → 启动 → 默认终端应用程序里选「Windows 控制台主机」。
  **注意：conhost 能否打字尚未在 2.2.18 上验证过**（只有 2.2.4 时期的历史日志里有过 conhost 的组合串记录）。
  验证方法：开一个 conhost 窗口，打几个拼音，看有没有新的 `inklink-tsf-<conhost pid>.log` 里出现 `compose buffer=`。

### 4. 无论上面结论如何，这两处都该进代码

- `tsf/src/lib.rs` 的 `register_with_tsf()`：`AddLanguageProfile` 写完**再补一个具名 `Description` 值**
  （`AddLanguageProfile` 只写了默认值，而 Windows 校验读的是具名 `Description`，
  缺了就会被 `Set-WinUserLanguageList` 静默丢弃——这是这次唯一确认有正面效果的改动）。
- `installer/install.ps1` 第 6 步：现在只打印"已加入"，不打回读结果。
  改成 `Set-WinUserLanguageList` 之后**回读 `Get-WinUserLanguageList` 校验**，
  没进列表就明确告警（用户至少知道要手动加）。

## 环境与命令备忘

```powershell
# 编译与测试（必须先修 PATH，Program Files 下有 GNU 版 rustc 会盖住 rustup）
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
cd C:\dsh\work\inklink\tsf; cargo test --release     # 165 项

# 构建 + 部署本机（会自动 Z+1，需要重启宿主进程才生效）
cd C:\dsh\work\inklink
powershell -ExecutionPolicy Bypass -File tools\deploy-local.ps1

# 日志：每个宿主进程一个文件，激活/按键/组合/选词都记
C:\Users\Public\inklink-tsf-<pid>.log
# 2.2.18 起 `test vk=` 行多了 hotkey= 字段

# 提交信息含中文或尖括号时用文件，别直接 -m
git commit -F $env:TEMP\msg.txt
```

## 相关但已解决的事（别混淆）

- **Ctrl+C 被吃成拼音**：已在 2.2.18 修复（`MOD_KEY_DOWN_AT` 兜底 + 三路取并集），
  提交 `b5c8690`，与本问题无关，不要回滚。
- 用户当前机器上 2.2.18 已部署，TIP 指向 `inklink_tsf_2.2.18.dll`。
