# 安全策略

## 报告问题

如果你发现安全问题（例如输入法把内容发到了不该去的地方、安装器提权被利用），
请**不要**开公开 issue，直接发邮件到仓库 owner 的邮箱（见 GitHub 主页），
或在 GitHub 上使用「Report a vulnerability」（Security → Advisories）。

## 我们关心什么

- **本地性**：inklink 不联网、不上传击键与候选。词库与配置都在本机。
- **最小权限**：只有安装器需要管理员；输入法组件与引擎以当前用户权限运行。
- **可追溯**：每个发布包附带 SHA-256；构建全部在 GitHub Actions 上完成。

## 不在范围内

- 上游组件自身的漏洞（`rime.dll` 来自 Weasel/librime），请报给上游。
