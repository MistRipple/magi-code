# 阶段 2 会话权威清理

使用 macOS arm64 Electron 45.0.0 目录包。新状态目录首次从本地安装专项测试包。修复前，确认安装可能在加载协调器接纳首个配置前返回 409。修复后，安装沿同一来源监视器等待首轮检查结束，实际接纳测试包并发布其命令。测试包用于核对原生调用边界。[命令回执](command-result.json) · [安装锁](command-package-lock.json)

- 在真实会话发起插件命令，daemon 将结果写入原生 Turn。命令拿到同一会话的只读身份。尝试用该身份递归执行另一条命令时，宿主拒绝调用。观察端没有收到宿主生成的本地命令事件。
- 在途命令收到主动停止信号，原生轮次结算为取消。停用插件时，在途命令结算为失败。两种情况下插件返回的迟到成功均未写入轮次。再次提交已停用插件的命令返回 403。

同一最终构建用固定原包复测了 Mission Control 0.2.1 的会话新建、重启和完成状态，以及 Message History 1.0.6 的实时输入、停用隔离和重启续写。两者的归档和安装锁分别与[会话创建批次](../session-created-2026-11-19/README.md)及[实时历史批次](../live-input-2026-11-19/README.md)一致。[Mission Control 回执](mission-regression.json) · [Message History 回执](history-regression.json)

同一构建复测 Notify 0.4.9 的完成、失败和取消通知，安装锁与[轮次通知批次](../notify-turns-2026-11-19/README.md)一致。[Notify 回执](notify-regression.json)

本批完成共享宿主测试、插件 Rust 回归和 daemon Clippy 检查。正式 Agent／Session 服务、带持久事实的 DSH 命令观察事件、继续问答及 Better Sidebar 右栏业务仍待交付。
