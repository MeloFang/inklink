# AGENTS.md

> 给所有 AI 与人类贡献者：读完这一份就能开工，不必先问基础问题。
> 最后更新：2026-10-21（阶段②，代码尚未开始）

## 这是什么

**inklink**：Windows 上开源的中文输入面板。热键唤起 → 拼音输入 → 用「内置词库 + 个人词库 + 使用习惯」出候选 → 上屏到当前窗口。AI（本地 Ollama / 远程 API）做续写、翻译、段落优化。

完整设计：[`docs/architecture.md`](docs/architecture.md)。产品边界：[`PRODUCT.md`](PRODUCT.md)。

**许可证：GPL-3.0**（见 `LICENSE`）。基础词库衍生自 [rime-ice](https://github.com/iDvel/rime-ice)（GPL-3.0），
以独立资源包分发（`data/` 不进 git），见 `docs/adr/0004`。

## 三条铁律（违反即回滚）

1. **不进入别人的进程**：不写 DLL、不做 TSF 注入、不 `SetWindowsHookEx` 接管键盘。见 `docs/adr/0001`。
2. **同步路径 50ms 封顶**：AI 与磁盘 IO 一律异步。见 `docs/adr/0002`。
3. **默认不联网**：任何网络请求必须是用户显式开启的。语料记录默认关闭。

## 目录结构

```
inklink/
├─ PRODUCT.md            产品定义与成功标准
├─ PLAN.md               实施计划与状态（改代码前先看这里）
├─ HANDOFF.md            续接文档（每阶段更新）
├─ AGENTS.md             本文件
├─ GLOSSARY.md           术语表
├─ docs/
│  ├─ architecture.md          架构与数据流
│  ├─ base-lexicon-eval.md     词库选型实测
│  ├─ adr/                     决策记录
│  └─ inklink项目复盘.md        上一版失败复盘
├─ app/                  【M0 待建】Rust + Tauri 应用
│  ├─ src-tauri/          引擎、热键、上屏（Rust）
│  └─ src/                面板 UI（Svelte + TS）
├─ tools/                词库转换脚本（Python，开发期）
├─ data/                 词库资源包（**不进 git**）
└─ extractor/            v1 笔记抽取管线（M2 复用）
```

## 构建与测试

**现在可跑（Python，仅词库与语料处理）**
```powershell
cd C:\dsh\work\inklink
.\.venv\Scripts\python.exe extractor\scan_vault.py
.\.venv\Scripts\python.exe extractor\build_phrase.py
```

**M0 之后（Rust，步骤 0 建好后可用）**
```powershell
# ⚠️ 必须先修正 PATH：本机 Program Files 下装着 GNU 版 rustc 1.98，
#    它会盖住 rustup 的 MSVC 版，导致 `dlltool.exe not found`。
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

cd app
npm install
npm run check                   # svelte-check，0 error 0 warning
npm run build                   # 产出 ../build
cargo tauri dev                 # 开发运行
cargo test                      # 全部单测
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

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
