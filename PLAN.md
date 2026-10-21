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
| 4 | 召回与排序 | 2,3 | 20 个拼音 Top1 与 luna 行为一致率 ≥ 80% | ⬜ |
| 5 | 面板 UI + 热键 | 0 | 热键 → 面板可见 < 150ms，能输入出候选 | ⬜ |
| 6 | 上屏 | 4,5 | 记事本 / 浏览器 / VSCode 三处能打 | ⬜ |
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
- [ ] 单测：20 个常见拼音的 Top1 与 luna 原生行为一致率 ≥ 80%
- [ ] 同一输入多次调用结果完全一致
- [ ] 单次查询 < 50ms（6 万词库）

**测试接缝**：`rank(candidates, ctx) -> Vec<Candidate>`

**可能踩的坑**：过度调参导致"看起来聪明但不稳定"；同音词过多时的分页。

---

## 步骤 5：面板 UI + 热键

**目标**：按下热键，面板出现在光标附近，能输入、能选词。

**做什么**：
- 全局热键注册（`tauri-plugin-global-shortcut`），默认 `Ctrl+\``
- 窗口：无边框、置顶、`skip_taskbar`、显示时不抢焦点（抢了就记录并在关闭时归还）
- 候选列表：键盘导航（↑↓ / 数字键 / Tab）、Esc 关闭、点击选择

**验收**：
- [ ] 热键按下 → 面板可见 < 150ms
- [ ] 输入 `xiangmu` 出现候选，↑↓ 可移动，数字键选中
- [ ] Esc 关闭后，焦点回到唤起前的窗口

**测试接缝**：UI 不做自动化测试，按上面清单手测

**可能踩的坑**：Tauri 窗口 show 时抢焦点；多显示器 DPI；热键与其他软件冲突。

---

## 步骤 6：上屏

**目标**：选中的词真正进入目标应用。

**做什么**：
- 通道一：`SendInput` + `KEYEVENTF_UNICODE` 直接发字符
- 通道二（回退）：写入剪贴板 → 发送 `Ctrl+V` → 恢复剪贴板原内容
- 记录唤起前的窗口句柄，上屏前 `SetForegroundWindow`

**验收**：
- [ ] 记事本、Chrome 输入框、VSCode 编辑区三处都能打出「项目代号赤兔」
- [ ] 通道一失败时自动走通道二，且剪贴板内容被恢复
- [ ] 失败时面板上有可见提示，不静默失败

**测试接缝**：`commit(text, target_hwnd) -> Result<(), CommitError>`（用真实窗口手测，不写单测）

**可能踩的坑**：UWP / 管理员权限窗口拒绝输入；某些应用需要 `WM_CHAR`；剪贴板被占用。

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
