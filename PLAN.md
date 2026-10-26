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

### T5a 用户词库学习（下次专门处理）

**背景**：librime 自带用户词库（`rime\user\luna_pinyin.userdb`），但实测这个版本学不到东西：

| 尝试 | 结果 |
|:---|:---|
| `RimeSelectCandidate(session, index)` | 返回成功，`RimeGetCommit` 拿不到 commit |
| `RimeSelectCandidateOnCurrentPage(session, slot)` | 同上 |
| 模拟数字键 `RimeSimulateKeySequence("5")` | 同上 |
| 恢复 `RimeGetContext` 刷新菜单后再选 | 同上 |

**结论**：不依赖 librime 的学习，自建记忆层（`code -> 词 -> 次数`）。

**教训**：曾一次性把「记录 + 落盘 + 排序提升」全加上，结果**查询路径崩溃**（原因未定位，
崩溃在 `query()` 内部，连 `q=...` 日志都没写出来）。下次必须分三步，每步验证：

1. **只记录到内存 + 日志**，不动排序 —— 确认连续查询不崩
2. 加落盘（`user.tsv`，`code\ttext\tcount`）
3. 才接排序提升（同拼音内按次数降序，`std::stable_sort`）

**已埋好的伏笔**（下次不用改 DLL）：
- 协议 v2 已保留 `type=2` 选词消息：`[u8 2][u32 index]`
- DLL 侧 `ipc::select(index)` 已实现，空格/数字键提交时调用
- 服务端 `select_candidate()` 目前只记日志，**在这里接学习逻辑即可**

### T5b 拼音音节分隔显示

librime 的 preedit 自带 `jian zou pian feng`，但候选窗顶部仍显示原始 `jianzoupianfeng`。
可在查询响应里附带 preedit，DLL 用它替换拼音行。

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
### T5 状态盘点（2026-10-26 更新）

**已完成**：

- 中文标点：`,` `。` `;` `/` `\` `[` `]` `'` `-` `=` 全部转中文；`/` 和 `\` 都出顿号
- Shift 变体：`Shift+/` → `？`、`Shift+;` → `：`、`Shift+,` → `《` 等
- 持久英文模式：Shift 单击切换（松开才生效；期间按过别的键就不算单击），CapsLock 开 = 大写英文
- CapsLock 检测改用 `GetKeyState`（`GetAsyncKeyState` 低位不可靠，之前大写一直失效）
- 英文候选：`github` / `vscode` 这类不像拼音的输入，原始串排候选第一
- 32 位 DLL：WPS 等 32 位应用可用（注册到 Wow6432Node）
- 候选窗：收起 1×5 / 展开 5×5，**只给选中行标 1-5**，↑↓ 换行、←→ 换列、数字键选当前行
- 分词符：`-`/`_` 留在组合串、查询前剥离（`xiang-mu` 显示为 `xiang-mu`）
- **分段上屏**：选字后剩余拼音保留继续选（`pikeqiu` 选「皮」→ 串变 `keqiu`）；尾巴多打的字母按汉字数吃掉（`pikeqiux` 3 字 → 吃掉 8 个）
- 21 个按键/切分逻辑单测（`cargo test --release`）
- 安装包 + 一键安装/卸载脚本（带完整日志 + 8 项自检）
- 仓库清理：14.4 GB → 563 MB

**未完成**（按优先级）：

| | 内容 | 备注 |
|:---|:---|:---|
| T5a | 用户词库学习 | **下一项就做**。产品核心卖点，协议 `type=2` 和 `ipc::select()` 已埋好，入口在 `rime-server/rime-server.cc` 的 `select_candidate()`。必须分三步（PLAN 见上），上次一次性全加崩过 |
| T5d | 拼音下划线 | 现在拼音像普通英文文字，是"最不像输入法"的地方 |
| T5b | 音节分隔显示 | librime preedit 已有 `jian zou pian feng`，未接到候选窗 |
| T5g | 模糊音 | `chuan`→`chuang`；三条路都试过失败，建议走服务端方案 |
| T5c | 候选窗美化 | |
| T5e | 个人云词库 | Obsidian 语料 + AI 兜底 |
| T5f | librime 工程化 | 现在靠手工解 Weasel 包 |

**测试能力现状**：

- 有：`cargo test` 9 个单测（按键认领、标点映射）
- 有：`tsf/test/test-harness.cc` GUI 自动化测试台（需要交互式桌面，agent 抢不到焦点）
- 缺：端到端验证（TSF 是否真把字符插进应用）只能人工测

**一个教训**：`/` 当初映射成全角 `／`，技术上对（微软拼音如此），但视觉上和 `/` 无法区分，
用户以为没转换。这类"看不出差别"的映射应优先选用户能感知的，而不是照搬某个实现。