# 续接文档：inklink

> **最后更新**：2026-10-22 ｜ **当前阶段**：T3 完成（TSF 能打中文），T4 进行中 ｜ **总体进度**：TSF 里程碑 3/5

---

## 一、这个程序现在是什么

**inklink 是一个 TSF 输入法**（不再是热键面板）。架构（ADR-0006 决定）：

```
应用进程（记事本/WPS/浏览器…）
  └─ inklink_tsf.dll（薄壳：收键、查候选、上屏，无词库）
         │  \\.\pipe\inklink-engine  往返 <1ms
         ▼
     inklink.exe --serve（词库 8 万条 + 切分 + 排序，独立进程）
```

**铁律**（AGENTS.md 已改）：引擎绝不进别人进程；DLL 只能是薄壳，`DllMain` 不做事、`DllCanUnloadNow` 恒 `S_FALSE`。

上一轮失败复盘 + TSF 蓝图见 `docs/tsf-plan.md` 和 `docs/adr/0006`。

---

## 二、做到了哪一步

| 里程碑 | 内容 | 状态 |
|:---|:---|:---|
| T1 | 注册成 TSF 输入法，出现在 Win+Space 列表 | ✅ |
| T2 | 按键流打通到 OnKeyDown | ✅ |
| T3 | 能打中文（查词库→候选→上屏），快捷键/退格/错误恢复 | ✅ |
| T4 | 组合串显示（拼音显示在光标处带下划线） | ⏳ |
| T5 | 兼容与兜底（引擎进程生命周期、各应用兼容） | ⏳ |

### 已完成的细节（实测验证）

- `xiangmu`+空格 → 「项目」；`nihao`→「你好」；`zgrm`→「中国人民」（简拼）
- 快捷键 Ctrl+A/C/V、Shift 打大写、CapsLock 打英文、Tab/回车/方向键正常
- 打错字（`xiangmm`）→ 前缀回退出候选，上屏后多余字母自动删除
- 回车 = 原样上屏拼音（恢复通道）
- 引擎管道用 NULL DACL（Box 持有，防悬垂指针），跨用户可连

### 代码位置

| 文件 | 作用 |
|:---|:---|
| `tsf/src/lib.rs` | COM 骨架 + 注册 + 按键状态机 + 查询/上屏 |
| `tsf/src/ipc.rs` | 命名管道客户端 + SendInput 上屏 |
| `tsf/src/bin/probe.rs` | 诊断工具（枚举语言配置 + 测试实例化） |
| `app/src-tauri/src/engine_server.rs` | `--serve` 引擎服务（管道服务器 + NULL DACL） |
| `app/src-tauri/src/engine/` | 词库/切分/排序（复用，未动） |
| `vm/deploy.ps1` `uninstall.ps1` `diag.ps1` | VM 部署/卸载/诊断脚本 |

---

## 三、怎么接着做（T4：组合串显示）

**目标**：打 `xiangmu` 时拼音直接显示在文档光标处、带虚线下划线，和搜狗/微软拼音一样。

**子步骤**：

1. **拿到光标处的 ITfRange**——已知坑：windows-rs 的 `ITfContext::GetSelection` 只返回字符位置（`TS_SELECTION_ACP`）、`GetStart` 返回 `IAnchor`，都**不是**标准 TSF 的 `TF_SELECTION.range`。需要先搞清楚 windows-rs 里怎么拿 ITfRange（可能需要 `GetSelection` 的另一个签名，或手动建 range）。
2. **ITfEditSession**：实现 `ITfEditSession_Impl::DoEditSession(ec)`，在里面做插入/更新。
3. **ITfCompositionSink + StartComposition**：首次字母时 `StartComposition` + 写拼音；后续字母更新文本。
4. **ITfDisplayAttributeProvider**：注册显示属性提供者，给组合串加下划线（这是独立的 TIP 注册）。

**签名已确认**（windows 0.61）：
- `ITfContext::RequestEditSession(tid, pes, flags) -> Result<HRESULT>`
- `ITfContext::StartComposition(ec, range, sink) -> Result<ITfComposition>`
- `ITfRange::SetText(flags, acpstart, acpend, &[u16]) -> Result<TS_TEXTCHANGE>`
- `ITfEditSession_Impl::DoEditSession(&self, ec) -> Result<()>`
- `ITfCompositionSink_Impl::OnCompositionTerminated(&self, ec, Ref<ITfComposition>) -> Result<()>`

---

## 四、关键教训（务必先读，避免重蹈覆辙）

1. **按键状态机必须放在 `OnKeyDown`，不是 `OnTestKeyDown`**——用 `fForeground=TRUE` 注册的接收器，TSF 跳过 `OnTestKeyDown` 直接调 `OnKeyDown`。曾把修饰键检查放错位置，Ctrl+A 修了很多轮才明白。
2. **只认领真正处理的键**：空闲时只认领"无 Shift/CapsLock 的纯字母"，其余（Tab/回车/方向键/符号）全放行；Ctrl/Alt/Win 组合永远放行。
3. **注册必须走 TSF 接口**（`Register` + `RegisterCategory(GUID_TFCAT_TIP_KEYBOARD)` + `AddLanguageProfile` + `EnableLanguageProfile`），只写注册表系统不认。重复注册时这些接口返回 `S_FALSE`，要当成成功（`ignore_s_false`）。
4. **NULL DACL 描述符要用 Box 持有**，否则 `SECURITY_ATTRIBUTES.lpSecurityDescriptor` 指向被 move 的栈变量，ACL 修复形同虚设（cands=0 查了很久）。
5. **DLL 一旦被加载就驻留**（`DllCanUnloadNow=S_FALSE`），改代码后必须：换 DLL 文件名注册 + 重启应用进程，否则跑的还是旧 DLL。
6. **引擎进程是独立生命周期**：崩了不会自启，DLL 连不上管道就静默 `cands=0`。验证前先确认 `Get-Process inklink` 存在。
7. **给 Windows 写含中文的 .ps1 必须带 UTF-8 BOM**，否则 PS 5.1 按 ANSI 解析会崩。
8. **VM 验证**：Hyper-V「Windows 11 开发环境」+ 快照 `tsf-base`；PowerShell Direct 账号 `inklink` / `Ink!2026test`；`Copy-VMFile` 只能 Host→Guest，同名文件会报错，用新文件名。

### 部署命令（在 VM 内）

```powershell
# 推送 DLL 后，注册（改用唯一文件名 + 直接改注册表路径）：
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$dll = "C:\Program Files\inklink\inklink_tsf_$stamp.dll"
Copy-Item 'C:\Users\Public\inklink_tsfXX.dll' $dll -Force
Set-ItemProperty 'HKLM:\SOFTWARE\Classes\CLSID\{8F2A6C1E-4B3D-4F7A-9A21-5C6D7E8F9A0B}\InprocServer32' -Name '(default)' -Value $dll
# 引擎：
Start-Process 'C:\inklink-engine\inklink.exe' -ArgumentList '--serve' -WorkingDirectory 'C:\inklink-engine' -WindowStyle Hidden
```

### 构建命令

```powershell
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"   # 必须先修 PATH（GNU rustc 会盖掉 MSVC）
cd C:\dsh\work\inklink\tsf
cargo build --release --target-dir target-static      # 静态 CRT，DLL 不依赖 vcruntime140
cd C:\dsh\work\inklink\app\src-tauri
cargo build --release --offline                       # 引擎
```

---

## 五、决策记录（TSF 阶段）

| 决策 | 内容 |
|:---|:---|
| ADR-0005 | 面板改为「不抢焦点 + 键盘钩子」（已被 ADR-0006 取代） |
| ADR-0006 | 改用 TSF 输入法：薄壳 DLL + 引擎独立进程，取代面板方案 |
| — | 面板版保留为免安装降级模式 |
| — | T3 上屏先用 SendInput（临时），T4 换 ITfRange::SetText |

## 六、变更日志（TSF 阶段）

| 日期 | 做了什么 |
|:---|:---|
| 2026-10-22 | T1：注册成输入法，出现在列表（关键：键盘分类注册） |
| 2026-10-22 | T2：按键流打通（关键：OnTestKeyDown 返回 TRUE 才触发 OnKeyDown） |
| 2026-10-22 | T3：能打中文（引擎管道 + 候选 + SendInput 上屏 + 错误恢复） |
| 2026-10-22 | 大量踩坑记录见 `docs/tsf-plan.md` T1/T2/T3a 实测记录 |

---

## 七、T4 完成后的现状（2026-10-23 更新）

### 已实现

| 能力 | 说明 |
|:---|:---|
| 组合串显示 | 拼音通过 `ITfComposition` 显示在文档光标处 |
| 组合串内编辑 | 内部光标 + ←→ 移动 + 插入/退格/Delete 在光标处 |
| 候选窗 | 拼音行 + 5 列网格；收起 1×5，展开 5×5（25 个） |
| 候选导航 | 展开后 ↑↓ 换行、←→ 换列、到边界翻页；空格上屏高亮项；1-9 直选 |
| 引擎自愈 | 命名互斥单例 + DLL 激活时自动拉起（`CreateProcessW`） |
| 引擎日志 | `C:\inklink-engine\engine.log`，含 panic 钩子 |

### 已知未做：拼音下划线

拼音现在显示为**普通文字**（没有虚线），所以看起来"不像输入法"。要做成主流样式需要新增：

1. `ITfDisplayAttributeInfo`（5 个方法）—— 描述"下划线 + 前景色"样式
2. `ITfDisplayAttributeProvider`（2 个方法）—— 按 GUID 返回上面的 info
3. `IEnumTfDisplayAttributeInfo`（4 个方法，含 Clone）—— 枚举支持
4. **新的 CLSID** 给 provider，并在 `DllGetClassObject` 里分发；注册时加
   `RegisterCategory(provider_clsid, GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER, provider_clsid)`
   和 `RegisterGUID(我们的属性 GUID)` 拿到 atom
5. 在 Compose 编辑会话里对 range 设属性：
   `context.GetProperty(&GUID_PROP_ATTRIBUTE)` → `ITfProperty::SetValue(ec, range, VARIANT(atom))`

**坑**：`ITfProperty::SetValue` 在 windows-rs 里要有 `Win32_System_Ole` + `Win32_System_Variant`
特性才会出现（vtable 字段被 cfg 掉了）；`ITfRange::Clone` 和 `ITfRange::GetRect` 则完全缺失。

估计 300–350 行，且要改进注册流程（新增 provider CLSID 属于**加性**改动，不动现有 TIP 注册）。

### 其他待办

- 候选窗美化（圆角/间距/配色）
- 引擎崩溃原因排查（日志已就位）
- 各应用兼容（WPS / 浏览器 / VSCode）
- M1 使用反馈排序、M2 个人词库、M3 AI

---

## 八、引擎换成 librime（2026-10-25）

### 为什么换

自研 Ranker 只能做「整词 + 前缀 + 简拼 + 一元词频」，实测打不出：

| 输入 | 自研引擎 | librime |
|:---|:---|:---|
| `jianzoupianfeng` | 见走片风（词库还没这个词） | **剑走偏锋** |
| `jiagjiang`（打错） | 家/加/价/假 | **价格将/价格/架构** |
| `jintiantianqihenhao` | 今天天气很好（碰巧对） | **今天天气很好** |

librime 是 **BSD-3-Clause**，自带音节切分、词图 + Viterbi、八股文语言模型、用户词库。

### 架构

```
inklink_tsf.dll (x64, TSF)  ──命名管道──▶  rime-server.exe (x86, 链 librime)
        │                                        │
   C:\Program Files\inklink\            C:\inklink-engine\
```

**为什么单独进程**：Weasel 分发的 `rime.dll` 是 **32 位**，而 TSF DLL 必须是 64 位。
进程间 32/64 位互不影响——这也正是 Weasel 自己的架构（x86 WeaselServer 服务 x64 前端）。

### 关键实现（`rime-server/`）

- **协议与旧 Rust 引擎完全一致**，DLL 只加了一个消息类型字节：
  `[u8 type][body]`，`1=查询`，`2=选词`
- **`FlushFileBuffers` 必须加**：否则 `DisconnectNamedPipe` 会丢掉客户端还没读的数据
  （表现为客户端只收到 count，收不到候选）
- **每连接一线程 + librime 互斥**：单实例循环会被一个卡住的客户端堵死
- **候选靠 `RimeGetContext` + `{Page_Down}` 收集**：`RimeCandidateList*` 这版返回空文本
- `rime\data\default.yaml` 的 `schema_list` 裁到只剩 `luna_pinyin_simp`，避免每次编译 8 个方案

### 部署

```
C:\inklink-engine\
  rime-server.exe     引擎（开机自启，HKCU Run）
  rime.dll            3.0 MB
  rime\data\          13 MB  词库与配置（54 万词）
  rime\user\          ~44 MB librime 构建产物（首次运行自动生成，约 1-2 分钟）
```

注册表 `HKLM\SOFTWARE\inklink` 的 `EnginePath` → `rime-server.exe`，DLL 会按它自动拉起。

### 已知未做

- **用户词库学习**：librime 这版 `RimeSelectCandidate` 不产生 commit，学不到。
  自建记忆层试过但导致查询崩溃，已回退。详见 `PLAN.md` T5a。
- 拼音音节分隔显示（librime 的 preedit 自带 `jian zou pian feng`，尚未用上）