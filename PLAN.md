# 实施计划：M0（能打字）

> 依据 `PRODUCT.md` 与 `docs/architecture.md`。一次只做一步，每步都有可观察的验收信号。
> 状态：⬜ 未开始 ｜ 🟡 进行中 ｜ ✅ 已完成 ｜ ⛔ 阻塞

## 步骤总览

| # | 步骤 | 依赖 | 验收信号 | 状态 |
|:---|:---|:---|:---|:---|
| 0 | 仓库骨架与构建链 | — | `cargo tauri dev` 开出空窗口，clippy 零告警 | ✅ |
| 1 | 词库格式与加载器 | 0 | 6 万条加载 < 200ms，前缀查询 < 1ms（单测） | ✅ |
| 2 | 词库转换工具（rime-ice → TSV） | 1 | 转换条数一致、抽样拼音一致、输出可重复 | ✅ |
| 3 | 拼音切分与模糊音 | 1 | 30 组标注样例全绿 | ✅ |
| 4 | 召回与排序 | 2,3 | 15 个真实查询 Top1 合理，查询 < 50ms | ✅ |
| 5 | 面板 UI + 热键 | 0 | 热键 → 面板可见 < 150ms，能输入出候选 | ✅ |
| 6 | 上屏 | 4,5 | 记事本通过，浏览器/VSCode 待人工 | ✅ |
| 7 | M0 验收 | 全部 | 对照 `PRODUCT.md` 成功标准逐条核对 | ⬜ |

---

## 步骤 0：仓库骨架与构建链

**目标**：有一块能跑、能测、能 lint 的地基，后面每一步都在上面长。

**做什么**：
- 初始化 git 仓库，`.gitignore` 覆盖 `data/`、`app/target/`、`app/node_modules/`、`.venv/`、`tmp/`
- 用 `npm create tauri-app`（Svelte + TypeScript 模板）生成 `app/`
- 删掉模板自带的示例页面，换成一个空白面板窗口（无边框、透明、置顶、不占任务栏）
- 在 `app/src-tauri/src` 里建 `engine/`、`platform/` 模块目录

> 说明：窗口默认隐藏要配合全局热键才有意义，那属于步骤 5。步骤 0 先让窗口可见，用 Esc 隐藏，作为构建链的可见证据。

**验收**：
- [x] 应用能启动并显示 `inklink` 窗口（`target\debug\inklink.exe`，title=inklink）
- [x] `cargo test`、`cargo clippy --all-targets -- -D warnings`、`cargo fmt --check` 全绿
- [x] `svelte-check` 0 error 0 warning；`npm run build` 产出 `../build`
- [x] `git status` 干净，`data/`、`target/`、`node_modules/` 未被跟踪
- [x] 附带：`tauri build --debug` 能出 MSI 与 NSIS 安装包

**测试接缝**：无（本步不写业务测试）

**可能踩的坑**：国内拉 crates 慢（配镜像）；WebView2 缺失导致白屏；Tauri 2 与 Node 24 的模板版本要匹配。

**实际操作中踩到的坑（已写进 AGENTS.md）**：
1. 本机 Program Files 下有 GNU 版 rustc 1.98，盖住 rustup 的 MSVC 版 → `dlltool.exe not found`。修法：把 `%USERPROFILE%\.cargo\bin` 放到 PATH 最前。
2. MSVC 工具链缺 `clippy` / `rustfmt` 组件 → `rustup component add clippy rustfmt`。
3. 本 DSH 会话是提权进程，WebView2 沙箱起不来 → `os error 5`。修法：验证时设 `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--no-sandbox`（用户正常双击不受影响）。
4. `main.rs` 里写的是模板的 `app_lib::run()`，改 crate 名后要同步改成 `inklink_lib::run()`。

---

## 步骤 1：词库格式与加载器

**目标**：引擎能在一份 TSV 词库上做毫秒级前缀查询。

**做什么**：
- 定义词库格式：`词 \t 拼音(空格分音节) \t 词频`，UTF-8 无 BOM；同目录 `manifest.json` 记名称、版本、条数、许可、来源
- 加载器：一次性读入，构建「无空格拼音 → 词条 id 列表」前缀表 + 词条数组
- 错误处理：文件缺失 / 格式错行 → 返回可读错误，提示重建词库，**不 panic**

**验收**：
- [x] 单测：加载 6 万条合成词库 < 200ms（release 实测 **153ms**，debug 164ms）
- [x] 单测：前缀查询 10 万次 < 100ms（release 实测 **76ms**，约 0.76µs/次）
- [x] 单测：缺文件返回 `Err`，坏行被跳过并计数（6 个单测全绿）

**测试接缝**：`Lexicon::load(path) -> Result<Lexicon, LexError>`、`Lexicon::lookup(code_prefix) -> impl Iterator<Item=&Entry>`

**可能踩的坑**：拼音含 `ü` 与 `v` 的归一化；空格分隔导致的索引键不一致；大文件读取时的内存峰值。

**实现备注**：
- `manifest.json` 挪到步骤 2 与资源包一起做，本步只做 TSV 与索引。
- 加载 153ms 里大头是 6 万次 `BTreeMap<String, Vec<u32>>` 插入；启动时一次性完成，不进按键路径，暂不优化。若将来词库涨到 30 万条，再换「排序数组 + 二分」。
- ⚠️ 教训：**不要用 PowerShell 的 `Set-Content` 改源码**（PS 5.1 默认 GBK 读写会毁掉 UTF-8 中文），一律用编辑工具。

---

## 步骤 2：luna_pinyin → TSV 转换工具

**目标**：能把外部词库转成我们的格式，且转换可复现。

> **数据源已变更**：原计划用 Rime `luna_pinyin`，实测后不可用（词表是字表 + 繁体语料，见 `docs/adr/0004`）。
> 现改用 **rime-ice**（GPL-3.0），项目协议随之定为 GPL-3.0。

**做什么**：
- `tools/build_base_lexicon.py`：解析 `8105.dict.yaml`（通用规范汉字表）与 `base.dict.yaml`（大词库），
  按权重阈值裁剪、按 `(词, 拼音)` 去重、全序排序后输出 `data/base.tsv` + `data/manifest.json`
- 阈值可调（默认 10000）；不做语义清洗（那是 M2 的事）

**验收**：
- [x] 输出条数 == 通过阈值的条目数（3,605 + 76,829 = **80,434**）
- [x] 抽样词与拼音与源文件一致（抽查「的 / 项目 / 中国 / 人民」均正确）
- [x] 同输入连跑两次，`base.tsv` 与 `manifest.json` 逐字节一致（SHA-256 相同）
- [x] Rust 加载器能直接吃这份文件：`cargo run --example lexicon_check` → 80,434 条，0 坏行

**测试接缝**：`parse_dict(path) -> Vec<(词, 拼音, 权重)>`（纯函数，用小样例字符串测）

**可能踩的坑**：yaml 表体起始标记 `...`；权重列非数字；`data/` 被 gitignore 后忘记生成。

**实现备注**：
- 实测加载 80,434 条：debug 283ms / release 约 150ms（一次性启动成本，不在按键路径）。
- 查询耗时 4–75µs（`de` 命中 263 条最慢），远低于 50ms 预算。
- 数据里存在"置顶梗词"（如 `江泽民` 权重 19,260,817），属上游有意为之；排序层用 `log(1+w)` 即可抑制，
  另由 M1 的使用反馈覆盖个人偏好。

---

## 步骤 3：拼音切分与模糊音

**目标**：把用户敲的字母串正确切成音节，并支持容错。

**做什么**：
- 最大匹配切分，声母表 + 韵母表内置
- 模糊音规则（可配置）：zh/z、ch/c、sh/s、n/l、in/ing、an/ang、en/eng
- 简拼识别：连续首字母组合
- `ü` 归一化：`v` / `lv` / `nv` → `lü` / `nü`

**验收**：
- [x] 标注样例全绿（6 组单测 / 12 个断言，覆盖 `xi'an`、`zhongguo`、`lvxing`、`LVXING`、`xmch`）
- [x] 无法切分时返回 `None`，不 panic

**测试接缝**：`decode(input: &str) -> Option<Vec<String>>`、`fuzzy_variants(&str) -> Vec<String>`、`is_abbreviation(&str) -> bool`

**可能踩的坑**：零声母（a/o/e 开头）；`er` 与儿化；歧义切分（`xian` = xi'an 还是 xian）。

**实现备注（红线测试抓到的两个真 bug）**：
1. `fuzzy_variants` 无限增长：`sh` → `shh` → `shhh`…… 原因是「sh→s」的反向规则对 `s` 前缀无条件生效；
   改成 `if 以双字母开头 { 换成单字母 } else if 以单字母开头 { 换成双字母 }`。
2. `xi'an` 被切成 `xian`：撇号原来被当垃圾字符删掉，改成**保留为音节边界**。
3. 音节表不是手写的：从 rime-ice 词表的拼音列反推出 402 个合法音节（避免漏/错）。
4. 单测跑挂过一次，原因是上面的无限循环——`cargo test` 因此挂死 60 秒以上；
   以后跑 `cargo test` 一律加超时保护。

---

## 步骤 4：召回与排序

**目标**：给定拼音串，返回一个"像输入法"的候选列表。

**做什么**：
- 召回：全拼前缀 + 简拼 + 模糊音变体，合并去重
- 打分（线性、可解释）：`log(1+词频)` + 个人权重 + 完整度惩罚（简拼/模糊音命中降权）+ 词长惩罚
- 排序稳定：同分按词频、再按词典序，保证确定性

**验收**：
- [x] 单测 6 个全绿；15 个真实查询人工核对，Top-1 全部合理（15/15）
- [x] 同一输入多次调用结果完全一致（单测覆盖）
- [x] 单次查询 < 50ms（实测 **平均 62µs，最慢 233µs**，8 万词库；建索引 90ms 只在启动做一次）

**测试接缝**：`Ranker::candidates(input: &str, limit: usize) -> Vec<Candidate>`

**可能踩的坑**：过度调参导致"看起来聪明但不稳定"；同音词过多时的分页。

**决策（用户 2026-10-21 拍板）**：M0 **不做跨词组合**，只做整词（全拼/前缀/简拼/模糊音）。
跨词组合排到 **4b**，且必须与语言模型（n-gram）一起做——原型证明没有 LM 的组合搜索会输出
「有很好在」这类高频单字串（`yhhz` 得分 27.7 > 银行行长 23.9）。
个人短语别名（C 方案）不做，用户计划在 AI 层用权重解决。

**实测结果**（release，`examples/preview_rank.rs`）：

| 输入 | Top-1 | 说明 |
|:---|:---|:---|
| nihao / xiangmu / zhongguo / women / yinhang / beijing / chifan | 你好 / 项目 / 中国 / 我们 / 银行 / 北京 / 吃饭 | 整词命中 |
| shijian / shanghai | 时间 / 上海 | 同分区间，靠词频决出；🟡 脆弱 |
| zhonggu | 中国 → 中国人 → 中国共产党 | 前缀补全 |
| zgrm / bjdx / xwlb | 中国人民 / 北京大学 / 新闻联播 | 简拼命中 |
| yhhz | 友好合作 | 简拼整词命中（词库原有），**不是**银行行长——设计边界 |
| yinhanghangzhang | 无候选 | 跨词组合未做（4b） |

**边界与已知噪声**：
- 模糊音会把 `nihao` 带出「利好/理好」，`chifan` 带出「此番/此方法」——已降权，排在后位；
  若觉得吵，可在 M1 按用户反馈开关。
- `shijian` 的三个候选分差 <0.02，稍有扰动就换位；M1 的上下文特征要解决这个。
- 前缀候选（项目管理/项目经理）排在前列属于期望行为，真人输入法也这样。

**步骤 4 开工前的实测基线**（`examples/preview_rank.rs`，2026-10-21）：

| 输入 | 当前 Top-1 | 前二差距 | 判断 |
|:---|:---|:---|:---|
| nihao / xiangmu / zhongguo / women / gongzuo / zhege / yinhang / chifan / renmin | 你好 / 项目 / 中国 / 我们 / 工作 / 这个 / 银行 / 吃饭 / 人民 | 巨大 | ✅ |
| shijian | 时间(504677) vs 事件(500566) | **0.8%** | 🟡 险胜 |
| shanghai | 上海(502185) vs 伤害(500317) | **0.4%** | 🟡 险胜 |
| shi / yi / ji | 是 / 一 / 级（46–58 个同音候选） | 大 | ✅ |
| zhonggu（前缀） | 中国 → 中国人 → 中国共产党 | — | ✅ 前缀可用 |
| **xiangmudaohaochitu** | **无候选** | — | ❌ 个人词，M2 才补 |
| **yinhanghangzhang** | **无候选** | — | ❌ **缺多词组合**，见下 |
| **zgrm（简拼）** | 未实现 | — | ❌ 本步要做 |

两条结论：
1. **纯权重排序已经"像那么回事"**，但很多同频词差距 <1%，说明上游权重被归一化过（大量 500000 附近的值）。
   排序不能只靠权重，必须引入完整度/词长等特征把差距拉开。
2. **缺"多词组合"**：真人打 `yinhanghangzhang` 期望「银行行长」，但我们只做整词匹配。
   这是真输入法与"词表查询器"的分水岭，实现要 beam search 或分级切分，工作量不小。

> ⚠️ **待用户决定**：多词组合是否进 M0。进，则步骤 4 拆成 4a（整词/前缀/简拼/模糊音）与 4b（多词组合）。

---

## 步骤 5：面板 UI + 热键

**目标**：按下热键，面板出现在光标附近，能输入、能选词。

**做什么**：
- 全局热键注册（`tauri-plugin-global-shortcut`），默认 `Ctrl+\``
- 窗口：无边框、置顶、`skip_taskbar`、显示时不抢焦点（抢了就记录并在关闭时归还）
- 候选列表：键盘导航（↑↓ / 数字键 / Tab）、Esc 关闭、点击选择

**验收**：
- [x] 热键 `Ctrl+\`` / `Alt+\`` / `Ctrl+Shift+\`` 三个组合都注册成功，按下后**面板出现在屏幕中央**
- [x] 启动即在屏幕外，不打扰；不占任务栏、无边框、置顶
- [x] 输入 `xiangmu` 出候选，按词频排序（想 / 向 / 先 / 相 / 像 / 现 / 显 / 象 / 线 等，实测截图）
- [x] **用户实机确认可用**：唤出 → 打字 → 退格 → Esc → 再唤出，全程不崩
- [x] 系统输入法不再抢按键（改用不可编辑元素接收按键）
- [x] 全量门禁：18 个单测、clippy `-D warnings`、`cargo fmt --check`、`svelte-check` 0 error 0 warning

**测试接缝**：UI 不做自动化测试，按上面清单手测

**可能踩的坑**：Tauri 窗口 show 时抢焦点；多显示器 DPI；热键与其他软件冲突。

**这一步踩到的六个真坑（都已解决，写下来免得重踩）**：

1. **工作区目录会让窗口创建失败**：exe 放在 `C:\dsh\work\...` 下运行时，
   `CreateWindowEx` 返回 `拒绝访问 (os error 5)`；**把同一个 exe 拷到桌面就能跑**。
   判断是工作区的 ACL 影响。开发期一律把 exe 拷到别处验证，发布时装到 `Program Files`。
2. **`hide()` 之后 `show()` 不回来**：Tauri 报 `Ok`、`is_visible()` 也是 `true`，
   但窗口再也不出现。改成**把窗口挪到 `(-30000,-30000)`**，唤出时挪回屏幕中央。
3. **系统输入法会抢走按键**：面板里敲 `xiangmu`，搜狗先弹它自己的候选框。
4. **用 `ImmAssociateContext` 摘 IME 会导致闪退**：摘除后打字正常，但**一按退格就 `0xc0000409`**
   （fast-fail，无日志）。最终方案是**不用 `<input>`**：换成 `<div tabindex=0>` + 自己处理 `keydown`，
   没有可编辑元素，输入法天然不介入。`platform::disable_ime` 保留但不再调用。
5. **前端直接 `win.hide()` 会让 Rust 的可见标记失同步**：下次热键被当成"该隐藏"，
   表现为热键失灵。改成统一走 `hide_panel` 命令。
6. **前端抢先查询**：词库在后台线程加载（300–600ms），前端一挂载就查，拿到空结果。
   加 `lexicon-ready` 事件，就绪后重查。

> 另注：前端实测一次查询往返约 **9ms**（IPC + JS），引擎本身 62µs。9ms 仍在 50ms 预算内，
> 但如果以后觉得"按键到候选"不够跟手，优先查这条链路，而不是引擎。

**更正**：日志里的旧结论（下述）已被上面第 1 条解释——不是 WebView2 坏了：

启动即在 `tauri::app.rs:1444` panic，错误在两次运行间摇摆：

```
Failed to setup app: WebView2 error: HRESULT(0x8000FFFF) 灾难性故障
Failed to setup app: 拒绝访问。(os error 5)
```

已排除的假设（逐一实测）：
1. `visible: false` 创建窗口 → 失败（改回可见仍失败）
2. `transparent: true` → 关闭透明仍失败
3. 全局热键插件 → 整块摘掉仍失败
4. WebView2 用户数据目录损坏 → 清空重建仍失败
5. 锁屏/桌面不可访问 → `OpenInputDesktop` 正常，无 LogonUI
6. 词库加载阻塞主线程 → 挪到后台线程仍失败

结论：**步骤 0 时同一个 exe 曾成功起窗口**（title=inklink），之后环境状态变化导致 WebView2 无法初始化。

> **更正（2026-10-21 晚）**：上面的判断是错的。真正的分界线是 **exe 所在目录**：
> 同一份二进制放在 `C:\dsh\work\...` 下必然失败，拷到桌面就正常。与提权、WebView2 版本无关。
> 已在 HANDOFF「已知问题」里改成这条。

---

## 步骤 6：上屏

**目标**：选中的词真正进入目标应用。

**做什么**：
- 通道一：`SendInput` + `KEYEVENTF_UNICODE` 直接发字符
- 通道二（回退）：写入剪贴板 → 发送 `Ctrl+V`
- 唤起面板前记住前台窗口句柄；上屏前先藏面板、再把焦点还回去

**验收**：
- [x] **记事本实测通过**：唤出面板 → 输入 `xiangmu` → 回车 → 「项目」写进记事本（截图验证）
- [x] 上屏后焦点回到原应用（`GetForegroundWindow` 校验为记事本）
- [x] 通道一失败会自动走通道二（`SendInput` 返回不足时）
- [ ] 待人工：WPS / 浏览器 / VSCode 三处
- [ ] 剪贴板兜底**不恢复原内容**（M0 简化，见下）

**测试接缝**：`commit_text(text) -> Result<(), String>`（用真实窗口手测，不写单测）

**可能踩的坑**：UWP / 管理员权限窗口拒绝输入；某些应用需要 `WM_CHAR`；剪贴板被占用。

**实现备注**：
- 顺序必须是「藏面板 → `focus_window` → 睡 40ms → `SendInput`」，否则字符会打进自己的窗口。
- `SetForegroundWindow` 在跨进程时会被系统拒绝，用 `AttachThreadInput` 临时共享输入队列绕开。
- 剪贴板改用 `tauri-plugin-clipboard-manager`：手写 `GlobalAlloc` + `SetClipboardData` 的 FFI 反复类型不匹配，
  不值得继续纠缠；顺带记下——**能抄不写**在这里是省时间的。
- 已知简化：走剪贴板兜底时会覆盖用户原有剪贴板内容，M1 再补「保存/恢复」。

---

## 步骤 7：M0 验收

**目标**：证明"能打字"这件事成立。

**做什么**：
- 对照 `PRODUCT.md` 成功标准逐条核对（性能、可用、不崩）
- 记录实测数据到 `HANDOFF.md`
- 由**用户**拍板是否进入 M1

**验收**：
- [ ] 面板唤起延迟实测值记录在案
- [ ] 候选刷新延迟实测值记录在案
- [ ] 连续使用 30 分钟无崩溃、无卡顿
- [ ] 用户确认「这个能当日常输入用」

---

## 待确认的接缝

按纪律，未经确认的接缝不写测试。以上四处在开工前需用户点头：
1. `Lexicon::load` / `lookup`
2. `parse_luna`
3. `decode`
4. `rank`

其余（UI、上屏）以手测清单为准。

---

## 步骤 6b：不抢焦点 + 键盘钩子（ADR-0005）

**为什么加这一步**：步骤 6 上屏能用，但实机暴露三个连带问题——面板抢焦点导致目标应用光标消失、
数字键打不出来、打一个词要重按一次热键。三者同源：外挂程序靠抢焦点才能拿按键。

**做了什么**：
- 面板窗口加 `WS_EX_NOACTIVATE`，唤出时不夺焦点
- `WH_KEYBOARD_LL` 低级键盘钩子采集按键，命中白名单键才吞掉
- 钩子回调只做「原子读 → 判断 → 入队」，不做任何其它工作（ADR-0005 硬约束）
- 忽略注入事件（否则自己上屏的字符会被吞），并用 `COMMITTING` 标志二次保险
- 带 Ctrl/Alt/Win 的组合键一律放行，避免吃掉热键

**验收**：
- [x] 唤出面板后前台仍是目标应用（`GetForegroundWindow` 校验）
- [x] 面板收到按键（候选正常出现），目标应用**不再收到**这些字母
- [x] 连续输入两个词并分别上屏，记事本内容正确（UI Automation 校验）
- [x] 钩子安装失败会打印错误并可降级（未实现自动降级，M1 补）
- [ ] 待人工：真实键盘体验（模拟按键会被判定为注入事件，测不了这条）

**测试专用开关**：`INKLINK_TEST_INJECTED=1` 让钩子接受注入事件，仅用于自动化验证 UI 链路。
正常使用必须不设这个变量。

---

## 阶段 T5 待办（librime 接入之后）

> 当前状态：TSF 输入法 + librime 引擎已在用户本机日常使用。
> 自研 Ranker 已退役（代码保留在 `app/src-tauri/src/engine/`，未删）。

### T5a 用户词库学习 —— ✅ 2026-10-27 已实现（M1）

**背景**：librime 自带用户词库（`rime\user\luna_pinyin.userdb`），但实测这个版本学不到东西：

| 尝试 | 结果 |
|:---|:---|
| `RimeSelectCandidate(session, index)` | 返回成功，`RimeGetCommit` 拿不到 commit |
| `RimeSelectCandidateOnCurrentPage(session, slot)` | 同上 |
| 模拟数字键 `RimeSimulateKeySequence("5")` | 同上 |
| 恢复 `RimeGetContext` 刷新菜单后再选 | 同上 |

**结论**：不依赖 librime 的学习，自建记忆层（`code -> 词 -> 次数`）。**已按下面三步做完。**

**教训（仍然适用）**：曾一次性把「记录 + 落盘 + 排序提升」全加上，结果**查询路径崩溃**
（原因未定位，崩溃在 `query()` 内部，连 `q=...` 日志都没写出来）。这次严格分三步、每步都验证。

#### 实现（2026-10-27，`rime-server/rime-server.cc`）

| # | 步骤 | 验证方式 |
|:---|:---|:---|
| 1 | 记录到内存 `g_user` + 日志 `learn key=... word=... count=...` | 连续查询不崩 |
| 2 | 落盘 `%LOCALAPPDATA%\inklink\user-words.tsv`（写 `.tmp` → `MoveFileExA` 原子替换） | 杀进程重启后仍生效 |
| 3 | `query()` 返回前 `apply_user_boost()` 按次数 `std::stable_sort` | 选一次「橡木」→ 下次 `xiangmu` 首选变「橡木」 |

**协议 v3**（比原计划的 v2 多两个字段，理由写在下面）：

```
type=2 选词： [u8 2][u32 index][u32 code_len][code utf8][u16 word_len][word utf8]
```

原计划只发 `index`，靠引擎记住"上次查询的码"——多应用并发（微信 + VSCode 同时打字）时会串，
而且下标脱离"当时查的是什么"没有意义。现在 DLL 把 `code` 和选中的 `word` 一起发过去。

**兼容性的教训（2026-10-27，微信被系统杀掉那次）**：原来的兼容分支写成"`read_exact` 失败就打
`select %u (no code: old protocol)`，照常回响应"。但 `ReadFile` 在对方没数据时**不失败、只阻塞**，
所以还在跑旧 DLL 的进程（只发 5 字节就去等响应）会让两边互等 —— 应用 UI 线程永久挂起，
Windows 判定"未响应"直接关闭（`Application Hang`，Event 1002）。71 秒后才轮到兼容分支，
因为那时客户端被杀了、管道断开。
现在引擎读可选字段前先 `wait_readable()`（`PeekNamedPipe` 轮询 100ms），没有后续字节立刻走兼容路径；
DLL 侧所有管道读也改成带超时的 `read_exact_timeout()`（`QUERY_TIMEOUT_MS = 150`、`SELECT_TIMEOUT_MS = 100`）。
回归探针：`tools\probe-legacy-select.ps1`。

**学到哪个码**：由**选中字数**决定，不是整串。`learn_key(raw_code, word)` 用贪心音节切分
（与 `tsf/src/pinyin.rs` 同规则）取前 N 个音节：

| 输入 | 选中 | 记到哪个码 |
|:---|:---|:---|
| `pikeqiu` | 皮 | `pi` |
| `pikeqiu` | 皮克 | `pike` |
| `xiangmu` | 项目 | `xiangmu` |
| `github`（切不动） | — | 整串 |

这条让「分段上屏」和「学习」对得上：用户选完「皮」继续选「克」，第二次打 `pike` 就能先看到「皮克」。

**其它规则**：
- 打英文不算：`word == strip_separators(code)` 时跳过（否则选英文候选会污染词库）。
- 计数上限 999；串数上限 `USER_MAX_CODES = 4000`，超了先丢"最高次数≤1"的（用过一次就再没选过的）。
- `std::mutex g_user_mutex`：每连接一线程，学习与查询并发。
- 存储位置**不能**在 `C:\inklink-engine\`（普通用户无写权限），也不混进 rime 的 `user_dir`。
- `rime-server.exe --selftest`：32 项纯 ASCII 断言（`utf8_char_count` / `strip_separators` /
  `normalize_boundaries` / `split_lengths` / `learn_key` / `should_learn` / `boost_score`），
  不初始化 librime、不碰管道。

**还没做的**：跨设备权重没有区分。"选错了想撤销"、"整句上屏也学习"、"跨应用权重区分"都已补上，见下面三节。

#### 跨应用权重区分（2026-10-28 补，M1 续）

**要解决的问题**：所有应用共用一套词库，微信里养出来的习惯会污染写代码的候选。
同一个拼音在不同地方想打的词不一样 —— 聊天里 `xiangmu` 常选「想你」，工位上常选「项目」。

| 层 | 做法 |
|:---|:---|
| 应用标识 | DLL 用 `GetModuleFileNameW(null)` 取宿主 exe 名，去 `.exe`、转小写（`weixin`/`code`/`notepad`），`OnceLock` 缓存；取不到就是空串（未知应用） |
| 协议 v5 | type 1/2/3 三种报文末尾**追加可选** `[u16 app_len][app]`。引擎先 `wait_readable(p, 100)` 探测，没有就当空串 —— 老 DLL 一个字节都不用改，也不会挂（复用 1.0.1 那次死锁的教训） |
| 存储 | `UserWord { total, apps }`，`apps` 是 `应用名 -> 次数`。文件每条 `码\t词\t次数\t应用名`，**每个 (码,词,应用) 一行**；`total` 不单独存，读盘时求和（少一个字段少一处不一致）。老的三列文件当成"未知应用"读进来 |
| 排序分 | `boost_score(w, app) = w.total + w.apps[app] * 2`：当前应用里的次数算**双倍** |
| 撤回 | `unlearn` 先扣当前应用；当前应用没记过就扣次数最多的那个（在 A 学、在 B 删也能减回去） |

**为什么是"双倍"而不是"只看本应用"**：只看本应用等于新应用里词库全关（一次都没选过）；
只看全局又退回所有应用混在一起。双倍是个能解释、能单测的经验值 —— 同等次数下本应用的词一定赢，
但别的应用里反复用过的词仍能靠全局次数压过刚用一次的。

`--review-dict` 现在会打印分应用明细（`xiangmu -> 橡木  x2  {weixin:1,code:1}`）。

**实测**（管道带 app 参数）：`code` 里选「项目」、`weixin` 里选「橡木」之后 →
`code` 查 `项目|橡木|…`，`weixin` 查 `橡木|项目|…`，同一份词库两个应用各排各的。

#### 整句学习（2026-10-28 补，M1 续）

**要解决的问题**：逐词学习只能记住"这个词选过"，记不住"这句话我习惯这么打"。
打 `jintiantianqi` 分段选「今天」+「天气」，学到的只有 `jintian→今天`、`tianqi→天气`；
整句 `jintiantianqi→今天天气` 反而没记录，下次打同样的全拼，整句候选顶不上来。

| 层 | 做法 |
|:---|:---|
| DLL 侧 | `SentenceAcc { code, text, picks }`：第一次选词时记下**当时完整的拼音串**，之后每次选择把词拼到 `text`、`picks += 1`；这一串拼音打完（`EditAction::Commit`）就上报整句 |
| 上报门槛 | `should_report_sentence(picks, chars)` = `picks >= 2 && chars >= 4`。只选一次的不报（那一次本身已按整串记过，重复报会让计数翻倍）；四个字以下不算"整句" |
| 协议 | 复用 `type=2`，`code` 传整段全拼、`word` 传整句 —— 引擎的 `learn_key()` 按字数切音节，四个字正好把整串吃掉，落在全拼这个码下 |
| 清理 | 新起一句（空格时缓冲为空）、Esc、退格删空、回车/CapsLock/Shift 上屏英文、组合被外部终止、失焦 —— 全部丢掉未完成的记录 |

为什么两边都记（词码 + 全拼码）：它们是不同的码，互不覆盖。逐词的记录让你单独打
`xiangmu` 时管用；整句的记录让你打 `jintiantianqi` 时管用。`apply_user_boost()` 只对命中
当前这个码的条目加权，所以不会串。

**整句的撤回（2026-10-28 补）**：逐词和整句记两条独立的账 —— `commit_mark`（最后一个词）和 `sentence_mark`（整句），每按一下退格两边各减一个字：

| 操作 | 撤回什么 |
|:---|:---|
| 删掉最后那个词 | 只撤 `tianqi→天气`，整句 `jintiantianqi→今天天气` 留着 |
| 接着删到整句没了 | 再撤整句那条 |

逻辑抽成 `fn step_mark(slot: &mut Option<CommitMark>) -> Option<(String, String)>`（过期自动清槽、删光返回 `(码, 词)`），不依赖 COM，可直接单测（`删掉最后一个词只撤回那个词`、`一直删到整句没了整句也撤回`、`过期的标记不再算误选`）。

**退格由输入法自己执行**（2026-10-28 修）：`OnTestKeyDown` 认领过退格之后，有的应用就再也收不到这个键了 —— 实际体验是"按两下没反应，第三下才删掉一个字"。所以撤回窗口内不再依赖"认领 + `OnKeyDown` 返 FALSE 让应用自己删"，改成在编辑会话里 `EditAction::DeleteBack` 自己删：`GetSelection` → 有选中就删选中、没有就 `Collapse(TF_ANCHOR_START)` + `ShiftStart(-n)` → `SetText(empty)` → 光标落回删除点，`OnKeyDown` 返 `BOOL(1)` 把键吃掉。连按次数攒在 `pending_backspace`（异步会话只有一个 `pending` 槽，否则第二下会覆盖第一下）。

**删字的三级级联**（2026-10-28 修）：WPS 的正文控件里 `ITfRangeACP::GetExtent` 返回 `S_OK` 但给 `(0,0)`，`ShiftStart` 又原地不动，于是 `SetText(empty)` 作用在空区间上 —— 日志写 `ok`、界面一个字没少，用户看到的是"按两下才删掉"。所以改成三级依次试、**每级都要确认真删掉了**：① ACP `SetExtent(from, len)` + `SetText`（`anchor == 0 && cch == 0` 视为宿主不认 ACP，跳到下一级）；② `Collapse(TF_ANCHOR_START)` + `ShiftStart(-n)`，**必须 `shifted != 0`**；③ `ipc::press_backspace(n)` 注入原生 `VK_BACK`，让宿主按自己的逻辑删。注入前 `mark_injected_backspace(n)` 记次数，`OnTestKeyDown`/`OnKeyDown` 见到就返 `BOOL(0)` 放行，否则会把自己的注入键再认领一次（越删越多）；标记 1 秒过期，防止宿主吞键后把用户真正的退格一直当注入键放行。

**打错的拼音不再让原始字母抢第一屏**（2026-10-28 修）：`is_pinyin_code` 判"不像拼音"只说明它不能整串切成音节，不代表用户想打英文。再加一条判据：前缀能连着切出 ≥2 个完整音节就是拼音（`shashahs` → `sha|sha|…`），此时原始字母排到候选最后；`github`/`hello`/`test`/`world`/`thanks` 前缀凑不出两个音节，仍然排第一（保留"打英文一眼可见"的老行为）。实测 `shashahs` → 傻傻还是 傻傻 莎莎 沙沙…，英文词全部不回归。

**还没做的**：长句分词边界学习。

#### 分词边界学习（2026-10-28 补，M1 续）

**要解决的问题**：`xian` 是一个音节（先），`xi'an` 是两个（西安）。用户主动打上边界
就是在表达"我要按 xi + an 切"，但我们以前**把边界抹掉了** —— `strip_separators()` 在查
引擎之前就把 `-` / `_` 删干净，于是 `xi'an` 和 `xian` 走的是同一个码，学到的也是同一份记录。

| 层 | 做法 |
|:---|:---|
| 协议 | 送进引擎的码改成**带边界**（`-` / `_` 先统一成 `'`，见 DLL 的 `normalize_boundaries()`）；引擎自己再还原成整串喂 librime |
| 记账键 | `learn_key()` 把"吃掉几个字母"映射回带边界的原串：`xi'an` + 西安 → 学 `xi'an`；`xiang-mu` + 项目 → 学 `xiang'mu`；`pikeqiu` + 皮 → 仍是 `pi` |
| 排序 | `apply_user_boost()` 同时看两个键：**带边界**的权重 ×4、抹掉边界的 ×1。所以打 `xi'an` 时边界学过的「西安」稳赢，打 `xian` 时那份记录根本不参与 |
| 兜底 | 带边界学过的词如果 librime 这次没给（生僻词），直接补进候选最前面（最多 3 个）——用户明明为这个边界选过它 |
| 英文候选 | 打了边界就不再追加"原始字母"候选：用户已经在说"这是拼音"了 |

为什么 ×4 而不是各记一份：两份记录会让"边界"这个信号消失（`xi'an` 学完顺手把 `xian` 也带跑了）。
边际写法本身就带信息量，只有用户真的照着打才该生效。

实测：`xi'an` + 西安 学一次后 → `xi'an` 与 `xi-an` 首候选都是「西安」（n=30），
`xian` 仍是「先」（n=31）。

**坑**：librime **不认** `'` 当分隔（`xi'an` 与 `xian` 喂进去候选集一样，实测 `delimiter: " '"`
只在真实按键流里生效），所以边界只能靠我们自己记账，别指望引擎帮忙切。

**还没做的**：组合串按音节显示分隔符（`sha'sh'h'` 那样，属于显示层）。

#### 误选撤回 + 词库巡检（2026-10-27 补，M1 续）

**要解决的问题**：词库只加不减。实测 70 条里混进了 `wojude -> 我局的`、`jnude -> 峻德`、
`zuiximian -> 最西面` 这种明显选错的，它们会一直污染排序；另有 15 条单字条目
（`de -> 的`、`zai -> 在`…）纯噪音 —— 单字本来就该由语言模型排，学一遍只会跟模型打架。

**判据：上屏后马上删掉 = 选错了。** 这是最可靠的行为信号，不需要 AI、不需要语言模型。

| 层 | 做法 | 为什么这样做 |
|:---|:---|:---|
| DLL 侧 | 提交后记一个 `CommitMark`（码、词、剩余退格数、2 秒期限）。非拼写态按退格时递减，删光就发撤回，然后把退格**放行**给应用 | 不用 `ITfTextEditSink`：那要新增编辑观察者 + 跟踪 range，出错就是挂死（参见 1.0.1 的选词死锁）。数退格不需要新接口，退格本来就由我们处理 |
| 引擎侧 | `unlearn()`：计数 >1 就减 1，否则删条目；`should_learn()` 三道筛子（空词 / 打英文 / 单字）挡在 `learn()` 前面 | 减而不是直接删：同一个词选过 3 次不会因为一次手滑清零 |
| 命令行 | `--review-dict` 按次数升序列出全部条目（单字打标）/ `--prune-single` 删单字 / `--forget <码> [词]` 删单条 | 旧版本已经学进去的噪音，得有个不用手改 tsv 的清理入口 |

**协议 v4**（在 v3 后面追加，不影响 v3 报文）：

```
type=3 撤回学习： [u8 3][u32 code_len][code utf8][u16 word_len][word utf8]
```

**为什么「减 1」而不是「加一次负分」**：条目本身就是 `码 -> 词 -> 次数`，次数是唯一排序依据；
用负数会让 `apply_user_boost` 的比较变复杂，而且负数没有语义。

**为什么撤回不出现在候选里**：撤回只改学习记录，不改当前文档 —— 用户已经按了退格，
文字本来就被删掉了。输入法再「帮」他插回去是灾难。

**坑**：`argv` 是 ANSI 编码（中文系统上是 GBK），`--forget wojude 我局的` 里的中文
永远匹配不上 UTF-8 存的词 → 加了 `argv_to_utf8()`（`MultiByteToWideChar(CP_ACP)` +
`WideCharToMultiByte(CP_UTF8)`）。
### T5b 拼音音节分隔显示

✅ **2026-10-28 已实现（M1 续）**，但走的不是这条路线：librime 的 preedit 确实自带
`jian zou pian feng`（要改查询响应协议把 preedit 带回来），而我们在 DLL 里用 402 音节表
自己切分（`pinyin::compose_view`），一行协议都不用动，还能单测。

- 缓冲里存用户原样敲的串，文档里和候选窗拼音行显示的都是带 `'` 的形式
  （`xiangmu` → `xiang'mu`、`jintiantianqi` → `jin'tian'tian'qi`、`zheg` → `zhe'g`）
- 用户自己按的 `-`/`_` 归一成 `'` 并优先保留（`xi-an` 显示 `xi'an`，不会又被并成 `xian`）
- 光标换算：落在音节边界时停在分隔符**前面**（`xiang|'mu`），因为退格删的是前一个字母
- 显示串只含 ASCII，所以下标 = 字节 = UTF-16 长度，`SetText` / `ShiftEnd` 可以直接用
- 单测：`组合串显示带音节分隔`、`显示光标停在音节边界前`

### T5c 候选窗美化

选中项圆角、间距微调、配色统一。

### T5d 拼音下划线

`ITfDisplayAttributeProvider` + `IEnumTfDisplayAttributeInfo` + 新 CLSID + `GetTextExt` 已有。
约 350 行，需动注册（加性改动）。注意 `ITfProperty::SetValue` 要开 `Win32_System_Ole` +
`Win32_System_Variant` 特性。

### T5e 云词库（自建「个人云」）

不抄搜狗的公共云（几百万用户数据，闭源）。做**个人云**：
Obsidian 语料 + AI 兜底，默认关闭联网（符合项目铁律）。

### T5f librime 工程化

- `rime.dll` / `rime\data` / `rime\user` 目前靠手工从 Weasel 安装包解出（见 `rime-server/README.md`）
- 后续可做一键安装脚本，并考虑裁剪 `rime\data`（现在 13MB，包含用不到的方案）
### T5g 模糊音（an/ang、zh/z 等）

**现象**：打 `chuanjianba`（少打一个 g）出「船舰吧」，期望「创建吧」。

**已确认不是词库问题**：`创建` 在词库里，`chuangjianba` 能出「创建吧」。

**根因**：rime 默认不开模糊音，`chuan` 和 `chuang` 是两个不同音节。

**试过的方案**：
1. 用户目录 `luna_pinyin.custom.yaml` + `speller/algebra/__append` —— 无效，
   而且把原来的 `__patch`（简拼/纠错）覆盖掉了
2. 直接改 `luna_pinyin.schema.yaml` 的 algebra 加 derive 规则 —— 生成了正确的
   build schema，但 rime 不重新编译 prism，**改动不生效**
3. 删除 `user\build` + `*.prism.bin` 强制重建 —— rime 报 maintenance done
   但没生成任何 schema/prism，**中文直接没了**（已从安装包副本恢复）

**结论**：rime 的 prism 重建路径还没摸清，动数据风险高。

**下次可选路线**（按风险从低到高）：
- A. 在 `rime-server` 里做模糊音：拿到 preedit 的音节切分后，逐音节生成
  an↔ang / en↔eng / in↔ing / zh↔z / ch↔c / sh↔s 变体，再 simulate 变体合并候选。
  不动 rime 数据，完全可控。
- B. 搞清 rime 的 deploy 流程（看 rime 自己的日志），让 prism 正常重建。
- C. 用 `RimeDeploySchema` / `RimePrebuildAllSchemas` 主动触发单方案重建。

#### ✅ 2026-10-27 已解决（走的还是 B 路线，但之前三步都错在细节）

之前三次失败的真实原因：

1. **文件名认错 scheme**：写的是 `luna_pinyin.custom.yaml`，而我们实际 deploy 的是
   `luna_pinyin_simp`（`rime-server.cc` 里 `RimeSelectSchema(g_session, "luna_pinyin_simp")`）。
   custom 文件名必须逐字匹配 schema id，否则 rime 根本不会读它。
2. **键路径错**：`speller/algebra/__append` 是无效写法。正确做法是**整段替换** `speller/algebra`，
   用 `__patch:` 把 `pinyin.yaml` 里的模糊音片段拉进来：

   ```yaml
   patch:
     speller/algebra:
       __patch:
         - pinyin:/zh_z_bufen      # 模糊音片段，必须排在简拼之前
         - pinyin:/abbreviation
         - pinyin:/spelling_correction
         - pinyin:/key_correction
   ```
3. **没重启引擎**：librime 在 `RimeInitialize` 时才发现配置变新并 deploy。旧脚本改完文件就以为生效了。

现在这套做成了 `tools\apply-fuzzy.ps1`（读 `rime-config\fuzzy.yaml` → 生成 custom.yaml →
重启引擎 → 等 deploy → 用管道查 `zi` 验证）。**默认全关**：实测开 `zh_z` 后打 `zi`
第一个候选从「子」变成「之」、`cang` 从「仓」变成「长」，对打字准的人是净损失。

要开就把配置里对应项改成 `true` 再跑一次脚本。配置分两层：

| 文件 | 归谁 | 进 git |
|:---|:---|:---|
| `rime-config\fuzzy.yaml` | 出厂默认（全关），装给所有人 | ✅ |
| `rime-config\fuzzy.local.yaml` | 本机个人偏好，脚本优先读它 | ❌ 已 ignore |

`tools\release.ps1` 打包时显式传 `-ConfigPath` 指出厂默认，保证发布包里永远是全关，
不会把开发机的个人偏好打进安装包。

### T5 状态盘点（2026-10-26 更新）

**已完成**：

- 中文标点：`,` `。` `;` `/` `\` `[` `]` `'` `-` `=` 全部转中文；`/` 和 `\` 都出顿号
- Shift 变体：`Shift+/` → `？`、`Shift+;` → `：`、`Shift+,` → `《` 等
- 持久英文模式：Shift 单击切换（松开才生效；期间按过别的键就不算单击），CapsLock 开 = 大写英文
- CapsLock 检测改用 `GetKeyState`（`GetAsyncKeyState` 低位不可靠，之前大写一直失效）
- 英文候选：`github` / `vscode` 这类不像拼音的输入，原始串排候选第一
- 32 位 DLL：WPS 等 32 位应用可用（注册到 Wow6432Node）
- 候选窗：收起 1×5 / 展开 5×5，**只给选中行标 1-5**，↑↓ 换行、←→ 换列、数字键选当前行
- **分词符**：`-`/`_` 留在组合串、查询前剥离；**显示层按音节插 `'`**（`xiangmu` → `xiang'mu`、`xi-an` → `xi'an`、`zheg` → `zhe'g`）
- **分段上屏**：选字后剩余拼音保留继续选（`pikeqiu` 选「皮」→ 串变 `keqiu`）；尾巴多打的字母按汉字数吃掉（`pikeqiux` 3 字 → 吃掉 8 个）
- **模糊音**（2026-10-27）：`rime-config\fuzzy.yaml` 七组开关 + `tools\apply-fuzzy.ps1` 生成配置并重启引擎验证；默认全关（开了会污染候选）
- **纠错探针**（2026-10-27，引擎侧）：`niihao`→你好、`xiangmuu`→项目、`zhemge`→这个、`shanghhai`→上海、`nihoa`→你好、`jintain`→今天。详见下面「纠错探针怎么工作」
- **M1 个人词库**（1.0.0–1.0.13）：选词记忆、误选撤回、整句学习与整句撤回、跨应用记账、分词边界、`--review-dict` 巡检
- 47 个单测（`cargo test --release`）+ 引擎 32 项 `--selftest`
- 安装包 + 一键安装/卸载脚本（带完整日志 + 8 项自检）
- 仓库清理：14.4 GB → 563 MB

**未完成**（按优先级）：

| | 内容 | 备注 |
|:---|:---|:---|
| T5d | 拼音下划线 | 现在拼音像普通英文文字，是"最不像输入法"的地方 |
| T5c | 候选窗美化 | |
| T5e | 个人云词库 | Obsidian 语料 + AI 兜底 |
| T5f | librime 工程化 | 现在靠手工解 Weasel 包 |

（T5a 用户词库学习已 ✅ 2026-10-27 实现；T5b 音节分隔显示已 ✅ 2026-10-28 实现，见同名小节；T5g 模糊音见「✅ 2026-10-27 已解决」）

### 纠错探针怎么工作（2026-10-27）

放在 **C++ 引擎侧**（`rime-server/rime-server.cc` 的 `query()`），不是 DLL 侧：一次管道往返就能把
所有变体探完，DLL 侧做的话每个变体都要一次 IPC（2–5ms × 8，超 50ms 预算）。协议没变，DLL 不用动。

触发条件是三层筛子，正常输入零开销：

| 输入状态 | 例子 | 行为 |
|:---|:---|:---|
| 整串能切成完整音节 | `xiangmu` / `jintian` | **一个探针都不发** |
| 末尾音节只打了一半 | `zheg` / `xiangm` / `shiq` | 不插手：librime 的前缀匹配本就对，插手会把「项目」换成「向」 |
| 中间多/错/换位一个字母 | `zhemge` / `niihao` / `nihoa` / `jintain` | 生成变体 → 查 → **纠错结果排最前** |

变体按可能性生成并排序：① 删相邻重复字母 ② 相邻换位 ③ 删任意一个字母；每个变体必须先
`is_full_pinyin()` 通过才去查 rime（所以 `github`/`vscode` 这种英文词一个变体都产不出来）。
同一族的变体再用 `variant_score()` 排序——惩罚"需要单字母音节"的切分，`nihoa` 的 `nihao`
（ni+hao）因此排在 `nioha`（ni+o+ha）前面。

兜底：不像拼音的输入，英文原样紧跟纠错结果第二位（`update` 这种"恰好差一个字母就是合法拼音"
的英文词仍然一眼能看到），不像拼音又没有纠错结果时（`github`）原样排第一。

明确不做：插入字母、混淆集替换（模糊音已覆盖）、编辑距离 2。

**Shift 组合键不再误切英文**（2026-10-28 修）：`Shift+=`（`+`）、`Shift+-`（`_`）在空闲态**不认领**（交给应用原样插入半角），于是 TSF 不调 `OnKeyDown`，`shift_used` 一直是 `false`，松开 Shift 就被 `OnKeyUp` 当成"单击切换中英文" —— 用户实测"按了 Shift+加号减号之后切不回中文"。修法：把"这次按键是 Shift 组合键"的判断提到 `OnTestKeyDown`（`is_shift_chord(vk, shift, ctrl_alt_win)`，排除 Shift 本身与 Ctrl/Alt/Win 快捷键），`OnKeyDown` 里再记一次做双保险。**教训**：凡是"根据按键序列推断意图"的状态，都要在最早的那个回调里记，不能挂在"认领之后才走"的路径上。

**鼠标点候选 + 数字后面的半角标点**（2026-10-28 修）：用户实测两个问题 ——「待选的文字我想用鼠标去点击会直接留下英文而不是中文」和「输入数字之后打的。会是.而不是。」。
- 鼠标：根因不是"没实现点击"，而是**实现成了点击穿透**（`WM_NCHITTEST -> HTTRANSPARENT`），点候选等于在文档里点了一下，组合串被打断、拼音原样留在文档里。改法：窗口自己收 `WM_LBUTTONDOWN`，`hit_index()` 把窗口坐标换成候选下标（整格可点，含序号那一小块），`hide()` 之后通过 `set_click_handler` 存下的函数指针回调 `lib.rs` 的 `handle_cand_click()` —— 上屏必须走编辑会话，只能在那边发。和空格/数字键走的是同一个 `apply_pick()` + `finish_pick()`，所以分段上屏、学习、撤回标记全都一致。
- 标点：`。` `，` `：` 在数字后面应该是半角（`3.14`、`1,000`、`10:30`）。按键线程看不到文档内容，所以新增 `EditAction::InsertPunct(全角, 半角)`，由编辑会话里的 `preceding_char_is_digit()` 用 `ITfRange::ShiftStart(-1)` + `GetText` 读光标前一个字符再决定（读完 `Collapse(TF_ANCHOR_END)` 把光标放回原处）。有选区时直接算"不是数字"，反正接下来是整段替换。
- 顺带把 `Insert` 分支里的插字逻辑抽成 `insert_at_caret()`，两个分支共用（含 `SendInput` 兜底）。

**测试能力现状**：

- 有：`cargo test` 45 个单测（按键认领、标点映射与引号交替、半角符号不转换、数字后半角标点、Shift 组合键不误判单击切换、拼音切分、分词边界归一、分段上屏消耗字母数、误选撤回与整句撤回的记账、整句上报门槛、候选窗上下翻与贴边、候选窗点击命中格、注入退格按次数放行与过期）
- 有：`rime-server.exe --selftest` 32 项引擎侧断言（`utf8_char_count` / `strip_separators` /
  `normalize_boundaries` / `split_lengths` / `learn_key` / `should_learn` / `boost_score`），纯 ASCII，不初始化 librime
- 有：`tsf/test/test-harness.cc` GUI 自动化测试台（需要交互式桌面，agent 抢不到焦点）
- 有：`tools/engine-pipe.ps1`（`Invoke-InklinkQuery`）直连引擎管道，纠错/模糊音/学习都靠它回归
- 缺：端到端验证（TSF 是否真把字符插进应用）只能人工测；个人词库的学习效果目前靠管道手工上报验证

**一个教训**：`/` 当初映射成全角 `／`，技术上对（微软拼音如此），但视觉上和 `/` 无法区分，
用户以为没转换。这类"看不出差别"的映射应优先选用户能感知的，而不是照搬某个实现。
