# rime-vault-bridge · 项目交接说明（HANDOFF）

> 用途：把这个项目从临时对话工作区迁移到 WorkBuddy「项目」后，新会话 @ 本文件即可无缝接上。
> 最后更新：2026-10-13
> 当前状态：Phase 1 已落地自测；产品化（开源 + exe + 5 项新需求）已评估、待开发。

---

## 1. 这是什么

把你的 **Markdown / Obsidian 仓库**变成 Rime 小狼毫输入法的**专属词库**，并预留后续能力：AI 续写、选中即 AI 操作、云端 API 兜底、开源打包成 exe 安装程序。

- 设计稿：`AI输入法-Obsidian闭环设计.md`（同工作区根目录，结尾已标注 Phase 1 落地状态）
- 手册落册：已进《自建AI平台部署手册（合订本）》§10.11（续48 / 续49）

---

## 2. 当前进度（已完成）

**Phase 1（Vault 术语 → Rime 词库）已落地并自测通过：**

| 文件 | 作用 |
|------|------|
| `config.yaml` | vault 路径、输出路径、阈值（min_weight / min_len / max_len） |
| `extractor/scan_vault.py` | 扫 `.md` 抽词：标题10 / 双链8 / 标签6 / 粗体4 / 2~4 字 n-gram 每现+1；按 mtime+size **增量缓存**，只重算变动文件 |
| `extractor/build_phrase.py` | pypinyin 转拼音 + **子串抑制**（去「光纤入/纤入」等长词碎片）+ 停用词 → 输出 Rime `custom_phrase.txt`（格式 `短语<TAB>拼音<TAB>权重`） |
| `sample_vault/` | 3 篇示例笔记（自测用，可删） |
| `output/custom_phrase.txt` | 产出（示例跑通 → 13 条干净术语） |
| `README.md` | 使用说明 |

**自测结果**：示例 vault 抽 367 候选 → 子串抑制后写出 13 条真实术语（光纤入户 / 嘉兴分公司 / 周报模板 / 运维平台 / 试点 / 项目进度…），拼音无误。

---

## 3. 本机环境事实（2026-10-13 探测）

| 项 | 状态 | 备注 |
|----|------|------|
| Rime 小狼毫 | **未安装** | 需装才能把词库部署进输入法 |
| Obsidian | 本机无集中仓库 | 工具**不依赖 Obsidian 运行**，扫任意 `.md` 文件夹；用户本机仍可直接指向其 Obsidian 仓库 |
| Ollama | **在跑**（127.0.0.1:11434） | 有 internvl 等模型；qwen3:14b 待确认在场 |
| Dify | **未起**（80 端口拒连） | Phase 2 RAG 续写需先 `docker compose up` |
| Python | managed venv | `C:/Users/Administrator/.workbuddy/binaries/python/envs/default`，已装 pypinyin 0.55.0 / pyyaml 6.0.3 |

---

## 4. 用户提出的 5 项产品化需求 + 可行性结论

| # | 需求 | 结论 | 落地要点 |
|---|------|------|----------|
| 1 | 开源 + exe 安装包 | ✅ 高 | PyInstaller 打包 + NSIS/Inno 安装包；GitHub 走 SSH443；许可证倾向 **GPLv3**（兼容 Rime）；需新增 GUI 层 |
| 2 | 去本机 Obsidian 依赖 | ✅ 已解耦 | extractor 扫任意 `.md` 文件夹；开源只需 `vault_path` 做成用户设置、不写死个人路径；**用户本机仍可用 Obsidian**（指向其仓库即可） |
| 3 | 支持云端 API | ✅ 高 | 抽 `AIProvider` 抽象：`LocalOllama`（127.0.0.1:11434）+ `ApiProvider`（OpenAI 兼容，填 `key+base_url+model`）；key 用设置 UI / keyring 存，别明文落盘 |
| 4 | 输入后自动推测后续文字 | 🟡 中高 | 两路：① 离线候选补全（词库/n-gram，不需模型）② AI 续写（已输入上下文→后端→续写候选/浮窗）；**难点是跨应用捕获已输入文本**，建议先浅集成（全局热键+剪贴板/浮窗） |
| 5 | 选中文本→AI 操作 | ✅ 高 | 全局热键/右键捕获选中文本（剪贴板）→ 发后端执行 搜索/RAG问答/总结/翻译/解释 → 浮窗展示；搜索可接 **Tavily** 联网或 vault RAG |

---

## 5. 建议架构（三层）

- **core**：extractor（已有） + provider 抽象（本地/API） + config（用户设置） → 对应需求 2、3
- **gui**（开源必须新增）：系统托盘 + 浮窗 + 全局热键 + 设置页 → 对应需求 4、5 的"捕获"和 1 的产品化
- **dist**：PyInstaller 打包 + 安装包 + 许可证/README → 对应需求 1

---

## 6. 建议推进顺序

1. **2 + 3**：解耦 + 后端抽象（纯逻辑、零 GUI 风险）
2. **5**：选中即 AI（浅集成即可用）
3. **4**：续写（浅集成）
4. **1**：打包开源（放最后，依赖前面稳定）

---

## 7. 待用户拍板的技术栈（尚未确认）

- GUI：**PySide6**（提议，LGPL、Python 原生、打包稳）
- 打包：**PyInstaller**（提议，单文件夹模式，避免 one-file 启动慢）
- 许可证：**GPLv3**（提议，与 Rime/小狼毫 GPL 兼容）

---

## 8. 怎么跑（Phase 1 现状）

```bash
cd rime-vault-bridge
python extractor/scan_vault.py        # 读 config.yaml 的 vault_path
python extractor/build_phrase.py      # 生成 output/custom_phrase.txt
```

部署到 Rime：把 `output/custom_phrase.txt` 放进 Rime 用户目录（如 `C:/Users/Administrator/AppData/Roaming/Rime/`），并在所用方案的 `.custom.yaml` 里 patch（设计稿 §5.2），托盘点「重新部署」即生效。

> 注：脚本路径都相对 `config.yaml` 所在目录，整个文件夹整体移动不影响内部相对结构；但若改 `vault_path` 为相对路径，基准是 config.yaml 所在目录。

---

## 9. 立刻能接手的下一步（请先和用户确认方向）

- **A**：先写 core 的「后端抽象 + 去 Obsidian 硬编码」——把 `vault_path` 从 config 改为用户设置入口、抽 `AIProvider`（LocalOllama / ApiProvider）接口。
- **B**：先把设计稿扩成「产品版」（含 5 点需求 + 三层架构 + 技术栈），再落册。
- 用户尚未二选一；新会话请先问用户选 **A** 还是 **B**，或先确认第 7 节技术栈后再动手。
