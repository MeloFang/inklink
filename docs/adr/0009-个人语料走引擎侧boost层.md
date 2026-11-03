# ADR-0009：个人语料走引擎侧 boost 层，不改 Rime 方案

- 状态：已接受（实现仍待做，见 `PLAN.md`「M2 切片 1」）
- 日期：2026-11-03
- 相关：[ADR-0003 基础词库选型](0003-基础词库选型.md)、[ADR-0004 词库改用 rime-ice](0004-词库改用rime-ice并采用GPL.md)

## 背景

M2 要把笔记里的术语（`extractor/` 的 v1 管线能抽出来）变成候选，并且**排在前面**：
验收是「笔记里写了『项目代号赤兔』之后，打 `chitu` 第一条就是赤兔」。

摆在面前的有三条路，都实际试过（2026-11-03，本机 librime = Weasel 0.17.4）：

### A. 合并词库：`import_tables: [base, corpus]`

新建 `user/inklink.dict.yaml` 导入 `base` 与 `corpus`，patch `translator/dictionary: inklink`。
**能跑**：`inklink.table.bin` 正常编译（17.5 MB），`码上飞`/`赤兔` 都进了候选。
但**排不到第一**：把词条权重从 1e5 提到 1e9，`mashangfei` 仍然是「马上飞、马上、码上飞」，
`chitu` 仍然是「吃土、赤兔」——排序由 librime 的整句/语言模型主导，词条权重不是决定项。
另外每次改语料都要重编 17 MB 的表（20 秒级），首次装机的代价也跟着涨。

### B. Rime 的 `custom_phrase` 翻译器

官方给「自造短语」留的口子（`table_translator@custom_phrase` + `stabledb` + `custom_phrase.txt`）。
试了两轮都没让 `corpus.txt` 真正加载：写 `dictionary: corpus` 会去打开不存在的
`corpus.table.bin`；改成 `dictionary: ""` + `user_dict: corpus` 后错误没了，但候选里
一条也没有。`custom_phrase` 的取值语义（`dictionary` / `user_dict` / `db_class` 三者谁决定
读哪个文件）没有可靠文档，纯靠猜，风险高。

### C. 引擎侧 boost 层（本 ADR 的选择）

引擎已经有「个人词库」那一套：`query()` 返回前 `apply_user_boost()` 按 `total + apps[app]*2`
排序，并且**学过的词若 librime 没给，会补到最前（最多 3 个）**。语料要的效果和它一模一样，
只是数据来源不同。

## 决定

1. 语料单独存 `%LOCALAPPDATA%\inklink\corpus.tsv`（`码\t词\t权重`，UTF-8），**不与
   `user-words.tsv` 混**：语料可整批重建/删除，学习记录不能；语料不该被"误选撤回"扣减，
   也不该被 4000 条学习上限挤掉。
2. 引擎启动时把它读进独立的 `g_corpus`；`query()` 在用户 boost 之后再加一层：
   命中的语料按权重排到最前，librime 没给的语料直接补进去（沿用现有"最多补 3 条"的护栏）。
3. **不动 Rime 方案、不写 custom.yaml、不重编词库表**：和模糊音、候选窗配置完全解耦，
   导完语料重启引擎即可生效。
4. 拼音由 `tools/import-corpus` 离线算：用 `rime\data\8105.dict.yaml`（字 → 拼音，按权重
   取常用读音）拼出词的音节，**不引入 pypinyin 之类的新依赖**，也不需要在引擎里做
   text→pinyin 转换（这版 librime 没有 `RimeGetPinyin`）。

## 后果

- 引擎要加一小段 C++（读 tsv + 一层排序/补词），需要 `tools\build-engine.ps1` 重编一次。
- 语料权重是"我们自己的排序"，与 librime 的语言模型是两个来源；两者冲突时以语料为准
  （这正是"只学我的词"的产品主张）。上限设 2000 条，超了按权重保留。
- 不产生 Rime 侧状态：卸载/换机器时语料只需拷一个 tsv。
- 代价：候选的第一条可能是"语料词"而不是"最通顺的句子"。如果实际用起来觉得太霸道，
  退路是把语料权重降级为"参与排序但不强制第一"——改的是同一个函数。

## 未采纳

- **A（合并词库）**：排序不可控，且每次改语料都要重编大表。
- **B（custom_phrase）**：加载语义不透明，试了两轮没跑通，且仍然依赖 patch 同一个
  `custom.yaml`——会和模糊音生成器抢文件。
