# TSF 薄壳 + 独立引擎：实施蓝图

> 目标：让 inklink 变成系统里能选中的输入法（语言栏里能切），
> 光标、退格、标点、组合串全部由系统框架处理，
> 但**引擎不进宿主进程**——这是与上一版 `rime.dll` 的根本区别。

---

## 一、最终形态

```
┌──────────────────────────────┐        ┌──────────────────────────┐
│ 目标应用（WPS / 浏览器 / 记事本）│        │  inklink-engine.exe      │
│                              │        │  （现在的引擎，独立进程）  │
│  ┌────────────────────────┐  │  管道  │  ┌────────────────────┐  │
│  │ inklink-tsf.dll        │◄─┼───────►│  │ 词库 8 万条         │  │
│  │  - 注册成 TIP           │  │ 1-2ms │  │ 切分 / 召回 / 排序   │  │
│  │  - 收按键 → 发管道      │  │       │  └────────────────────┘  │
│  │  - 显示组合串 + 候选窗   │  │       └──────────────────────────┘
│  │  - 提交文本             │  │
│  └────────────────────────┘  │
└──────────────────────────────┘
```

- **DLL 只有几百行**，不含词库、不含引擎、不起后台线程
- **引擎崩了**：输入法暂时无候选，系统和其他应用无恙
- **DLL 崩了**：只影响当前这个应用，且可以用 `catch_unwind` 兜住大部分

---

## 二、要写的模块（按依赖顺序）

### 1. COM 骨架（约 300 行）

| 接口 | 作用 |
|:---|:---|
| `DllGetClassObject` / `DllCanUnloadNow` | 标准 COM 入口。**`DllCanUnloadNow` 必须永远返回 `S_FALSE`**（上一版就死在这条） |
| `ITfTextInputProcessorEx` | 输入法主体：`Activate` / `Deactivate` |
| `ITfThreadMgrEventSink` | 跟踪焦点变化（切窗口时重置状态） |
| `ITfKeyEventSink` | **核心**：`OnTestKeyDown` / `OnKeyDown` 接收按键 |
| `ITfCompositionSink` | 组合串被应用取消时的通知 |

### 2. 注册（约 100 行）

- 写 `HKLM\SOFTWARE\Classes\CLSID\{clsid}\InprocServer32`
- 写 `HKLM\SOFTWARE\Microsoft\CTF\TIP\{clsid}\LanguageProfile\0x00000804\{profile}`
- `DllRegisterServer` / `DllUnregisterServer`
- **验证方法**：调用后进「设置 → 语言 → 中文 → 键盘」，能看到 inklink 并加上

> 上一版的坑 #7：`regsvr32` 报成功但列表里没有——多半是 LanguageProfile 子键没写全。

### 3. IPC（约 200 行）

命名管道 `\\.\pipe\inklink-engine`，二进制帧（比 JSON 快）：

```
请求: [u8 kind][u32 len][payload]
  kind=0 查询:  { code: String }
  kind=1 提交:  { text: String }
  kind=2 心跳
响应: [u32 len][payload]
  查询: { matched_len: u32, items: [{text, score}] }
```

- **超时必须设**：`OnKeyDown` 是同步的，超过 **30ms 没回就放行原键**（宁可不出候选，也不能卡住用户）
- 引擎未启动时：DLL 首次请求自动拉起 `inklink-engine.exe`，失败则降级为"透传所有按键"

### 4. 组合串显示（约 250 行，最麻烦的一块）

两种做法，建议分两步：

**A. 不出组合串（第一版）**
- 只弹候选窗，拼音显示在候选窗里（和现在一样）
- 工作量小，但应用里看不到拼音

**B. 应用内组合（第二版）**
- `ITfContext::StartComposition` 拿一个 `ITfComposition`
- 用 `ITfRange::SetText` 写入拼音
- 用 `ITfDisplayAttributeProvider` 加下划线（主流输入法的那个虚线下划线）
- 退格时改 `ITfRange`，而不是改我们自己的字符串

> B 是"看起来像真输入法"的关键，也是唯一能让光标、退格完全正确的做法。

### 5. 候选窗（约 150 行）

- 无边框 `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` 窗口
- 位置：`ITfContextView::GetScreenExt` + `ITfRange::GetRect` —— **这是 TSF 最值钱的地方，位置由应用告诉我们**
- 复用现在的候选条 UI（Svelte 那套可以砍掉，改用原生绘制或保留 WebView）

### 6. 引擎侧改造（约 150 行）

- 现在这个 Rust 程序加一个"管道服务模式"：`inklink-engine.exe --serve`
- 词库、`Ranker`、`decode` 全部复用，**一行都不用改**
- 空闲 10 分钟自动退出，下次请求再拉起

---

## 三、里程碑（建议这么切）

| 里程碑 | 内容 | 验收 | 预估 |
|:---|:---|:---|:---|
| **M-T1** | COM 骨架 + 注册 | 语言栏里能看到 inklink 并切换过去（哪怕不能打字） | 3 天 |
| **M-T2** | 按键转发 + IPC + 透传 | 切到 inklink 后打字，字母照样进文档（证明链路通） | 3 天 |
| **M-T3** | 查询 + 候选窗 + 提交 | 记事本里打 `xiangmu` 出候选，空格上屏「项目」 | 5 天 |
| **M-T4** | 组合串（应用内拼音 + 下划线） | 拼音显示在文档里、退格正确、光标位置正确 | 5 天 |
| **M-T5** | 兼容与兜底 | WPS / 浏览器 / VSCode 三个跑通；DLL 崩溃不拖死应用 | 4 天 |

合计 **约 20 个工作日**，其中 M-T1/2 是"不知道会不会踩坑"的阶段。

---

## 四、风险清单（按严重度）

| 风险 | 影响 | 对策 |
|:---|:---|:---|
| **DLL 崩了拖死宿主** | 又回到上一版 | 全部逻辑 `catch_unwind`；`DllMain` 什么也不做；不启线程；`CanUnloadNow` 恒 `S_FALSE` |
| **按键同步等待卡顿** | 全系统输入卡 | IPC 30ms 超时，超时放行；引擎查询本身 62µs，余量足够 |
| **注册失败/列表不显示** | 装完用不了 | 按上一版坑 #7 检查 LanguageProfile 子键，写注册校验脚本 |
| **32/64 位** | 32 位应用里不可用 | 先只做 x64（你的环境全是 x64），文档写明 |
| **杀软拦截** | 装不上 | 开源可审计 + 后期签名 |
| **调试困难** | 出问题只能猜 | 引擎侧有完整日志；DLL 侧只记错误码；给 DLL 生成 PDB |

---

## 五、和现在的关系

- **引擎、词库、排序、单测：100% 复用**，一行不改
- **现在的面板版**可以保留为"免安装的降级模式"：不想装 DLL 的人还能用
- 候选条 UI 可以复用（WebView 那套要换成原生窗口，否则又变成"进程里跑 WebView2"）

---

## 六、一句话结论

**要做的不是"再写一个输入法"，而是"给现有引擎加一个符合 TSF 规范的前端"**。
难点集中在 COM 胶水和组合串（M-T1、M-T4），其余都是体力活。


---

## T1 实测记录（2026-10-22）

**结果：✅ 通过**——inklink 已出现在 Win+Space 输入法列表里（与微软拼音并列）。

**踩过的坑（按严重度）**：

1. **必须注册「键盘输入法」分类**：`ITfCategoryMgr::RegisterCategory(CLSID, GUID_TFCAT_TIP_KEYBOARD, CLSID)`。
   没有这一步，探针显示"已枚举、可实例化"，但 `Set-WinUserLanguageList` 就是会把 TIP 丢弃。
   这是 T1 卡住最久的原因。
2. **LanguageProfile 注册表子树要交给 `AddLanguageProfile` 写**，不要手写。
   手写会弄错 `IconIndex` 类型（该是 DWORD）和 `Description` 位置，还会和 TSF 自己写的打架。
   以微软拼音 `HKLM\SOFTWARE\Microsoft\CTF\TIP\{81D4E9C9...}` 为对照。
3. **静态链接 CRT**（`-C target-feature=+crt-static`）：干净 VM 没有 `vcruntime140.dll`，
   `regsvr32` 会报退出码 3（无法加载）。静态后只剩系统 DLL。
4. **`regsvr32` 退出码不可信**（32 位 WOW64 重定向、静默吞错误），改用 `LoadLibrary` + 直接调 `DllRegisterServer` 读 HRESULT。
5. **PowerShell 5.1 按 ANSI 读 .ps1**：含中文的脚本必须带 UTF-8 BOM，否则字符串解析崩坏。
6. **VM 需先装 zh-Hans-CN 语言**：纯英文系统没有中文语言条目，TIP 没处挂。
7. **`DllCanUnloadNow = S_FALSE` 副作用**：DLL 一旦被加载就驻留进程、文件锁死，
   迭代必须换 DLL 文件名（v2/v3/v4/v5 就是这么来的）。

**诊断工具**：`tsf/src/bin/probe.rs` —— 枚举语言配置 + 测试能否实例化 TIP，是定位"注册成功但列表不显示"的分水岭。

**部署脚本**：`vm/deploy-v11.ps1`（自诊断、含中文语言补装、被锁自动换名）、`vm/uninstall.ps1`。

## T2 实测记录（2026-10-22）

**结果：✅ 通过**——按键完整流进 `OnKeyDown`，日志实测：

```
DllGetClassObject
ActivateEx
activate tid=12 advise=ok
keydown vk=65 (0x41)   ← A
keydown vk=83 (0x53)   ← S
deactivate
```

**关键坑（浪费了好几轮）**：

1. **`OnTestKeyDown` 必须返回 TRUE**，否则 TSF 根本不会调用 `OnKeyDown`。
   之前的实现返回 FALSE（"我不处理"），导致日志永远空。
   正确语义：`OnTestKeyDown` 返回 TRUE（我要接收），`OnKeyDown` 返回 FALSE（但我不吞，键照常进应用）。
2. **部署脚本用字符串替换生成，替换目标写错会静默失败**——连续几轮一直在复制旧 DLL，
   表现为"能打字但没日志"。教训：部署脚本必须**校验源文件大小**并在复制后比对，不一致直接中止。
3. **DLL 热替换不可行**（`DllCanUnloadNow=S_FALSE` + 进程驻留），每次部署用**时间戳唯一文件名**注册。
4. 诊断日志写 `C:\Users\Public\inklink-tsf-<pid>.log`（固定路径比 `%TEMP%` 好找）。