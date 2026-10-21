# rime-vault-bridge

把你的 Markdown / Obsidian 仓库变成 Rime 小狼毫输入法的**专属词库**，并预留后续 AI 续写 / 习惯回流能力。

> 对应设计文档：`AI输入法-Obsidian闭环设计.md`（同工作区根目录）Phase 1。

## 已完成：Phase 1 术语词库（当天可用）

1. 改 `config.yaml` 的 `vault_path` 指向你的仓库（任何含 `.md` 的文件夹都行，不强制 Obsidian）。
2. 运行（用本项目 managed venv 的 python）：
   ```bash
   python extractor/scan_vault.py        # 扫描抽词 -> extractor/terms.json（支持增量）
   python extractor/build_phrase.py      # 生成 output/custom_phrase.txt
   ```
3. 把 `output/custom_phrase.txt` 放进 Rime 用户目录（如 `C:/Users/Administrator/AppData/Roaming/Rime/`），
   并在你所用方案的 `.custom.yaml` 里 patch（见设计文档 §5.2），托盘点「重新部署」即生效。

### 提取规则与权重
| 来源 | 权重 |
|------|------|
| 标题 `#` | 10 |
| `[[双链]]` | 8 |
| `#标签` | 6 |
| `**粗体**` | 4 |
| 2~4 字高频片段 | 每出现 1 次 +1 |

阈值在 `config.yaml` 调：`min_weight` / `min_len` / `max_len`。

## 尚未做（设计文档 Phase 2~4）
- RAG 续写伴生程序（需 Dify + Ollama 跑通）
- 模板插入触发串
- 习惯回流写回 Vault

## 目录
```
rime-vault-bridge/
├── config.yaml
├── extractor/
│   ├── scan_vault.py      # 扫 Vault 抽词 + 增量缓存
│   └── build_phrase.py    # 生成 Rime custom_phrase.txt
├── sample_vault/          # 示例仓库（自测用）
└── output/                # 产出 custom_phrase.txt
```
