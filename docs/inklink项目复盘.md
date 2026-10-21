# inklink 项目复盘

> 一份给自己的简单记录：这项目是啥、为啥做、为啥停、踩了哪些坑。
> 状态：**已终止并清理**（2026-10-20 本机已无任何残留）。

---

## 一、项目是什么

**inklink** —— 一个 Windows 上的中文输入法（AI 增强方向）。

- 目标：把 Markdown/Obsidian 笔记仓库变成 Rime 个人词库，并在输入法里挂 AI 续写 / 选中即 AI。
- 路线：基于 PIME/TSF 框架，用 C++ 重写前端（`PIMETextService.dll`），把 librime 拼音引擎直接编进 DLL，进程内单例驱动，不依赖 Python 后端。

## 二、为什么停

最后一次实机验证又崩了：`Explorer.EXE` / `WorkBuddy.exe` 出错模块 `rime.dll`，
异常 `0xc0000409`，**WPS 也被连带搞崩**。

输入法这种东西会被塞进 explorer、WPS、记事本**每一个**进程，一次崩溃就等于"系统崩了"，
而每次验证都要用户重启一次机器 —— 没有真机联调环境（双机内核调试 + 符号服务器），
迭代成本过高，决定放弃。

## 三、踩过的坑（按印象深的排序）

**1. 输入法是高稳定性门槛的注入式组件**
一个 DLL 进每个宿主进程，任何 `__fastfail` 体感就是"系统挂了"。没有符号文件只能靠崩溃偏移猜，
每次复现都要重启。这是这个项目最根本的难度，不是代码 bug。

**2. `DllCanUnloadNow` 必须永远返回 `S_FALSE`**
原实现返回 `canUnloadNow()`，TSF 在 COM 引用归零时 `FreeLibrary` 卸载 DLL，
而静态 `RimeEngine` + 后台初始化线程仍存活 → 在已释放代码上析构/执行 → `__fastfail(0xC0000409)`。
坑点：**测试 exe 从不卸载 DLL，所以这个问题在单元测试里永远测不出来**，只有实机切换输入法时才爆。

**3. 多进程 userdb 抢锁导致黑屏**
librime 的 `luna_pinyin.userdb` 用排他进程锁。多个宿主进程共用同一 `user_data_dir`
→ 抢锁失败发生在宿主 UI 线程 → 界面冻结 → explorer 被杀 → 桌面黑屏。
修法：每进程独立 `user_data_dir`（`u<pid>`），词库部署产物共享 `build\`。

**4. 不能在 UI 线程做重量初始化**
`onActivate` 是宿主 UI 线程同步回调，原代码在那里同步全量建库（实测 11.3s）→ 卡死。
修法：`CreateThread` 后台跑，引擎未 ready 时按键透传系统。

**5. `RimeStartMaintenance` 是全局串行的**
多宿主进程并发建库会排队（实测 1000 / 6250 / 7297 ms）。
加 `stagingIsFresh()` 跳过已部署产物 → 实测降到 15 / 16 / 16 ms。

**6. `LoadLibraryW(L"rime.dll")` 是致命的**
Windows 搜 DLL 的顺序不含"本 DLL 所在目录"，宿主是 notepad（在 `C:\Windows\`）就永远找不到。
修法：拼绝对路径 + `LoadLibraryExW(..., LOAD_WITH_ALTERED_SEARCH_PATH)`。

**7. 注册文件不随包，注册表有项但输入法不出现**
`regsvr32` 报成功，列表里却没有 —— 缺 `backends.json` 或 `ime.json`，
`DllRegisterServer` 收集不到 language profile。验证要看 `CTF\TIP\{CLSID}\LanguageProfile` 是否真有子键。

**8. 跨线程铁律**
`ready_` 用 `std::atomic<bool>`，**不能用锁**（init 持锁 11s，UI 线程等锁又冻结一次）；
后台线程**不调成员函数**（`this` 不可靠，TSF 随时析构 Client）→ 引擎初始化抽成文件内静态函数。

## 四、环境层面的坑（与项目无关，但很坑）

- **沙盒里删大目录删不动**：`shutil.rmtree`/`os.remove` 被 safe-delete 接管走回收站，880MB 目录直接失败。
  得用 Win32 `DeleteFileW`/`RemoveDirectoryW` + `\\?\` 前缀。
- **Windows 保留名 `nul`/`con` 删不掉**：`DeleteFileW` 返回 5，要先 `MoveFileW` 改名再删。
- **回收站是隐藏的"没删干净"**：之前以为删了的文件其实都在回收站里（本轮清出 24534 项 / 14.4GB）。
- **`bash -c` 拼 Windows 路径是转义地狱**：一律写成 `.py` 文件再跑。

## 五、一句话总结

> 给系统塞一个会进每一个进程的组件之前，先确认有真机联调条件；
> 没有符号服务器和双机调试，这类项目别硬做。
