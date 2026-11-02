# 代码签名策略

inklink 是自由软件（GPL-3.0）。本文说明发布产物的签名状态、哪些文件会被签名、从哪里构建，
以及谁能发布。申请 [SignPath Foundation](https://signpath.org/) 的免费开源签名需要仓库里有这样一份公开说明。

## 现状：还没签

**从 [`Releases`](https://github.com/MeloFang/inklink/releases) 下载的包目前都没有数字签名。**
正在申请 SignPath Foundation 的签名（给开源项目的免费 OV 级签名）。

自签名对陌生用户来说和没签一样（SmartScreen 照样拦），所以没有做。

拿到签名之前，请用同一次发布的 `SHA256SUMS.txt` 核对哈希：

```powershell
certutil -hashfile inklink-<版本>-setup.exe SHA256
```

## 谁签、签什么

签名由 **SignPath Foundation** 作为第三方服务执行（项目本身不持有私钥）。

只签**本仓库构建**的二进制：

| 文件 | 说明 |
|:--|:--|
| `inklink-<版本>-setup.exe` | 单文件安装器 |
| `inklink_tsf.dll` | TSF 输入法组件（x64） |
| `inklink_tsf32.dll` | 同上（x86，供 32 位程序加载） |
| `inklink-settings.exe` | 设置窗 |
| `rime-server.exe` | 承载 librime 的引擎进程 |

**不签**上游产物，原样分发：

| 文件 | 来源 |
|:--|:--|
| `rime.dll` | [Weasel](https://github.com/rime/weasel) 的发行包 |
| `rime/data/**`、`rime/user/**` | 词库与配置（数据文件，不是 PE） |
| `data/upstream/ice/*.dict.yaml` | [rime-ice](https://github.com/iDvel/rime-ice)（GPL-3.0） |

重签上游二进制会掩盖它的来源，也违反上游的分发条款，所以包里会同时存在
"已签名的 inklink 文件"和"未签名的 `rime.dll`"。

## 怎么构建

签名对象必须能从本仓库可复现地构建出来，全部构建都在 GitHub Actions 上完成，
没有本地手工打包的例外：

1. [`.github/workflows/release.yml`](../.github/workflows/release.yml) 在推送 `v*` 标签或手动触发时运行。
2. 先校验 **tag 与 `VERSION` 文件一致**，不一致直接失败（避免"标着新版本、装的却是旧包"）。
3. 取 librime（`tools/fetch-librime.ps1`，Weasel 0.17.4，SHA-256 写死在脚本里并校验）。
4. 编译 x86 引擎（`tools/build-engine.ps1`）与 64/32 位 TSF DLL（`tools/deploy-local.ps1 -NoDeploy`）。
5. 打包（`tools/release.ps1`）生成 zip、单文件安装器与 `SHA256SUMS.txt`。
6. 产物作为构建工件上传，并附到对应的 GitHub Release 上。

拿到签名后，签名请求由该工作流在构建成功后发起（SignPath 的 GitHub Action），
签名后的产物再回到 Release 页面。

## 怎么验证

- 哈希：每个 Release 附带 `SHA256SUMS.txt`，用 `certutil -hashfile <文件> SHA256` 对照。
- 签名（拿到之后）：`signtool verify /pa <文件>` 可以看到签名主体是 SignPath Foundation，
  以及它背书的项目是本仓库。

## 发布权限

- 只有仓库 owner 有推送 tag 的权限，而 tag 是唯一的发布开关。
- 发布者账户需启用双因素认证（GitHub 账户设置里的 2FA）。
- 没有推送权限的贡献者走 Pull Request；CI 在合并前跑格式化与 clippy。

## 报告的漏洞

见 [SECURITY.md](../SECURITY.md)。
