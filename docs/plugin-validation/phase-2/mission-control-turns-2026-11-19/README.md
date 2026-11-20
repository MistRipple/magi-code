# 阶段 2 Mission Control 任务状态

在 macOS arm64 Electron 45.0.0 目录包中，从 Magi 设置页在线安装 `dsh-plugin-mission-control@0.2.1` 和 `dsh-connect-workbuddy@5.21.2`。通过原包的 `MISSION_CONTROL_URL` 配置将 HTTP 请求发往本机独立接收器，观察原包提交的状态。归档、构建及依赖锁见[回执](result.json)与[安装锁](package-lock.json)。

| daemon 已提交结果 | 原包发出的任务状态 | 验收 |
| --- | --- | --- |
| WorkBuddy `deepseek-v4.1-flash` 完成首轮 | 按会话 ID 创建 `IN_PROGRESS` 任务，随后更新为 `REVIEW` | 通过 |
| 第二轮在模型请求在途时停止 | 任务更新为 `ASSIGNED` | 通过 |
| 第三轮的隔离 Provider 返回 401 | daemon 保存 `model_auth_failed`，原包却将任务更新为 `REVIEW` | 未通过，预期是 `BLOCKED` |

固定 DSH SDK 的 `TurnEndReason` 用 `error` 表达失败。原包 `src/index.js` 的 `REASON_STATUS` 接受 `failed`，遇到 `error` 时走默认 `REVIEW`，原包活动记录也明确写出该原因与状态。保留该上游差异及失败回执，Magi 继续按照共享 SDK 的公开失败事件发布。

本批验证的是原包实际发出的 HTTP 请求。真实 JARVIS 看板、会话创建事件、完整工具事件和其余生命周期场景未在本批验收。
