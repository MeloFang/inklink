# 续接文档：inklink

> **最后更新**：2026-11-06（1.1.43） ｜ **当前阶段**：M0/M1 完成，产品化 P0–P1、P3、P4、P5 已做完 ｜ **总体进度**：M0 ✅ M1 ✅ P1 ✅ P2a ✅ P3 ✅ **P4a ✅ P4b ✅（1.1.38）** P5 ✅（仓库已公开，CI 出过 v1.1.27/28/29）｜ 下一步 **M2 切片 1**（笔记术语进候选，ADR-0009 + PLAN），P2b 设置台看后面还要不要 ｜ **已知问题**：单次查询引擎侧 ~2ms（带 app 段），老客户端 15ms；AI 失败静默、无进度提示，选区 >2000 字不处理

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
| M1 | 用户词库学习（`码 → 词 → 次数`，自建记忆层；误选撤回、整句学习、跨应用记账、分词边界） | ✅ 1.0.13 |
| M1 续 | 组合串按音节显示分隔符（`xian` → `xi'an`） | ✅ 1.0.14 |
| M2 | Obsidian 语料（用户笔记进词库） | ⬜ 未开始（P6） |
| M3 | AI（离线词库体检、润色/续写/翻译） | ⬜ 未开始（P4 壳 / P6 实体） |
| T5d | 拼音下划线（`ITfDisplayAttributeProvider`） | ✅ 1.0.15 |
| P0 | 全局状态（中英 + 中文标点开关，注册表） | ✅ 1.0.17 |
| P1 | 浮条（中/英、标点、☰、拖动记忆；1.1.10 起由 DLL 在宿主进程内自绘，去托盘/开机自启） | ✅ 1.1.0（1.1.10 改进程内） |
| P3 | 配置外置 + 候选窗暗色（`HKCU\Software\inklink\Config`；列数/行数/撤回窗口/学习开关） | ✅ 1.1.21 |
| P5 | 发布工程化（一键取 librime、裁 data、引擎编译脚本、CI 出包、单文件安装器） | ✅ （CI 未实跑：仓库还没 remote） |

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
8. **VM 验证**：Hyper-V「Windows 11 开发环境」+ 快照 `tsf-base`；PowerShell Direct 用本地测试账号（口令不入库，见你自己的密钥管理器）；`Copy-VMFile` 只能 Host→Guest，同名文件会报错，用新文件名。

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
cargo test --release --target-dir target-static       # 122 个单测

# 引擎（x86 C++，链 Weasel 的 rime.dll）
.\tools\build-engine.ps1 -Dest C:\inklink-engine

# 从零铺引擎数据（下载 Weasel 0.17.4 + 校验 SHA256 + 裁 data + 铺 rime-ice 词库）
.\tools\fetch-librime.ps1 -Dest C:\inklink-engine -DownloadLexicon

# 出发布包（zip + 单文件安装器 + SHA256SUMS.txt + RELEASE-NOTES.md）
.\tools\release.ps1
```

> `release.ps1` 里编安装器那段必须先把 `~\.cargo\bin` 放 PATH 最前面。本机同时装着 GNU 和
> MSVC 两套 Rust，不修 PATH 会挑到 GNU 那套，编 `windows-sys` 时要 `dlltool.exe`（MSVC 里没有）
> 直接失败；报错长这样：`error calling dlltool 'dlltool.exe': program not found`。

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
| 组合串 | 拼音显示在光标处，**音节之间带 `'` 分隔**（`xiangmu` → `xiang'mu`、`jintiantianqi` → `jin'tian'tian'qi`、`zheg` → `zhe'g`，和搜狗一致；见 `pinyin::compose_view`），←→ 在串内移动，Backspace/Delete 在光标处删；**拼音下面有一条橙色点状下划线**（`display.rs`：`ITfDisplayAttributeProvider` + `GUID_PROP_ATTRIBUTE`，上屏前清掉） |
| 候选窗 | 拼音行 + 5 列网格；收起 1×5，展开 5×5（25 个）；**只给选中行标 1-5**，↑↓ 换行、←→ 换列、数字键选当前行；**鼠标左键点哪格选哪个**（窗口自己处理 `WM_LBUTTONDOWN`，`hit_index()` 算格子，回调把下标交给 `lib.rs` 的 `handle_cand_click`）；**靠近屏幕底部自动翻到光标上方**，右侧放不下自动左移（`candwin::place`，按显示器可用区算，已扣任务栏） |
| 翻页 | `-`/`=`（收起态 `-` 是分词符，展开态才是翻页）、PgUp/PgDn |
| 分词符 | `-`/`_` 留在组合串里、查询前剥离；**显示时按音节插 `'`**（`xi-an` → `xi'an`，用户给的边界优先保留） |
| 容错 | 拼音打一半也出候选：`zheg`、`zhge`、`xiangm`、`jig`、`shiq` 都能出「这个/项目/几个/事情」（引擎侧 `is_pinyin_code` 允许**一个**音节只打一半）；真英文词（`hello`/`github`/`world`）仍然英文候选排第一 |
| 纠错 | 多打/换位一个字母也能纠：`niihao`→你好、`xiangmuu`→项目、`zhemge`→这个、`shanghhai`→上海、`nihoa`→你好、`jintain`→今天。引擎侧 `edit_variants()` 生成变体（删重复/换位/删一个），`variant_score()` 排掉"单字母音节"那种垃圾切分；末尾半音节（`zheg`/`xiangm`）交给 librime 前缀匹配、不插手 |
| 分段上屏 | 选了一个字后**剩下的拼音保留**继续选（`pikeqiu` 选「皮」→ 串变 `keqiu`）；尾巴上多打的字母按汉字数吃掉 |
| 标点 | `,` `。` `;` `/` `\` `[` `]` 全转中文，Shift 变体 `？：《》「」`；**光标前一个字符是数字时 `.` `,` `:` 出半角**（`3.14` `1,000` `10:30`，判断在编辑会话里用 `preceding_char_is_digit()` 做，因为按键线程看不到文档内容）；**Shift+1~0 出全角 `！＠＃￥％……＆＊（）`**；**引号按一下开、再按一下合，自动成对 `“”` `‘’`**（`DQUOTE_OPEN`/`SQUOTE_OPEN` 两个全局开关）；**`-` `=` `_` `+` 永远半角不转换**（`halfwidth_symbol()`，Excel 公式 `=A1-B1` 要原样字符），空闲时干脆不认领这两个键 |
| 英文模式 | **全局的，不是每个应用各一份**（存 `HKCU\Software\inklink\State` 的 `Chinese`，DLL 每次按键现读、不缓存不起线程，见 `state.rs` 与 ADR-0007）；Shift 单击切（按下不动、松开才生效，期间按过别的键就不算单击）；CapsLock 开 = 大写英文；英文模式下不认领任何键（WPS/Excel 兼容）。**"按过别的键"要在 `OnTestKeyDown` 里记**，因为 `Shift+=`、`Shift+-` 这类我们根本不认领的键没有 `OnKeyDown` |
| 32 位 | `inklink_tsf32.dll` 注册在 `WOW6432Node`，WPS 等可用 |
| 模糊音 | 七组可选开关在 `rime-config\fuzzy.yaml`，跑 `tools\apply-fuzzy.ps1` 生成引擎配置并重启验证。**默认全关**（实测开 `zh_z` 会把 `zi` 的首选从「子」顶成「之」） |
| **个人词库（M1）** | 你选过的词会被记住并顶到候选前面：`xiangmu` 选一次「橡木」→ 以后 `橡木` 排第一。存在 `%LOCALAPPDATA%\inklink\user-words.tsv`（`码\t词\t次数`，UTF-8），重启不丢，最多 4000 组码自动剪枝。分段上屏按**选中字数**反推学到哪个码（`pikeqiu` 选「皮」记在 `pi` 下，不是 `pikeqiu`）。**选错了就把字删掉**：上屏后 2 秒内把刚上屏的几个字全删光，这次学习自动撤回；单字不学；`--review-dict` / `--prune-single` / `--forget` 可以巡检和清理。**整句也学**：分段选完一句话（≥2 次选择、≥4 字）会按整段全拼再记一条，下次打同样的全拼整句候选往前排；**删掉整句也会撤回**（逐词和整句是两条独立的账）。**按应用分开记账**：同一个拼音在微信里和 VSCode 里可以各记各的（`boost_score = 全局 + 本应用×2`），词库文件第四列是应用名。**分词边界也是信息**：打 `xi'an` 选「西安」只记在 `xi'an` 下（`-` 会自动换成 `'`），下次打 `xi'an`/`xi-an` 西安排第一，打 `xian` 仍然是「先」 |
| 安装器 | 脚本模板在 `installer\`，打包成 `dist\inklink-<版本>.zip`；`install.cmd` 一键装，带完整日志 `C:\Users\Public\inklink-install.log`，结尾 8 项自检 |
| 单测 | `cd tsf && cargo test --release` → 111 passed（按键认领、标点与引号交替、半角符号、数字后半角、Shift 组合键、拼音切分、音节分隔显示、分词边界、分段上屏、误选撤回、整句上报、候选窗摆放与点击、注入退格、显示属性、全局状态、浮条与菜单布局 / 绘制 / 圆角抗锯齿、品牌标取帧、浮条可见性仲裁（模拟宿主 + 穷举序列）、标点横向错开） |

### 还没做的（按建议优先级）

| # | 任务 | 为什么现在做 |
|:---|:---|:---|
| 1 | 设置台（P2，ADR-0007） | 浮条已就位（1.1.10 改进程内），点 ☰ 还是灰的；设置台是挂在浮条上的窗口，六页：输入/候选窗/模糊音/词库/AI/关于 |
| 2 | T5c 候选窗美化 | 圆角/间距/配色（并入 P3：做成可配置项） |
| 3 | 用户词库管理界面 | 现在只有命令行 `--review-dict`，桌面上没法看/删（放进上面那个设置台） |
| 4 | M2 Obsidian 语料 | 读用户笔记补专有名词（`extractor/` 有 v1 管线可复用） |
| 5 | M3 AI（离线）/ T5e 个人云 | 词库体检、润色/续写/翻译，原始愿景，最后做；AI 配置也放设置台 |

> **全局状态已就位（1.0.17）**：`HKCU\Software\inklink\State` 的 `Chinese` / `Punctuation`。
> **浮条已就位（1.1.0，1.1.1 起"用时才出现"；1.1.10 起由 DLL 在宿主进程内画，不再有 `inklink-ui.exe`）**：右下角可拖动，点「中/英」和「，。」两个方块切换状态。

- **浮条第一格放品牌标记**（1.1.3）：`installer\inklink-badge.ico`（透明底、字形撑满的字母 i），点它等同点 ☰；窗口 168→204px。
- **点浮条不会立刻消失**（1.1.3）：`Deactivate` 的收起请求会被"最近 1.5s 点过 / 鼠标还在浮条上"挡掉一次，否则点一下就没。
- **☰ 菜单是自绘的**（1.1.9，修"点一下变一条白带"）：窗口类 `inklink-menu`。系统的 `TrackPopupMenu` 在我们这种拿不到前台的 `WS_EX_NOACTIVATE` 窗口上会**永不返回**（模态循环卡死消息循环，随后 `user32.dll` 里 `0xc0000005`/`0xc000041d`）。菜单内选中走 `WM_LBUTTONUP`，`Esc`/点外面走 10ms 轮询；`row_at()`/`menu_size()` 有单测。
- **浮条改进程内**（1.1.10，ADR-0008）：`tsf\src\bar.rs`（`inklink-bar`）+ `tsf\src\menu.rs`，删掉 `tsf\src\ui_notify.rs` 与整个 `ui\` exe。`Activate`/`ActivateEx` → `bar::show()`、`Deactivate` → `bar::hide()`。菜单去「开机自启」「退出浮条」；`panic="abort"` 下必须 panic-free，菜单 `popup()` 的 `PeekMessageW` 只按菜单 hwnd 过滤、无 `std::process::exit`。
- **进程内浮条不能"一个进程一条"**（1.1.14，修"桌面上并排三条浮条"）：TSF 是**按线程**激活的，
  一个进程里几条线程各 `Activate` 一次就各建一个窗口；而 `Deactivate` 只在"这条线程不再用这个输入法"时来，
  用户 Alt+Tab 切程序**不会**触发它。两个修法：① 窗口句柄放 `thread_local!`（每条线程只看得见自己那条）；
  ② 600ms 看门狗（`TIMER_WATCH`）：不在前台就自己收起，回到前台再复活。
  （"同进程只留一条"在 1.1.14 是靠"先查有没有别的可见浮条"做的，还会偶尔冒出两条，2026-11-01 改成原子仲裁，见下一条。）
- **同一进程最多一条浮条**（2026-11-01，**已部署 1.1.20 / 提交 `014fc3b`**）：1.1.14 之后仍偶尔同时出现两条。根因是"想不想显示"（`WANTED`）
  和"是不是被看门狗收起的"（`AUTO_HIDDEN`）是进程级的，一条线程置了"被收起"，别的线程回到前台都绕过去重各放一条。
  现在实例状态按线程（`INSTANCE_VISIBILITY`），显示由进程级 `VISIBLE_OWNER` 的一次比较交换授权，收起时先隐藏、
  确认不可见再释放；枚举上下文放栈上、按 PID 过滤。编排入口 `drive_visibility`，有内存模拟宿主 + 穷举序列单测。
  同一轮里第三格的句号左移、逗号右移各 1 个设计像素（× DPI），不再上下对齐。
  细节、测试清单和**待做的手测清单**见 `PLAN.md`「修"偶尔同时冒出两条浮条"」一节。
- **配置外置 + 候选窗暗色**（2026-11-01，**已部署 1.1.21**；`Dark` 覆盖三处外观见 1.1.22）：用户调外观不用再改代码。配置放
  `HKCU\Software\inklink\Config`（不是 `ui.yaml` —— 这些值在按键路径上读，注册表读取原子且微秒级，
  不必引解析依赖、也没有"文件写一半"），DLL 每次现读、不缓存不起线程。五个值：`Dark`（候选窗暗色，
  缺省浅色）、`Cols` 3–9（列数，也是收起时每页条数）、`Rows` 1–9（展开行数）、`UndoMs` 0–10000
  （误选撤回窗口，0 = 完全不记账）、`Learn`（0 = 只查不学，**缺省开**）。默认值与之前写死的常量一致，
  升级不改变手感。用 `tools\config.ps1` 改，改完下一次按键生效，不用重启。**1.1.22 起 `Dark` 同时管
  候选窗、浮条、☰ 菜单三处**（暗色的激活色取近白，深蓝在深底上看不见；浮条靠 600ms 看门狗自己变色）。
  实现见 `tsf/src/config.rs`；说明见 `PLAN.md`「P3：配置外置 + 候选窗暗色」。
- **浮条标点格上下叠放、符号自己画**（1.1.14）：像搜狗那样上面句号、下面逗号；全角标点的墨迹只占字框底部，
  15px 字体画出来是一粒灰点，所以 `draw_ring`/`draw_comma` 用 GDI 圆/多边形直接画（和 ☰ 同一个思路，不依赖字体）。
> 所有应用同时切换（状态在注册表里，DLL 每键现读）。
> 显隐由 TSF 的 `Activate`/`Deactivate` 驱动——切到这个输入法才出现，切走自动收起，不再一直挂在角落。

### M1 个人词库学习（已实现，2026-10-27）

学的是**用户最终选了哪个词**，不是"出现过什么"。三步都做了，并且是按顺序验证过的
（上次一次性全加导致查询崩溃，这次每步都单测 + 端到端验证）：

1. 记录到内存 + 日志 —— 连续查询不崩
2. 落盘 `%LOCALAPPDATA%\inklink\user-words.tsv`（`码\t词\t次数\t应用名`，写 `.tmp` 再 `MoveFileExA` 原子替换）
3. 接排序提升：`query()` 返回前 `apply_user_boost()` 按次数 `std::stable_sort`

关键设计：

- **协议 v3**：选词上报要带 `code` 和 `word`（`[u8 2][u32 index][u32 code_len][code][u16 word_len][word]`）。
  原来只发下标 —— 引擎跨连接记"上次查询"在多应用并发时会串，下标脱离"当时查的是什么"没有意义。
  旧 DLL 只发 5 字节时引擎打 `select %u (no code: old protocol)` 照常回响应，不断连接。
- **学到哪个码**由选中字数决定（`learn_key()` 用贪心音节切分）：`pikeqiu`+「皮」→ `pi`，
  `pikeqiu`+「皮克」→ `pike`，`xiangmu`+「项目」→ `xiangmu`；英文那种切不动的记整串。
- **打英文不算选中文候选**：`word == strip_separators(code)` 时跳过。
- **存储位置不能在 `C:\inklink-engine\`**（普通用户没有写权限），也不该混进 rime 的 `user_dir`
  —— 那是引擎的配置区，用户词库是我们的产品数据。
- 剪枝：`USER_MAX_CODES = 4000`，超了先丢"最高次数≤1"的（用过一次就再没选过的）。
- `std::mutex g_user_mutex`：每连接一个线程，学习和查询会并发。
- `rime-server.exe --selftest` 有 32 项纯 ASCII 断言（`utf8_char_count` / `strip_separators` /
  `normalize_boundaries` / `split_lengths` / `learn_key` / `should_learn` / `boost_score`），
  不初始化 librime、不碰管道，CI 或排障时直接跑。

入口：`C:\dsh\work\inklink\rime-server\rime-server.cc` 的 `select_candidate()` → `learn()`。


#### 跨应用权重区分（2026-10-28 补）

以前所有应用共用一套词库：微信里养出来的习惯会影响写代码时候选。现在按**宿主 exe 名**分开记账。

- DLL 侧 `ipc::app_tag()`：`GetModuleFileNameW(null)` 取宿主进程路径 → 去 `.exe`、转小写
  （`weixin` / `code` / `notepad`），`OnceLock` 缓存一次。取不到就是空串（未知应用）。
- **协议 v5**：type 1/2/3 三种报文末尾**追加可选** `[u16 app_len][app]`。引擎先 `wait_readable(p, 100)`
  探一下，没有就当空串 —— 老 DLL 一个字节都不用改，也不会挂（1.0.1 那次死锁的教训）。
- 存储：`UserWord { total, apps }`，文件每条 `码\t词\t次数\t应用名`，**每个 (码,词,应用) 一行**；
  `total` 不单独存，读盘时求和。老的三列文件读进来就是"未知应用"，照样能用。
- 排序分：`boost_score(w, app) = w.total + w.apps[app] * 2`（本应用次数双倍）。
  只算本应用等于新应用里词库全关；只看全局又退回混在一起。
- 撤回也分应用：先扣当前应用，当前应用没记过就扣次数最多的那个。
- `--review-dict` 会打印分应用明细：`xiangmu -> 橡木  x2  {weixin:1,code:1}`。

实测（管道带 app）：`code` 里选「项目」、`weixin` 里选「橡木」之后，
`code` 查到 `项目|橡木|…`，`weixin` 查到 `橡木|项目|…` —— 同一份词库两个应用各排各的。


#### 分词边界学习（2026-10-28 补）

`xian` 是一个音节（先），`xi'an` 是两个（西安）。用户主动打边界就是在说"按 xi + an 切"，
所以我们**不再把它抹掉**：

- DLL：`normalize_boundaries()` 把 `-` / `_` 统一成 `'` 再发给引擎（`xi-an`、`xi_an`、`xi'an` 等价）；
  引擎内部才 `strip_separators()` 还原成 `xian` 喂 librime。
- 记账键带边界：`learn_key("xi'an", "西安") == "xi'an"`、`learn_key("xiang-mu", "项目") == "xiang'mu"`，
  分段上屏的 `pikeqiu + 皮` 仍然是 `pi`。
- 排序时两个键都看：带边界 ×4、抹掉边界 ×1。所以 `xi'an` 下学过的「西安」只在打 `xi'an` 时赢，
  打 `xian` 仍然是「先」。学过的词若 librime 没给，直接补到最前（最多 3 个）。
- 打了边界就不再追加"原始字母"候选 —— 用户已经在说这是拼音了。

实测：`xi'an` + 西安 学一次 → `xi'an` / `xi-an` 首候选都是「西安」，`xian` 仍是「先」。

⚠️ **librime 不认 `'` 当分隔**：`xi'an` 和 `xian` 喂进去候选集完全一样（schema 里的
`delimiter: " '"` 只在真实按键流里生效），所以边界只能靠我们自己记账。


#### 误选撤回（2026-10-27 补）

词库以前只加不减，选错的词会一直污染排序（实测 70 条里混了 `wojude -> 我局的` 这种）。
现在的判据是**行为信号**：上屏后 2 秒内把刚上屏的字全删光 = 选错了。

- DLL 侧 `CommitMark`（`tsf/src/lib.rs`）：提交时记下 `(码, 词, 剩余退格数, 2 秒期限)`，
  非拼写态按退格就递减，删光时发 `ipc::unlearn()`，然后**把退格放行**（`Ok(BOOL(0))`），
  所以用户看到的行为和平时打通格一样。
- 引擎侧 `unlearn()`（协议 v4 `type=3`）：计数 >1 就减 1，否则删条目。
- 不用 `ITfTextEditSink` 的理由：要新增编辑观察者 + 跟踪 range，出错就是挂死（见 1.0.1 选词死锁）。
- 单字不学（`USER_MIN_CHARS = 2`）：单字由语言模型排序，学一遍只会跟模型打架。
- **删字的三级级联**（`EditAction::DeleteBack`）：认领了退格就得自己把字删掉，但"怎么删"没有一条路对所有宿主都管用，所以三级依次试、**每级都要确认真的删掉了**：
  1. `ITfRangeACP::GetExtent/SetExtent` —— 最干净，给锚点和长度，没有歧义。
  2. `Collapse + ShiftStart` —— 有些宿主返回 S_OK 却一步没动，必须检查 `shifted != 0`。
  3. 注入原生 `VK_BACK`（`ipc::press_backspace`）—— WPS 正文控件在 ① 里 `GetExtent` 给 `(0,0)`、在 ② 里原地不动，只剩这条路。注入前用 `mark_injected_backspace(n)` 记次数，`OnTestKeyDown`/`OnKeyDown` 见到就把键放行给应用，否则会把自己的注入键再认领一次。
  注入标记有 1 秒超时：宿主如果把注入键吞掉，不能让标记一直挂着把用户真正的退格当注入键放行。

#### 整句学习（2026-10-28 补）



逐词学习只能记住"这个词选过"，记不住"这句话我习惯这么打"。



- DLL 侧 `SentenceAcc`（`tsf/src/lib.rs`）：第一次选词时记下当时**完整的拼音串**，

  之后每次选择把词拼到 `text`、`picks += 1`；这串拼音打完就 `ipc::select(0, 全拼, 整句)`。

- 上报门槛 `should_report_sentence(picks, chars)`：`picks >= 2 && chars >= 4`。

  只选一次的不报——那一次本身已按整串记过，再报会让计数翻倍。

- 复用 `type=2`，不新增协议；引擎 `learn_key()` 按字数切音节，四个字正好吃掉整串全拼。

- 两边都记（词码 + 全拼码）不冲突：`apply_user_boost()` 只加权命中当前码的条目。

- 未完成的记录在新起一句 / Esc / 退格删空 / 英文上屏 / 组合被外部终止 / 失焦时清掉。

- **整句的撤回**：逐词和整句记两条独立的账（commit_mark / sentence_mark），每按一下退格两边各减一个字；删掉最后一个词只撤那个词，一直删到整句没了就再撤整句那条。逻辑抽在 n step_mark(slot: &mut Option<CommitMark>) 里，能直接单测。

清理旧噪音：

```powershell
rime-server.exe --review-dict          # 按次数升序列出全部条目，单字带 <- single char
rime-server.exe --prune-single         # 删掉所有单字条目
rime-server.exe --forget wojude 我局的  # 删某一条（先停引擎，否则内存里的旧表会写回来）
```
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

- **窗口按线程，"要不要显示"也必须按线程；"先查有没有、再显示"本身就是竞态**（2026-11-01 修"偶尔两条浮条"）：
  1.1.14 把窗口句柄放进了 `thread_local`，但 `WANTED` / `AUTO_HIDDEN` 还是进程级的，复活条件是
  `AUTO_HIDDEN || !any_other_bar_visible()` —— 任一线程在后台置了 `AUTO_HIDDEN`，几条线程的看门狗回到前台
  都直接放行。就算去掉这个短路，"两条线程同时查到没有、各自显示"仍然会发生，枚举结果放进程级 `ANY_BAR`
  还会被另一次枚举覆盖。正确做法是由**一次原子比较交换**授权（`VISIBLE_OWNER`，`-1` 哨兵覆盖
  "收起别的 → 绘制 → 显示 → 发布"全程），枚举只做清理和核验。这类问题光看真机复现很难，
  先把编排抽成纯函数 + 内存模拟宿主（能在"握着哨兵重绘时"插入另一条线程的请求），再穷举短序列，比手测可靠得多。
  附带：`cargo test` 进程一般不在前台，按新设计后台激活不显示，冒烟测试要用 `show_with_foreground(|| true)`。
- **想要圆角平滑就不能用 `SetWindowRgn`**（1.1.18 修，用户原话"四个圆角锯齿很粗糙"）：窗口区域只有
  "在/不在"两种，`CreateRoundRectRgn` 裁出来的角是一圈台阶，`RoundRect` 画的 1px 描边在角上也只剩
  几个孤立像素。浮条现在是 `WS_EX_LAYERED`：整条画进 32 位 DIB，最后按圆角矩形距离场统一算 alpha
  和描边（`apply_frame`），`UpdateLayeredWindow(ULW_ALPHA)` 交给系统合成。两点要记住：
  ① GDI 往 32 位 DIB 画字/图标**不管 alpha 字节**，所以 alpha 只能在最后一步统一写；
  ② 分层窗口不走 `WM_PAINT`，改了状态要调 `redraw()` 同步重画；显示前必须先 `redraw` 一次，
  否则窗口是空的。高度同时从 44 收到 36（`PAD` 6→4、`ITEM_H` 32→28），句号外径 8.7→6.4px。
- **小字号彩色文字不要用 `CLEARTYPE_QUALITY`**（1.1.16 修）：`CreateFontW` 传 `5`（ClearType，
  子像素抗锯齿）时，17px 的「中」「英」在笔画边缘会拉出橙/蓝色边，用户的原话是"字发花、很脏"。
  改成 `ANTIALIASED_QUALITY = 4`（灰度抗锯齿）后立刻干净。ClearType 适合白底黑字的大段文本，
  **不适合十来像素、还带颜色的界面文字**。标点格同理 —— 想过用字体画「。」「，」，
  14px 下就是两粒看不见的灰点，最后是 GDI 直接画形状（`draw_ring`/`draw_comma`），和 ☰ 一样不依赖字体。
- **空字符串的 `Vec::as_ptr()` 是悬垂指针（值 = 2），`DrawTextW` 会真去读它 → 宿主进程 AV**
  （1.1.11 修）。`text.encode_utf16().collect::<Vec<u16>>()` 在空串时是零长 `Vec`，
  `as_ptr()` 返回 `align_of::<u16>() = 2`；`DrawTextW(hdc, ptr, 0, …)` **并不会**因为
  `cchText == 0` 就不碰指针，它读 `0x2` 就 AV：`c0000005`、`ExceptionInformation[1] = 0x2`、
  报在 `user32.dll+0x24702`，从窗口回调逃出去升级成 `c000041d`（= `STATUS_FATAL_USER_CALLBACK_EXCEPTION`）。
  浮条第 0 格（品牌格）画的是图标，hover 时用**空串**调 `draw_item` 画高亮底 → 一悬停就崩，
  用户看到的是"白条 + 一个灰方块"（背景和第一格画完了，后面的没画）。
  修法：`encode_utf16().chain(once(0)).collect()` 再传 `len-1`（`menu.rs` 的 `wide()` 一直是对的）。
  **取证方法**：`%LOCALAPPDATA%\CrashDumps\*.dmp` 用 `tools\parse-dump.py` 解析，
  异常流的 `ExceptionInformation[1]` 直接给出访问的坏地址 —— 比猜"哪个 API 崩了"快得多。
  **另一个更隐蔽的坑**：`UpdateWindow` 只在窗口有无效区域时才同步发 `WM_PAINT`，
  上一次 `BeginPaint` 已经把区域验证掉了，所以"改状态 → `UpdateWindow`"的循环是空转的，
  测试全绿但宿主一悬停就死；回归测试必须先 `InvalidateRect` 再 `UpdateWindow`。
- **`MoveWindow` 的 `nWidth/nHeight` 是新尺寸，传 0 会把窗口缩成 0x0**（浮条"拖一下就不见了"）。
  拖动时要原样传回 `window_size()`。
- **绝对不要用 PowerShell 的 `Get-Content`/`Set-Content` 改中文源码**（本阶段真把 `tsf/src/bar.rs`
  整份毁掉过一次）。PS 5.1 的 `Get-Content` 按 ANSI(GB18030) 解码 UTF-8，中文全变 `娴潯锛?`，
  有些字节序列还会把紧跟的换行一起吃掉，于是文件里两行并成一行、cargo 报
  `error: unexpected closing delimiter: '}'`。只改代码就用 `edit`/`write` 工具；
  万一手滑了，`git checkout -- <file>` 回到上次提交再逐条重做，比修乱码快。
  附带坑：`[IO.File]::ReadAllBytes('相对路径')` 用的是 .NET 的 CWD（`C:\dsh\work`），
  不是 PowerShell 的 `cd` 目录 —— 一律传绝对路径。
- **`tools\deploy-local.ps1` 不能对 stderr 做任何重定向**（`2>&1`、`2>$null` 都不行）。
  脚本第 16 行是 `$ErrorActionPreference = 'Stop'`，而对脚本调用做 stderr 重定向后，
  `cargo` 写到 stderr 的 `Compiling …` 会被包成 `ErrorRecord` → 升级成终止错误 →
  脚本在 `cargo test` 那一行 exit 1（看起来像测试失败，其实测试根本没跑完）。
  只接 stdout：`& .\tools\deploy-local.ps1 -NoBump | Select-Object -Last 22`。
- **`release.ps1` 别把 `installer\` 里的二进制当文本改写**：它原本遍历整个 `installer\` 做版本号替换，
  图标 `inklink.ico`/`inklink.png` 也被 `ReadAllText` + `WriteAllText` 过了一遍，结果包内出现 73 KB 的
  坏 ico、168 KB 的坏 png。现在只遍历 `.cmd/.ps1/.txt/.md`，二进制由 `Copy-Item` 单独拷。
- `git commit -m` 带中文多行会被 PowerShell 拆坏（`error: unknown switch /`）→ 用 `git commit -F msg.txt`，文件 UTF-8 无 BOM。
- **`argv` 是 ANSI 编码**（中文系统上是 GBK），命令行传中文（`--forget <码> <词>`）会匹配不上
  UTF-8 存的词 → 用 `argv_to_utf8()` 转换（`MultiByteToWideChar(CP_ACP)` + `WideCharToMultiByte(CP_UTF8)`）。
- **误选撤回靠数退格，不靠 `ITfTextEditSink`**：后者要新增编辑观察者 + 跟踪 range，出错就是挂死。
- **原始字母当不当第一个候选，要看"像不像在打拼音"**：`is_pinyin_code` 说不像，不代表用户想打英文。
  现在再加一条判据 —— 前缀能连着切出 ≥2 个完整音节就是拼音（`shashahs` → `sha|sha|…`），
  这时中文候选排前、原始字母排最后；`github`/`hello`/`test` 前缀连一个音节都凑不出两个，仍排第一。
- **`OnTestKeyDown` 认领过的键，应用很可能收不到**，所以"认领但不处理（`OnKeyDown` 返 FALSE）"这种
  写法等于把键吃掉：撤回窗口里连按退格会出现"前两下没反应、第三下才删掉一个字"。要么别认领，
  要么认领了就自己把副作用做完（`EditAction::DeleteBack` 自己删字并返回 `BOOL(1)`）。
  异步编辑会话只有一个 `pending` 槽，连按两下会互相覆盖 → 次数要攒在计数器里
  （`pending_backspace`），会话一次删完。
- **"Shift 单击切英文"的判断依据不能在 `OnKeyDown` 里记**：`Shift+=`（打 `+`）、`Shift+-`（打 `_`）、
  `Shift+Tab` 这些我们**不认领**的键根本不会进 `OnKeyDown`，`shift_used` 一直是 false，
  松开 Shift 就被当成"单击切换" → 打个 `+` 输入法就变英文了。要在 `OnTestKeyDown` 里就记
  （`is_shift_chord(vk, shift, ctrl_alt_win)`），`OnKeyDown` 里也记一次做双保险。
- **"API 返回 S_OK"不等于"事情做成了"**：`ITfRangeACP::GetExtent` 在 WPS 正文控件里返回
  `S_OK` 但给 `(0,0)`，`ITfRange::ShiftStart` 返回 `S_OK` 但 `shifted == 0`。两处都信返回值，
  结果就是"日志写 ok、界面一个字没少"。删字现在三级级联、每级都验证；最后一级是注入原生
  `VK_BACK` 让宿主自己删，注入前记抑制次数，否则自己的注入键会再被认领一次。
- **给组合串加下划线没有 `ITfRange::SetDisplayAttribute` 这种接口**（msctf.idl 里都没有，网上抄的
  代码片段容易把人带沟里）。要走：`RegisterGUID` 拿 atom → `ITfDisplayAttributeProvider` 提供
  `TF_DISPLAYATTRIBUTE` → `ITfProperty(GUID_PROP_ATTRIBUTE).SetValue(VT_I4(atom))`。provider 可以挂在
  **同一个 CLSID** 上（TSF 会自己 QI），注册只是多加一个 `GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER` 分类。
  属性没有 `Clear`，清样式要写 `VT_EMPTY`，而且**上屏前必须清**，否则打出去的字一直带虚线。
  Electron 系（VS Code）不支持 `GUID_PROP_ATTRIBUTE`，那边拼音没有下划线属正常。
- **`AddLanguageProfile` 的字符串参数要自己补 NUL**：Rust 的 `encode_utf16().collect::<Vec<u16>>()`
  没有结尾 0，TSF 按 C 字符串读会越界 —— 表现是语言列表里我们的**图标路径变成一串乱码**
  （`…inklink_tsf_1.0.15.dll῅′ℹ…`）、名字也显示不出来。要 `desc.push(0)` / `icon.push(0)`，
  并且给一个真实存在的 `.ico`（`C:\Program Files\inklink\inklink.ico`，没有就退回 DLL 自身）。
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
- **候选窗位置要在 DLL 里算，不能只传一个坐标**：只给"光标底边"的话，光标贴着任务栏时候选窗
  只能往下画，一半跑到屏幕外（用户拿搜狗对比出来的）。现在 `text_ext_pos`/`caret_screen_rect`
  都返回 `(x, 顶边, 底边)`，`candwin::place_in` 按显示器**可用区**（`GetMonitorInfoW` 的 `rcWork`，
  已扣任务栏）决定放下方还是上方、左右怎么贴边；翻上去的基准是顶边，用底边会盖住光标那一行。
- **rime 的 custom.yaml 文件名必须逐字等于真正 deploy 的 schema id**：我们跑的是 `luna_pinyin_simp`，
  早先写的 `luna_pinyin.custom.yaml` 从来没被读过（这是模糊音"改了没效果"的真凶之一）。
- **改 `speller/algebra` 要整段替换**：`speller/algebra/__append` 是无效写法，正确是
  `patch: speller/algebra: __patch: [...]`，且模糊音片段必须排在 `pinyin:/abbreviation` 之前。
- **改了 rime 配置必须重启 `rime-server`**：librime 只在 `RimeInitialize` 时发现配置变新并 deploy，
  文件写进去不会立刻生效（`tools\apply-fuzzy.ps1` 会重启 + 等 deploy + 用管道验证）。
- **`windows.h` 的 `max`/`min` 宏会把 `std::max(` 展开成语法错误**（`error C2589: '(': illegal token`）。
  `std::min<size_t>(...)` 带模板参数所以没事，`std::max(a, b)` 会被吃掉 → 用三元表达式，或加括号 `(std::max)(a, b)`。
- **诊断工具的输出用纯 ASCII**：控制台代码页和管道编码不可靠，`--selftest` 打中文在默认 GBK 控制台里
  会变成乱码（虽然断言本身是通过的），排障时反而让人怀疑是 bug。
- **改管道协议必须向后兼容，而且兼容分支要"立刻"生效**（2026-10-27 微信被系统杀掉的根因）：
  v3 选词报文多了 code/word，引擎在 `read_exact(&clen, 4)` 上死等，而还在跑旧 DLL 的进程只发
  `[type][index]` 五字节就等响应 —— **两边互等，应用的 UI 线程永久挂起**，Windows 判定"未响应"
  直接关掉它（`Application Hang`，Event 1002）。引擎的旧报文兼容分支原来放在 `read_exact` 失败之后，
  而 `ReadFile` 在没有数据时不会失败、只会阻塞，所以那个分支在正常使用中永远触发不到，
  只有客户端主动断开才轮到它。修法：**先用 `PeekNamedPipe` 探测有没有后续字节**（引擎侧 `wait_readable()`，
  等 100ms 没有就走老协议），DLL 侧所有管道读也改成 `read_exact_timeout()`（`QUERY_TIMEOUT_MS = 150`、
  `SELECT_TIMEOUT_MS = 100`），任何情况下都不许无限阻塞 UI 线程。
  回归探针：`tools\probe-legacy-select.ps1`（写 5 字节老报文，期望 100ms 内收到响应）。

## 十、P4b（AI 的界面）—— 已完成（1.1.37）

> 上一版这里写的是"接手要点"。P4b 已按那份要点做完并真机验证，这一节改成**现状 + 坑**。
> 引擎侧（P4a）**一行没动**：协议 6/7、WinHTTP、后台线程、`WM_APP + 30` 通知都是 1.1.34 的。

### 链路

```
☰ 菜单「翻译/改写/续写」
  → ai::from_menu(action)                     tsf/src/ai.rs
  → TextService 排 EditAction::ReadSelection  （单槽位，忙就跳过）
  → DoEditSession: GetSelection → GetText → ai::start
  → ipc 协议 6 → 引擎后台线程打 HTTP → PostMessage(bar hwnd, WM_APP+30, seq)
  → bar 的 wndproc 调 ai::on_done(seq)
  → ipc 协议 7 取回文本（内存读）
  → TextService 排 EditAction::AiApply → SetText 写回
```

### 代码落点

| 文件 | 作用 |
|:---|:---|
| `tsf/src/ai.rs`（新） | 动作编号、命令映射、任务记账（seq → 原文）、线程局部服务指针、`WM_AI_DONE`、`on_done` |
| `tsf/src/ipc.rs` | `ai_request()`（协议 6）/ `ai_result()`（协议 7）+ 报文与解析的纯函数和单测 |
| `tsf/src/lib.rs` | `EditAction::ReadSelection` / `AiApply`、`request_edit_if_idle()`、`current_context()`、`selection_text()`、`apply_ai_result()`、两个 thunk |
| `tsf/src/menu.rs` | 三个 AI 菜单项（命令 10/11/12）；AI 没启用时灰掉 + 一行说明 |
| `tsf/src/bar.rs` | `handle_menu` 派发 AI、`WM_AI_DONE` 分支、`bar::hwnd()` |
| `settings/src/main.rs` | 「启用 AI」+ 接口地址 + 模型名，读写 `AI_Enable`/`AI_Endpoint`/`AI_Model` |

### 设计上要记住的四条

1. **读和写各排一次编辑会话**，都用 `request_edit_if_idle()`：槽位占着就放弃，绝不挤掉
   用户正在打的那次组合串更新。AI 结果晚几秒才到，这期间用户很可能在打字。
2. **写回前核对选区**：重新读一遍选区，和发起时的原文比对，不一样就丢掉（只写日志）。
   同时 `is_composing()` 为真也丢掉 —— 不能把译文插进拼音中间。
3. **只有续写不替换**：`ai::replaces_selection()`；续写把选区塌到末尾再插入。
4. **服务指针按线程存**（`thread_local!`）：TSF 按线程激活，浮条也是按线程一条；
   存进程级会在浏览器那种多线程宿主里把结果交给另一条线程的服务实例。

### 怎么验（不用真模型）

```powershell
# 1. 起一个假 Ollama（返回「你好」，并把提示词记到 %TEMP%\inklink-mock-ollama.log）
python tools\mock-ollama.py 11435

# 2. 指向它（也可以直接在设置窗里填）
Set-ItemProperty 'HKCU:\Software\inklink\Config' AI_Enable 1 -Type DWord
Set-ItemProperty 'HKCU:\Software\inklink\Config' AI_Endpoint 'http://127.0.0.1:11435'
Set-ItemProperty 'HKCU:\Software\inklink\Config' AI_Model 'mock'

# 3. 走管道验证引擎（不涉及 UI）
powershell -ExecutionPolicy Bypass -File tools\probe-ai.ps1 -Action 0 -Text hello

# 4. 真 UI：记事本打 hello → 回车 → Ctrl+A → 浮条 ☰ → 翻译选中文字
#    成功时 DLL 日志（C:\Users\Public\inklink-tsf-<pid>.log）是：
#      ai: start seq=1 action=翻译 chars=5
#      ai: done  seq=1 action=翻译 chars=2
#      ai: apply action=翻译 -> ok
```

1.1.37 的实测结果：记事本里 `hello` 被替换成「你好」，假 Ollama 收到的提示词正好是
`把下面这段内容翻译成中文，只输出译文：` + 换行 + `hello`。截图在
`C:\dsh\work\shots\`（`1-selected.png` / `2-menu.png` / `3-result-translate.png`）。

### 这一轮踩到的坑（测试环境，不是产品 bug）

- **Win11 记事本会恢复上次没保存的标签**：第一次自动化跑出来的选区是 `deactivate\rhello`
  （上一轮留下的 `inklink-tsf-*.log` 的内容），看着"翻译成功"其实替换错了对象。
  自动化必须先 `Ctrl+A` + `Delete` 清空文档。
- **DSH 里的 PowerShell 是 DPI unaware 的**：`GetWindowRect` 给 96 DPI 虚拟坐标，
  `Screen.Bounds` 是 1536×864，截屏只抓到左上角一块。脚本里先 `SetProcessDPIAware()`。
- **前台锁**：用户在 WPS 里干活时 `SetForegroundWindow` 会静默失败，`Win+Space` 全打在
  别人的窗口里。自动化要先核对 `GetForegroundWindow()`，不对就别往下按键。
- 顺带记一笔：**别在用户正在用电脑的时候跑 UI 自动化**。这次就因为前台是 WPS 表格，
  半途停了下来，改用管道 + 一次干净的前台窗口把验收跑完。

### 1.1.38：状态字 + 退格撤回

- `bar::set_status(text, ttl)`：把一行字盖在浮条上（`WM_STATUS` 投递重画，`WM_TIMER` 到点擦掉）。
  AI 请求发出时显示「翻译中…」，回来显示「完成 · 退格可撤回」/「AI 失败（详见日志）」。
- `AiUndo`：写回成功后记下（原文、插入长度、写入后的 ACP 锚点、`UndoMs` 期限）。窗口内按一次
  退格，`EditAction::AiRevert` 核对光标还在原位后，删掉插入段并把原文写回；续写只删插入段。
- 不靠应用 Ctrl+Z：TSF 的 `SetText` 进不进宿主撤销栈各家不同。
- 单测 139 项（新增撤回区间的边界）。

### 1.1.39：单字兜底 + 两字整句学习

- 引擎：`prune_single_chars` 把单字挪到候选末尾（最多 5 个），不再直接删。验收：
  `wangjia` 末尾是 `王/望/往/忘/网`，选「王」后剩 `jia` 继续选「佳」。
- DLL：`SENTENCE_MIN_CHARS` 4 → 2；新增 `merge_sentence_code`（`keqiu` 是 `pikeqiu` 的后缀
  就不拼，`jia` 不是 `wang` 的后缀就拼成 `wangjia`）。
- 单测 141、引擎 `--selftest` 43 项。

### 打包踩到的两个真 bug（1.1.37 的包受影响）

1. `release.ps1` 把 `$ENGime\data` 拷到 `$filesime` 时，目标目录不存在会把 **data 的内容
   拍平**到 `rime\` 下；安装脚本按 `rime\data` 找，静默跳过 → 装完**没有词库，一个候选都没有**。
2. `install.ps1` 从来没拷 `inklink-settings.exe`（浮条菜单的「设置…」点了没反应）。
   两处都已修，并补了自检项（rime 数据 / 设置窗）。

### 1.1.40：浮条 AI 三格 + 结果确认框

- 浮条 7 格：品牌 / 中英 / 标点 / 翻译 / 改写 / 续写 / ☰；亮暗切换进菜单（「暗色主题」）。
- 新增 `tsf/src/preview.rs`：AI 结果先弹确认框（NOACTIVATE，不抢焦点），点「确定」才
  写到文档；Esc / 点外面取消。`ai.rs` 用 `PENDING` 存待确认结果。
- 引擎提示词：改写 = 错别字/用词/逻辑；续写 = 前文不动、只输出新增。
- 单测 145、引擎 selftest 45。

### 1.1.41：英文原样候选（修 bug）

librime 对纯字母会返回满 50 个中文，而"追加原始英文"那段被 `out.size() < MAX_CANDS`
挡住，导致 `github`/`hello` 连原样候选都没有。改成满了先 `pop_back()` 腾位。
实测 `github`/`gith`/`hello` 第一候选就是原文。**英文联想仍没有**（见下）。

### 1.1.42：AI 面板

- 浮条 5 格：品牌 / 中英 / 标点 / **AI** / ☰。AI 点开是 `tsf/src/ai_panel.rs` 浮动面板。
- 面板：读选区 → 显示原文 → 翻译/改写/续写 → 结果 → 确定/取消/重新生成。
  未启用、没选中、生成中、失败原因都在面板里显示（用户反馈"没提示"）。
- 不抢焦点（NOACTIVATE），确认后走原来的 `EditAction::AiApply`。
- ☰ 菜单去掉三项 AI；单测 141。

### 1.1.43：AI 修好、面板常驻、浮条彩光、退格一次删、单字补全（2026-11-06）

用户一次报了五个问题，都修了。单测 152、引擎 selftest 52。

- **AI 用不了**（引擎）：配置的 `qwen3.5:9b` 是思考型模型，不关思考会先输出几百个 token 的推理，
  30 秒超时经常不够。请求体加 `"think":false`（实测翻一句 3.5s → 0.3s）；端点对它报 400 就去掉重试；
  `strip_think()` 兜底去掉漏进结果的 `<think>…</think>`；接收超时 30s → 120s。失败原因翻成人话显示在面板
  （`ai_error_text()`：Ollama 没开 / 超时 / 模型没装并给出 `ollama pull` 命令）。
- **选中的字进不了面板**（DLL）：两个原因叠在一起。① 只在点「AI」那一下读一次选区，点文档面板就关了；
  ② 读选区用的是"最后一次按键的上下文"——Kiro / VS Code 一个窗口里好几个编辑区，读到的是聊天框。
  现在面板开着时每 400ms 读一次**焦点文档**的选区（`focus_context()`），编辑会话请求时记下上下文
  （`pending_ctx`），`DoEditSession` 用同一个。选区没变不重画、只在长度变化时写日志（不记内容）。
- **面板常驻屏幕中下方**：放在当前显示器工作区底部正中（`bottom_center()`），只有 × 能关；
  宿主切到后台/输入法被切走时先收起，回来再出现。「取消」只丢掉结果；「确定」后面板不关，写没写进去
  显示在面板里（`ai::on_applied()`），写不进去时可以「复制结果」。
- **浮条彩光**：面板开着时浮条外圈一圈以紫为主的五彩光在转（`bar::set_glow`，40ms 一帧，只在可见时画）。
  为此浮条窗口四周常留 `GLOW_PAD = 7` 的透明边，命中测试/拖动/菜单位置都换算回本体坐标
  （`to_bar_coords()`）；记住的位置仍是本体左上角，老版本存的位置照常可用。预览图
  `tsf/target/bar-preview-glow.bmp`。
- **上屏后要按两次退格**：以前撤回窗口（上屏后 2 秒）里的退格是**认领下来自己用 `SetText` 删**；
  Chromium 系宿主（VS Code / Kiro / Edge / Electron）会把上屏后紧跟的那次外部删除丢掉，日志里两次退格的
  ACP 锚点一模一样。现在退格**不认领**，交给宿主自己删，我们只记账（`note_idle_backspace()`，按
  `(lparam, GetMessageTime())` 去重）。删掉了 `EditAction::DeleteBack`、注入原生退格那一整套
  （`INJECTED_BACK` / `ipc::press_backspace`）。AI 结果的退格撤回仍然认领。
- **单字太少**（引擎）：`prune_single_chars` 只留 5 个单字，非固定搭配一个字一个字找时经常找不到。
  改成 `arrange_single_chars()`：单字全部保留、挪到词后面，再用第一个音节单独查一次补齐（最多 60 个）。
  实测 `xiangmu` 单字 5 → 60、`wangjia` 16，查询 1～6ms。

**部署时的坑**：`deploy-local.ps1` 这次跑完设置窗构建后挂住超时（DLL 注册、图标、模糊音都已完成），
原因没查。引擎是手动换的：停掉 → 备份成 `rime-server.1.1.42.exe.bak` → 拷新的 → 重启。
**测试时的坑**：AI 协议 7 取走结果就删，两个测试脚本用同一个序号会互相抢，看着像"取不到结果"。

**还欠真机手测**：Kiro/记事本里上屏后退格一次删一个字；面板在文档里选字能实时出现；彩光转动；
`wangjia` 翻到「佳」以外的单字。
### 还没做的（留给 P6 / M3）

- 结果写进文档后**不能撤回**（它不属于组合串，`UndoMs` 那套管不到）。
- 没有"翻译整段/逐句对照"这类更强的交互；现在只处理 ≤2000 字的选区。
- 流式输出没接（引擎是一次性 `stream:false`）。

## 十一、下一步：M2 切片 1（笔记术语进候选）

1.1.37 已发布（tag `v1.1.37`，CI 出包）。产品化 P0–P5 到此齐了，剩下的核心空缺只有 M2：
**笔记里的词，打一遍就得出来**。

- 设计定案见 `docs/adr/0009-个人语料走引擎侧boost层.md`：语料单独存
  `%LOCALAPPDATA%\inklink\corpus.tsv`，引擎启动时读入，在 `query()` 里 boost/补词；
  **不改 Rime 方案**（合并词库的排序压不过语言模型，`custom_phrase` 加载语义试不通，
  两条路的实测结果都记在 ADR 里）。
- 落点、排序规则、测试接缝和验收在 `PLAN.md`「M2 切片 1」。
- 第一步只需：笔记目录 → `corpus.tsv` → 引擎读入 → `chitu` 第一条是「赤兔」。

**还欠一次手测**：P4b 的改写/续写只在单测里覆盖，真机只点过翻译（当时前台被 WPS 占着，
自动化没法安全抢焦点）。下次打开记事本顺手点一下两个新菜单项即可。

