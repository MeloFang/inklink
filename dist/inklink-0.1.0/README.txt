inklink 输入法 0.1.0  测试版
============================

一个开源的 Windows 中文输入法。注意：这是早期测试版，欢迎反馈问题。


安装
----

1. 解压到一个目录（不要在压缩包里直接运行）
2. 右键 install.cmd -> 以管理员身份运行
3. 看到「安装完成」后，按 Win+Space 切换到「inklink 输入法」
4. 如果输入法列表里还没有，重启一次电脑


怎么打字
--------

  拼音输入        xiangmu          -> 项目
  长句输入        wolaishuoshuo    -> 我来说说
  分段上屏        pikeqiu          选「皮」之后剩下的 keqiu 留在候选窗里，
                                   可以接着选「克」「球」；多打的字母自动丢掉
  空格            上屏当前高亮的候选
  数字键 1-5      选「当前那一行」的第 N 个候选
  ↓ 或 =          候选窗展开成 5x5
  ↑ ↓             展开后换行
  ← →            展开后行内左右选；收起时移动拼音光标
  - =  / PgUp PgDn  展开后翻页
  Esc             取消这次输入
  回车           拼音原样上屏（当英文）
  Shift/CapsLock  拼音原样上屏（当英文）
  \               顿号「、」
  , . ; /         自动转成中文标点「，。；／」
  -  (拼写中)     拼音分词符，xiang-mu 会显示成 xiang-mu 并正常出候选
  按住 Shift      临时打英文（标点也保持英文）

  点鼠标到别处 = 取消当前输入（拼音不残留）


注意
----

- 刚装完第一次打字可能没反应，等 2-3 秒再打一次（引擎在加载词库）
- 已经在运行的软件需要重启一次才能用上（DLL 已被加载，不支持热替换）
- 词库约 54 万词，自带语言模型；不联网，不收集任何输入内容


卸载
----

右键 uninstall.cmd -> 以管理员身份运行。


文件说明
--------

  C:\Program Files\inklink\
      inklink_tsf.dll      64 位输入法组件（记事本/VSCode/浏览器等）
      inklink_tsf32.dll    32 位输入法组件（WPS 等 32 位软件）

  C:\inklink-engine\
      rime-server.exe      引擎（用 librime）
      rime.dll             librime 本体
      rime\data\           词库与配置
      rime\user\           用户词库与构建产物
      rime-server.log      引擎日志（出问题时看这个）


遇到问题
--------

安装/卸载出问题
  日志文件：C:\Users\Public\inklink-install.log
            C:\Users\Public\inklink-uninstall.log
  直接把这个文件发给开发者，里面记录了系统版本、每一步做了什么、卡在哪。

  如果双击 install.cmd 完全没反应：
    a) 先确认是从解压后的文件夹里运行的，不是压缩包的预览窗口里
    b) 右键 install.cmd -> 属性 -> 如果底部有「解除锁定」，勾上确定后再试
    c) 右键 install.cmd -> 以管理员身份运行（不要直接双击）
    d) 都不行就手动开一个管理员 PowerShell，执行：
         cd /d 你解压的目录
         powershell -ExecutionPolicy Bypass -File install.ps1

打字出问题
  1. 引擎日志：C:\inklink-engine\rime-server.log
  2. 输入法没反应：确认 rime-server.exe 在任务管理器里；没有就双击
     C:\inklink-engine\rime-server.exe
  3. 某个软件里用不了：告诉我软件名和它的位数（64 位一般没问题）


许可证
------

GPL-3.0。词库衍生自 rime-ice（GPL-3.0），引擎为 librime（BSD-3-Clause）。

更新到新版本
------------

如果你已经装过旧版，更新只需要：

1. 关掉所有正在运行的软件（或者直接重启电脑）
2. 右键 install.cmd -> 以管理员身份运行
3. 完成

安装脚本会自动处理"旧 DLL 被占用"的情况（改用带时间戳的新文件名），
不需要先卸载。更新后同样需要重启正在用的软件。