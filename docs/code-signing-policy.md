# 代码签名策略

inklink 是自由软件（GPL-3.0），Windows 安装包与动态库通过
[SignPath Foundation](https://signpath.org/) 的免费开源项目签名服务签名。

## 谁签、签什么

- **签名主体**：SignPath Foundation（作为开源项目的第三方签名服务）。
- **被签名的产物**：只签本仓库构建出的二进制 ——
  - `inklink-<版本>-setup.exe`（安装器）
  - `inklink_tsf.dll` / `inklink_tsf32.dll`（TSF 输入法组件）
  - `inklink-setup.exe` 之外的工具不签。
- **不签名的产物**：上游二进制一律保持原样、不重签 ——
  - `rime.dll`（来自 [Weasel](https://github.com/rime/weasel) 的发行包）
  - `rime-server.exe` 引用的 librime 数据文件

重签上游二进制会掩盖来源，也违反上游的分发条款，所以包里会同时存在
"已签名的 inklink 文件"和"未签名的 rime.dll"。

## 怎么构建

签名对象必须能从本仓库可复现地构建出来，全部构建都在 GitHub Actions 上完成：

1. `.github/workflows/release.yml` 在推送 `v*` 标签或手动触发时运行。
2. 取 librime（`tools/fetch-librime.ps1`，Weasel 0.17.4，SHA-256 写死在脚本里）。
3. 编译 x86 引擎（`tools/build-engine.ps1`）与 64/32 位 TSF DLL（`tools/deploy-local.ps1 -NoDeploy`）。
4. 打包（`tools/release.ps1`）生成 zip、单文件安装器与 `SHA256SUMS.txt`。
5. 产物作为构建工件上传，并附到对应的 GitHub Release 上。

签名请求由该 workflow 在构建成功后发起（SignPath GitHub Action），
签名后的产物再回到 Release 页面。

## 怎么验证

每个 Release 都附带 `SHA256SUMS.txt`：

```
certutil -hashfile inklink-<版本>.zip SHA256
```

签名后的 exe/dll 可以用 Windows 自带的 `signtool verify /pa <文件>` 查看签名主体。

## 报告的漏洞

见 [SECURITY.md](../SECURITY.md)。
