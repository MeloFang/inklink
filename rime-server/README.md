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

## 首选项

`luna_pinyin_simp`（简体）。`rime\data\default.yaml` 的 `schema_list` 已裁到只剩它，
避免每次部署都编译 8 个输入方案。
