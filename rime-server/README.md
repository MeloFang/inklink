# rime-server

用 **[librime](https://github.com/rime/librime)** 做引擎，通过命名管道给 TSF DLL 供候选。

## 为什么单独一个进程

`rime.dll`（Weasel 分发的版本）是 **32 位**的，而我们的 TSF DLL 是 64 位。
进程间 32/64 位互不影响，所以让 x86 的 `rime-server.exe` 承载 librime，
x64 的 `inklink_tsf.dll` 通过管道查询——这也正是 Weasel 自己的架构。

## 协议（与原来的 Rust 引擎完全一致）

```
请求: [u32 len LE][UTF-8 拼音]
响应: [u32 count LE] + count * ([u16 len LE][UTF-8 候选词])
```

## 依赖文件（不进 git）

以下文件从 **Weasel 安装包**里解出来（`weasel-<ver>-installer.exe`，用 7-Zip 解压）：

```
rime.dll          3.0 MB   librime 本体（x86）
rime\data\       13 MB     词库与配置（default.yaml / luna_pinyin.* 等）
rime\user\       ~44 MB    librime 运行时生成的构建产物和用户词库
```

`rime\user\` 会在首次运行时自动构建（`rime\data` 里的词典编译成 `.bin`），
耗时约 1-2 分钟；之后复用。

## 构建

```powershell
# 必须用 x86 工具链（因为 rime.dll 是 32 位）
& "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvarsall.bat" x86
cl /nologo /utf-8 /EHsc /std:c++17 /O2 /MD rime-server.cc advapi32.lib /Fe:rime-server.exe
```

## 部署

```
C:\inklink-engine\
  rime-server.exe     ← 本程序
  rime.dll
  rime\data\
  rime\user\
```

注册表 `HKLM\SOFTWARE\inklink` 的 `EnginePath` 指向 `rime-server.exe`，
DLL 在引擎不在时会按这个路径自动拉起。

## 实现要点

- **每连接一个线程**：一个卡住的客户端不会堵死其它应用；librime 会话用互斥量串行化。
- **写完必须 `FlushFileBuffers`**：否则 `DisconnectNamedPipe` 会把客户端还没读的尾部数据丢掉。
- **NULL DACL**：输入法会被不同权限的应用调用，管道必须允许任何人连。
- **候选靠 `RimeGetContext` + `{Page_Down}` 翻页收集**（`RimeCandidateList*` 在本版本返回空）。
- **英文候选插在哪由 `is_pinyin_code()` 决定**：像拼音就把原始输入排最后（`zheg` → 这个 排第一），
  不像拼音才排在纠错结果后面（`github` → github 排第一）。判定分三条：
  1. 整串能完整切成合法音节（`xiangmu`、`nice`）；
  2. 全是声母（`zg`、`npm`，简拼）；
  3. 长度 ≥3 且**允许一个音节只打一半**（`zheg` = zhe + g、`zhge` = zh + ge、`xiangm` = xiang + m）。
     有了第 3 条，打错/漏字母不会再被当成英文；`hello`/`test`/`world`/`code` 这类仍然是 0 或 2+ 个半音节，不受影响。
- **纠错探针**（`query()` 里，2026-10-27）：多打/错打/换位一个字母时，在引擎侧生成变体再查一遍，
  一次管道往返搞定，DLL 完全不用改。三层筛子保证正常输入零开销：
  1. `is_full_pinyin(code)` 为真（`xiangmu`）→ 不探；
  2. `ends_with_half_syllable(code)` 为真（`zheg`/`xiangm`，末尾音节打一半）→ 不探，
     这类 librime 的前缀匹配处理得更好，插手会把「项目」换成「向」；
  3. 其余（`niihao`/`zhemge`/`shanghhai`/`nihoa`）→ `edit_variants()` 生成变体：
     ① 删相邻重复字母 ② 相邻换位 ③ 删任意一个字母，每个变体必须先 `is_full_pinyin()` 通过
     才去查 rime（所以 `github`/`vscode` 一个变体都产不出来）。纠错结果排最前。
- **`variant_score()`**：给合法变体排优先级的启发式，惩罚"需要单字母音节"的切分——
  `nihoa` 的两个合法变体里 `nihao`（ni+hao，0 个单字母）排在 `nioha`（ni+o+ha，1 个）前面。
  只按生成顺序会先出「你哦哈」这种垃圾。

## 首选项

`luna_pinyin_simp`（简体）。`rime\data\default.yaml` 的 `schema_list` 已裁到只剩它，
避免每次部署都编译 8 个输入方案。
