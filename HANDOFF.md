# 续接文档：inklink

> **最后更新**：2026-10-26 ｜ **当前阶段**：T1–T4 全部完成，日常可用；安装器可用 ｜ **总体进度**：TSF 里程碑 4/5，M1 未开始

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
| T4 | 组合串显示（拼音显示在光标处）+ 候选窗（收起 1×5 / 展开 5×5） | ✅ |
| T5 | 兼容与兜底（引擎进程生命周期、32 位、安装器、标点/英文模式） | ✅ |
| M1 | 用户词库学习（`code -> 词 -> 次数`，自建记忆层） | ⬜ 未开始，**下一步** |
| T5d | 拼音下划线（`ITfDisplayAttributeProvider`） | ⬜ |

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
---

## 九、现状与下一步（2026-10-26 更新）

### 已经能用的（用户本机日常在打）

| 能力 | 说明 |
|:---|:---|
| 中文输入 | librime 引擎（`C:\inklink-engine\rime-server.exe`，x86 链 `rime.dll`），整句/词组/简拼 |
| 组合串 | 拼音显示在光标处，←→ 在串内移动，Backspace/Delete 在光标处删 |
| 候选窗 | 拼音行 + 5 列网格；收起 1×5，展开 5×5（25 个）；**只给选中行标 1-5**，↑↓ 换行、←→ 换列、数字键选当前行 |
| 翻页 | `-`/`=`（收起态 `-` 是分词符，展开态才是翻页）、PgUp/PgDn |
| 分词符 | `-`/`_` 留在组合串里、查询前剥离；`xiang-mu` 显示为 `xiang-mu` |
| 容错 | 拼音打一半也出候选：`zheg`、`zhge`、`xiangm`、`jig`、`shiq` 都能出「这个/项目/几个/事情」（引擎侧 `is_pinyin_code` 允许**一个**音节只打一半）；真英文词（`hello`/`github`/`world`）仍然英文候选排第一 |
| 纠错 | 多打/换位一个字母也能纠：`niihao`→你好、`xiangmuu`→项目、`zhemge`→这个、`shanghhai`→上海、`nihoa`→你好、`jintain`→今天。引擎侧 `edit_variants()` 生成变体（删重复/换位/删一个），`variant_score()` 排掉"单字母音节"那种垃圾切分；末尾半音节（`zheg`/`xiangm`）交给 librime 前缀匹配、不插手 |
| 分段上屏 | 选了一个字后**剩下的拼音保留**继续选（`pikeqiu` 选「皮」→ 串变 `keqiu`）；尾巴上多打的字母按汉字数吃掉 |
| 标点 | `,` `。` `;` `/` `\` `[` `]` 全转中文，Shift 变体 `？：《》「」`；**Shift+1~0 出全角 `！＠＃￥％……＆＊（）`**；**引号按一下开、再按一下合，自动成对 `“”` `‘’`**（`DQUOTE_OPEN`/`SQUOTE_OPEN` 两个全局开关）；**`-` `=` `_` `+` 永远半角不转换**（`halfwidth_symbol()`，Excel 公式 `=A1-B1` 要原样字符），空闲时干脆不认领这两个键 |
| 英文模式 | Shift 单击切（按下不动、松开才生效，期间按过别的键就不算单击）；CapsLock 开 = 大写英文；英文模式下不认领任何键（WPS/Excel 兼容） |
| 32 位 | `inklink_tsf32.dll` 注册在 `WOW6432Node`，WPS 等可用 |
| 模糊音 | 七组可选开关在 `rime-config\fuzzy.yaml`，跑 `tools\apply-fuzzy.ps1` 生成引擎配置并重启验证。**默认全关**（实测开 `zh_z` 会把 `zi` 的首选从「子」顶成「之」） |
| 安装器 | 脚本模板在 `installer\`，打包成 `dist\inklink-<版本>.zip`；`install.cmd` 一键装，带完整日志 `C:\Users\Public\inklink-install.log`，结尾 8 项自检 |
| 单测 | `cd tsf && cargo test --release` → 27 passed（按键认领、标点映射含引号交替、拼音切分、分段上屏消耗字母数） |

### 还没做的（按建议优先级）

| # | 任务 | 为什么现在做 |
|:---|:---|:---|
| 1 | **M1 用户词库学习** | 产品核心卖点（"每人专属词库"）一条都还没有；协议 `type=2` 和 `ipc::select()` 都已埋好，服务端 `select_candidate()` 现在只写日志 |
| 2 | T5d 拼音下划线 | 拼音现在像普通英文，"最不像输入法"的地方 |
| 3 | T5c 候选窗美化 | 圆角/间距/配色 |
| 4 | T5e 个人云词库 + M3 AI | Obsidian 语料 + 翻译/续写，原始愿景，最后做 |

### M1 必须分三步（上次一次性全加导致查询崩溃）

1. 只记录到内存 + 日志，不动排序 —— 确认连续查询不崩
2. 加落盘（`user.tsv`，`code\ttext\tcount`）
3. 才接排序提升（同拼音内按次数降序，`std::stable_sort`）

入口就在 `C:\dsh\work\inklink\rime-server\rime-server.cc` 的 `select_candidate()`。

### 版本号与构建 / 部署 / 打包

**版本格式 `X.V.Z`**（`VERSION` 是唯一真相源）：X = 里程碑（0=M0，1=M1，2=M2，3=M3），
V = 里程碑内阶段序号，Z = 修订号（每次发版 +1；**进入新阶段或新里程碑时归零**，沿用 SemVer 规则）。
例：`0.1.22` = 旧命名的 v22；M1 的第一个版本是 `1.0.0`。换阶段用 `Step-InklinkStage`，换里程碑用 `Set-InklinkVersion 1.0.0`。

```powershell
# 改完代码 → 自增 Z → 构建 64+32 位 → 部署到 C:\Program Files\inklink\inklink_tsf_<ver>.dll
powershell -ExecutionPolicy Bypass -File tools\deploy-local.ps1
# 只重新构建部署，不动版本号 / 只拷已有产物
powershell -ExecutionPolicy Bypass -File tools\deploy-local.ps1 -NoBump
powershell -ExecutionPolicy Bypass -File tools\deploy-local.ps1 -NoBump -NoBuild
# 打发布包 → dist\inklink-<ver>.zip（脚本从 installer\ 注入版本号，排除个人词库）
powershell -ExecutionPolicy Bypass -File tools\release.ps1
```

DLL 被加载后无法卸载：脚本每次用**新文件名 + 改注册表**，目标程序要重启才会用上新 DLL。
手动构建的话：`cd tsf` → `cargo build --release --lib --target-dir target-static`（64 位）/
`cargo build --release --lib --target i686-pc-windows-msvc --target-dir target-static`（32 位）。

### 踩过的坑（本阶段新增）

- `git commit -m` 带中文多行会被 PowerShell 拆坏（`error: unknown switch /`）→ 用 `git commit -F msg.txt`，文件 UTF-8 无 BOM。
- `.ps1` 必须 UTF-8 **带 BOM**；`.cmd` 必须**纯 ASCII + CRLF**（cmd.exe 按 OEM 代码页读，`goto` 在 LF-only 文件里会失效）。
- `net session` 判管理员不可靠（Server 服务被禁就恒失败）→ 用 `fltmc`。
- 拼写中按 `-` 的语义取决于是否展开：展开=翻页，收起=分词符。判定前必须先把 `expanded` 的 MutexGuard 放到块里释放，否则再 lock 会死锁。
- **引号开合状态要用 `fetch_xor(true)` 翻转，不能用 `swap(false)`**：后者每次都写成 false，
  于是第一下左、之后永远右，引号再也开不回来（0.1.27 的 bug）。单测当时只断言了"两下一开一合"，
  正好覆盖不到第三下 —— 状态机测试至少跑三个周期。
- **焦点不在可编辑区域时 TSF 照样把按键送进来**（我们是 `fForeground=TRUE` 的接收器），
  以前会在没光标的地方飘出候选窗。现在 `TextService_Impl::can_compose(pic)` 双向判定
  （`pic` 为空时再问 `ITfThreadMgr::GetFocus()` → `ITfDocumentMgr::GetTop()`），
  不成立就一个键都不认领；候选窗也去掉了 `(200, 200)` 坐标兜底——拿不到可信坐标直接不显示。
- **rime 的 custom.yaml 文件名必须逐字等于真正 deploy 的 schema id**：我们跑的是 `luna_pinyin_simp`，
  早先写的 `luna_pinyin.custom.yaml` 从来没被读过（这是模糊音"改了没效果"的真凶之一）。
- **改 `speller/algebra` 要整段替换**：`speller/algebra/__append` 是无效写法，正确是
  `patch: speller/algebra: __patch: [...]`，且模糊音片段必须排在 `pinyin:/abbreviation` 之前。
- **改了 rime 配置必须重启 `rime-server`**：librime 只在 `RimeInitialize` 时发现配置变新并 deploy，
  文件写进去不会立刻生效（`tools\apply-fuzzy.ps1` 会重启 + 等 deploy + 用管道验证）。