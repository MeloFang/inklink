# inklink

给 Windows 用的中文拼音输入法（TSF），目标是**个人的词库真的属于个人**：喜欢用的词排前面，选错了能撤回，以后还会用你自己的语料（Obsidian）和本地/在线大模型继续学。

> 状态：能日常用。个人词库、误选撤回、整句学习、模糊音、暗色外观、设置窗都已就位；AI 相关功能还在计划里。

![inklink](docs/brand/inklink-lockup.png)

## 安装

从 [Releases](https://github.com/MeloFang/inklink/releases) 下载最新版，两种都行：

- `inklink-<版本>-setup.exe` —— 单文件安装器，双击，允许提权即可（推荐）
- `inklink-<版本>.zip` —— 解压后右键 `install.cmd` → 以管理员身份运行

要求：Windows 10 1809+ / Windows 11，64 位。装完在「设置 → 时间和语言 → 语言和区域」里能找到 **inklink 输入法**。

卸载：`设置 → 应用 → 已安装的应用 → inklink`，或者再跑一遍 `setup.exe --uninstall`。

## 现在能做什么

- **中文拼音输入**，整句/词组/简拼，词库衍生自 [rime-ice](https://github.com/iDvel/rime-ice)
- **个人词库**：选过的词会记住，按应用分别加权；上屏后马上退格删掉，会当成选错自动撤回
- **整句学习**：一句话里选了两次以上，整句记下来
- **模糊音**：`zh/z`、`n/l`、`r/l`、`r/y`、`f/h`、`eng/ong`、`en/eng`，默认全关，用 `tools\apply-fuzzy.ps1` 开
- **精确优先**：开了模糊音之后，精确命中的候选排前面，模糊命中接在后面（不会出现打 `huang` 第一屏全是 `fang`）
- **候选窗**：最多 9 列 × 9 行，可切换深/浅色
- **浮条**（画在宿主进程里的那一条）：中英切换、标点开关、设置入口，位置可拖，跟着输入法激活显示
- **设置窗**：浮条 ☰ → 设置…

## 已知限制

- **不联网**：没有云输入、没有同步、没有账号。这是设计，不是没做完。
- **Electron 应用（VS Code 等）没有拼音下划线**：它们不支持 TSF 的显示属性，不是本项目的 bug。
- 输入法目前只做简体、只做拼音（`luna_pinyin_simp`）。

## 从源码构建

需要：Rust（stable + `i686-pc-windows-msvc` 目标）、Visual Studio 2022 的 C++ 生成工具、7-Zip。

```powershell
cd C:\dsh\work\inklink

# 1. 取 librime（Weasel 0.17.4，SHA-256 固定）+ 铺词库 + 裁 data
.\tools\fetch-librime.ps1 -Dest C:\inklink-engine

# 2. 编 x86 引擎（链 rime.dll；64 位没有 rime.dll，所以必须分进程）
.\tools\build-engine.ps1 -Dest C:\inklink-engine

# 3. 跑单测 + 构建 64/32 位 DLL + 部署到本机（需要管理员）
.\tools\deploy-local.ps1

# 4. 打发布包（dist\ 下出 zip / setup.exe / SHA256SUMS.txt）
.\tools\release.ps1
```

第一次铺词库时引擎要编译 `.bin`，约 4 秒；之后复用。

## 架构

三个部分，一条原则：**引擎绝不进宿主进程**。

```
inklink_tsf.dll (x64, 进宿主进程)  ──命名管道──▶  rime-server.exe (x86, 链 rime.dll)
        │                                              ▲
        └── 浮条、候选窗、键处理                        └── librime 词库与用户词库
```

分开的原因很实际：Weasel 分发的 `rime.dll` 只有 32 位，而 TSF DLL 必须是 64 位；更早的版本把 `rime.dll` 加载进每个进程，结果崩了资源管理器。细节见 [`docs/architecture.md`](docs/architecture.md) 与 [`docs/adr/`](docs/adr/)。

## 代码签名

本项目使用 [SignPath Foundation](https://signpath.org/) 为开源项目提供的免费代码签名服务
（申请中）。策略、签名范围与构建来源见 [`docs/code-signing-policy.md`](docs/code-signing-policy.md)。

在签名生效之前，发布包没有数字签名（自签名对陌生用户等于没有，所以没做）。
请用同一次发布的 `SHA256SUMS.txt` 校验下载的包：

```powershell
certutil -hashfile inklink-<版本>.zip SHA256
```

## 许可

GPL-3.0。词库衍生自 [rime-ice](https://github.com/iDvel/rime-ice)（GPL-3.0），librime 与默认配置来自 [Weasel](https://github.com/rime/weasel)。

安全相关问题请按 [`SECURITY.md`](SECURITY.md) 的方式私下报告。
