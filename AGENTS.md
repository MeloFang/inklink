# AGENTS.md

> 给所有 AI 与人类贡献者：读完这一份就能开工，不必先问基础问题。
> 最后更新：2026-10-26（TSF 输入法 + librime 引擎已在本机日常使用）

## 这是什么

**inklink**：Windows 上开源的中文输入法。装进系统后就是一个正常的 TSF 输入法（Win+Space 切换），
打拼音 → 候选窗 → 上屏。产品愿景是用个人词库 + Obsidian 语料 + AI 做个性化，目前只到 M0（能打字）。

架构（ADR-0006）：`inklink_tsf.dll`（薄壳，装在应用进程里）──命名管道──▶ `rime-server.exe`（x86，链 librime）。
**为什么分两个进程**：Weasel 分发的 `rime.dll` 只有 32 位，而 TSF DLL 必须 64 位。

完整设计：[`docs/architecture.md`](docs/architecture.md)。产品边界：[`PRODUCT.md`](PRODUCT.md)。
当前进度与下一步：[`HANDOFF.md`](HANDOFF.md) 第九节。

**许可证：GPL-3.0**（见 `LICENSE`）。基础词库衍生自 [rime-ice](https://github.com/iDvel/rime-ice)（GPL-3.0），
以独立资源包分发（`data/` 不进 git），见 `docs/adr/0004`。

## 三条铁律（违反即回滚）

1. **引擎绝不进别人的进程**：TSF DLL 是允许的（见 `docs/adr/0006`），但它只能是**薄壳**——
   不含词库、不含引擎、不启动线程、`DllMain` 不做事、`DllCanUnloadNow` 恒返回 `S_FALSE`。
   所有重活（切分、召回、排序）必须在独立进程里，通过命名管道通信。见 `docs/adr/0006`。
2. **同步路径 50ms 封顶**：AI 与磁盘 IO 一律异步。见 `docs/adr/0002`。
3. **默认不联网**：任何网络请求必须是用户显式开启的。语料记录默认关闭。

## 目录结构

```
inklink/
├─ VERSION               版本号单一真相源（X.V.Z，见下）
├─ PRODUCT.md            产品定义与成功标准
├─ PLAN.md               实施计划与状态（改代码前先看这里）
├─ HANDOFF.md            续接文档（每阶段更新）
├─ AGENTS.md             本文件
├─ GLOSSARY.md           术语表
├─ tsf/                  【现在的主线】TSF 输入法 DLL（Rust，64 + 32 位）
│  ├─ src/lib.rs         按键状态机 + 组合串 + 候选导航
│  ├─ src/candwin.rs     候选窗（Win32 弹出窗口）
│  ├─ src/display.rs     组合串的下划线（ITfDisplayAttributeProvider + GUID_PROP_ATTRIBUTE）
│  ├─ src/ipc.rs         命名管道客户端 + 引擎自动拉起
│  ├─ src/pinyin.rs      402 音节表（贪心最长匹配；算分段上屏吃掉多少字母 + 组合串按音节显示的 `'`）
│  ├─ src/state.rs       全局状态（注册表 HKCU\Software\inklink\State：中英、中文标点）
│  ├─ src/ui_notify.rs   让浮条出现/收起（Activate→PostMessageW(WM_APP+12)，Deactivate→+13）
│  └─ build.rs           把 VERSION 编进 DLL
├─ ui/                   常驻 UI 进程（浮条 + 托盘；独立 crate，不加载进别人进程）
│  ├─ src/main.rs        浮条窗口：中/英方块、标点方块、☰、拖动记忆、按 WM_APP+12/13 显隐
│  ├─ src/state.rs       注册表读写（State / UI 位置 / 开机自启）
│  └─ src/tray.rs        托盘图标 + 右键菜单
├─ rime-server/          C++ 引擎服务（链 librime，x86）
│  └─ rime-server.cc     协议 [u8 type] + 查询/选词；个人词库学习层 + --selftest
├─ rime-config/          引擎侧配置源
│  └─ fuzzy.yaml         模糊音七组开关（默认全关）
├─ installer/            安装/卸载脚本模板（版本号由打包脚本注入）
├─ tools/                构建部署与打包脚本 + 词库转换
│  ├─ version.ps1        读写 VERSION
│  ├─ deploy-local.ps1   构建 + 部署到本机
│  ├─ apply-fuzzy.ps1    按 fuzzy.yaml 生成 rime custom.yaml 并重启引擎验证
│  ├─ engine-pipe.ps1    命名管道客户端（诊断/验证，可 dot-source）
│  ├─ probe-legacy-select.ps1  回归探针：老协议选词不会让引擎死锁
│  ├─ make-icon.py       从品牌图形生成 installer\inklink.ico（产品图标）+ inklink-badge.ico（浮条第一格）
│  └─ release.ps1        打 dist\inklink-<版本>.zip
├─ dist/                 发布产物（**不进 git**）
├─ docs/                 架构、ADR、阶段记录
├─ app/                  【已退役】旧 Tauri 面板 + 自研引擎，只作参考
├─ data/                 词库资源包（**不进 git**）
└─ extractor/            v1 笔记抽取管线（M2 复用）
```

## 版本号与发布

**格式 `X.V.Z`**（`VERSION` 文件是唯一真相源，`tools/version.ps1` 负责写并同步 `Cargo.toml`）：

| 位 | 含义 |
|:---|:---|
| X | 里程碑序号：0 = M0 能打字，1 = M1 个人词库，2 = M2 Obsidian 语料，3 = M3 AI |
| V | 里程碑内的阶段序号，进入新阶段时 +1 |
| Z | 修订号，每次发版/部署自增；**进入新阶段（V+1）或新里程碑（X+1）时归零** |

归零规则沿用 SemVer 2.0.0（主版本号递增时次版本号与修订号归零，次版本号递增时修订号归零）：
X 变 → V/Z 都归零（`tools\version.ps1` 里 `Set-InklinkVersion 1.0.0`）；
V 变 → Z 归零（`Step-InklinkStage`）；其余 → Z+1（`Step-InklinkVersion`，`deploy-local.ps1` 自动做）。

例：`0.1.22` = M0 第 1 阶段的第 22 次修订（即旧命名的 v22）；**M0 之后的 M1 第一个版本是 `1.0.0`，不是 `1.0.23`**。

命名出现在四处且必须一致：`VERSION`、DLL 文件名（`inklink_tsf_0.1.23.dll`）、
安装包（`dist\inklink-0.1.23.zip`）、安装界面与日志（`C:\Users\Public\inklink-install.log`）。

```powershell
# 改完代码后一键构建 + 部署（会自动 Z+1）
powershell -ExecutionPolicy Bypass -File tools\deploy-local.ps1
# 只是重新部署、不动版本号
powershell -ExecutionPolicy Bypass -File tools\deploy-local.ps1 -NoBump
# 打发布包
powershell -ExecutionPolicy Bypass -File tools\release.ps1
```

⚠️ `-ExecutionPolicy Bypass` 必须带：本机 `CurrentUser` 策略是 `RemoteSigned`，未签名的本地脚本会被拒。
**产物不会无限堆积**：`release.ps1` 打完包自动清理 `dist\` 里的旧版本，只留刚打出来的那个 +
`-KeepOld` 指定个数的历史版本（默认 1）。`deploy-local.ps1` 部署后会删掉注册表已不再引用的旧
`inklink_tsf*.dll`；正被程序占用的会跳过并提示名字，重启那些程序后再跑一次 `-NoBump` 就清掉了。
打包先压到 `%TEMP%` 再整体 `Move-Item` 替换，避免"先删旧包、压缩又失败"把发布包弄丢。

## 构建与测试

```powershell
# ⚠️ 必须先修 PATH：Program Files 下装着 GNU 版 rustc，会盖住 rustup 的 MSVC 版
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

cd C:\dsh\work\inklink\tsf
cargo test --release                       # 54 个单测（按键认领 / 标点 / 引号 / 拼音切分 / 音节分隔显示 / 分词边界 / 分段上屏 / 误选撤回 / 整句上报 / 候选窗摆放与点击 / 注入退格抑制 / 显示属性 / 全局状态缺省值）
cargo build --release --lib --target-dir target-static
cargo build --release --lib --target i686-pc-windows-msvc --target-dir target-static

# 浮条（独立进程，不加载进别的应用；改完必须重启它才生效）
cd C:\dsh\work\inklink\ui
cargo build --release
```

引擎是 C++（`rime-server/rime-server.cc`），编译命令写在文件头注释里，需要 32 位 MSVC 环境
（`vcvars32.bat`）；日常改 DLL 不需要动它。

引擎侧还有一份可单测的纯逻辑：`rime-server.exe --selftest` 跑 32 项断言
（`utf8_char_count` / `strip_separators` / `normalize_boundaries` / `split_lengths` / `learn_key` /
`should_learn` / `boost_score`），纯 ASCII 输出，
不初始化 librime、不碰管道，改学习逻辑后先跑它。

引擎侧配置（模糊音）在 `rime-config\fuzzy.yaml`，用 `tools\apply-fuzzy.ps1` 生成到
`C:\inklink-engine\rime\user\` 并重启引擎验证。**custom.yaml 的文件名必须逐字等于真正 deploy 的
schema id**（我们是 `luna_pinyin_simp`，不是 `luna_pinyin`），且改完必须重启 `rime-server`
——librime 只在 `RimeInitialize` 时重新 deploy。

配置分两层，脚本按序解析：`-ConfigPath` 显式指定 → `rime-config\fuzzy.local.yaml`（本机个人偏好，
**不进 git**）→ `rime-config\fuzzy.yaml`（出厂默认，全关）。`tools\release.ps1` 显式传出厂默认那
一份，所以发布包里永远是全关，不会把开发机的个人偏好打进去。

> 环境事实：rustup 默认 `stable-x86_64-pc-windows-msvc`（1.99.1）；VS 2022 Community + Windows SDK 10.0.26100 已装；Node 24 / Python 3.13 已装；**无 .NET**。

> ⚠️ **在 DSH 会话里验证 GUI**：本会话是提权进程（High Mandatory Level），WebView2 的 Chromium 沙箱会失败，报
> `Failed to setup app: 拒绝访问。(os error 5)`。验证时临时加：
> `$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS='--no-sandbox'`。
> 用户自己双击运行时是普通权限，**不需要**这个变量，也不要写进发布配置。

## 编码约定

- **语言**：标识符英文，注释与文档中文；文件名用英文 kebab-case。
- **Rust**：默认 `rustfmt`；`clippy -D warnings` 零容忍；错误用 `thiserror`，不用 `unwrap()`（测试与初始化除外）。
- **Python**：保持现有风格（4 空格、类型标注可选）；不引入新依赖除非在 PLAN 里写明。
- **提交**：Conventional Commits，如 `feat(engine): 拼音切分支持简拼`、`fix(panel): 修复焦点未归还`。
- **词条格式**：TSV `词 \t 拼音(空格分音节) \t 词频`，UTF-8 无 BOM。
- **管道协议**：`[u8 type]` 开头，改字段必须**向后兼容**且兼容分支立刻生效——引擎读可选字段前先
  `PeekNamedPipe` 探测（`wait_readable()`），不许在 `read_exact` 上死等（老客户端不发的字段永远等不到，
  会把应用的 UI 线程挂死）。DLL 侧所有管道读都带超时（`read_exact_timeout`）。改协议后跑
  `tools\probe-legacy-select.ps1`。

## 测试策略

- **只在预先约定的接缝上写测试**（PLAN 每步有「测试接缝」）；未经确认的接缝不写。
- 引擎（切分/召回/排序）必须有单测，用固定小词库（50 条）做 fixture，不依赖真实词库。
- UI 不做自动化测试，用手测清单（PLAN 每步「验收」）。
- 期望值必须来自独立真相源（手工标注的样例），不许用被测代码自己算。

## 工作流

1. 读 `PLAN.md`，只做最上面一个未完成步骤。
2. 先写失败测试（红），再写最小实现（绿）。
3. 完成后立刻更新 `PLAN.md` 状态 + `HANDOFF.md` 进度，并提交。
4. 遇到新术语补 `GLOSSARY.md`；遇到难以逆转的决策开 ADR。

## 禁止事项

- ❌ 把词库文件提交进 git（`data/` 已在 `.gitignore`）。
- ❌ 在 UI 线程做文件读写或网络请求。
- ❌ 未经确认就引入大依赖（Tauri 插件、模型库）。
- ❌ 在候选列表里塞入未完成的 AI 结果（会闪烁、会覆盖用户输入）。
