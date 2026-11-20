# 阶段 2 会话创建观察

macOS arm64 Electron 45.0.0 目录包，从设置页在线安装固定 `dsh-plugin-mission-control@0.2.1` 和 `dsh-connect-workbuddy@5.21.2`。Mission Control 使用原包的 `MISSION_CONTROL_URL` 指向独立的本机 HTTP 接收器。[本批回执](result.json) · [安装锁](package-lock.json)

- 显式创建会话后，尚未发送首条消息，原包已经提交与会话 ID 关联的 `IN_PROGRESS` 任务。
- 在隔离的状态目录故意使会话目录事务写入失败，创建请求返回 500。恢复写入后成功创建另一会话，原包请求中仅出现后者的任务。
- 完整重启后，原包没有收到旧会话的新建通知。通过原包支持的事件配置，将开始轮次和用户消息监听改为独立验收事件，专门验证首轮接纳的创建通知。发送首条消息创建另一个会话后，原包建立对应任务。WorkBuddy 正常完成，任务进入 `REVIEW`。两个成功会话各有一个建任务请求。

同一最终构建复测 Notify 的成功、停止和失败通知，以及 Message History 的实时输入、停用隔离和重启续写。两项回归各使用独立状态目录，依赖摘要与此前各自的锁一致。[Notify 回执](notify-regression.json) · [Message History 回执](history-regression.json)

本批业务验收覆盖会话创建通知及写入失败的隔离。尚未验证真实 JARVIS 看板、完整会话句柄和会话处置事件。模型失败状态仍存在原包的[上游合同差异](../mission-control-turns-2026-11-19/README.md)。

daemon 持久化回归覆盖已存在的项目会话投影。项目历史附加时，旧会话即使尚未进入当前写缓存，也不会被识别成新建。共享宿主和插件系统的默认测试通过，6 个需要预置 Mgr 原包的用例本批未运行。
