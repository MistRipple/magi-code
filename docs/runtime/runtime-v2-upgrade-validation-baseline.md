# Magi Terminal Runtime V2 升级与验证基线

> 版本: 1.0  
> 更新时间: 2026-02-06  
> 适用范围: Orchestrator + Worker(Claude/Codex/Gemini) 终端能力  
> 文档性质: 长期演进基线（唯一参考）

## 1. 文档定位

本文档定义 Magi 终端能力的长期目标架构、升级路径与验证标准，用于替代“单次命令执行器”模式，建设可媲美 Augment 的终端运行时系统。

本文档是后续终端相关开发与回归验证的唯一基线，任何设计或实现变更均需与本文档对齐。

## 2. 目标与非目标

### 2.1 目标

1. 支持四角色并发使用终端，且会话、输出、状态严格隔离。
2. 统一终端能力接口为进程生命周期模型，而非一次性执行模型。
3. 提供可观测、可审计、可回放的终端事件流。
4. 提供可量化门禁指标，保证长期迭代稳定性。

### 2.2 非目标

1. 不做“临时判空兜底”式补丁修复。
2. 不保留同一能力的多套并行实现。
3. 不通过提示词替代运行时能力缺陷。

## 3. 工程硬约束

1. 单一实现: 终端执行内核必须统一为 `TerminalRuntimeV2`。
2. 禁止回退分叉: 不允许“旧路径继续可用 + 新路径试运行”长期并存。
3. 禁止补丁式修复: 问题必须在状态机、协议或调度层根因修复。
4. 角色隔离强制: 不允许跨角色共享同一 stdin 会话。
5. 协议先行: 所有终端行为必须可映射到结构化事件。

## 4. 五步诊断结论（当前差距）

### 4.1 [表象分析]

当前终端可用但在复杂场景下表现不稳定:

1. 长命令与并发任务下完成判定不稳定。
2. 输出边界偶发混杂，退出码语义不完整。
3. 多角色并发时缺少标准会话隔离与复用调度。

### 4.2 [机理溯源]

高质量终端体验依赖三件事:

1. 进程生命周期工具契约（launch/read/write/kill/list）。
2. 统一执行状态机与输出采集协议。
3. 可持续会话管理（会话复用、占用控制、cwd 一致性）。

### 4.3 [差距诊断]

与目标相比，当前主要差距:

1. 工具协议过薄（偏一次性执行）。
2. 完成判定与输出采集链路不够统一。
3. 角色并发调度与会话隔离模型未固化为协议。

### 4.4 [根本原因分析]

根因是终端能力抽象层级不足: 将终端当“函数调用”，而非“有状态运行时资源”。

### 4.5 [彻底修复与债清偿]

必须从架构层升级为“终端运行时系统”，并清理旧接口与分叉路径。

## 5. 目标架构（Terminal Runtime V2）

### 5.1 核心组件

1. `TerminalRuntimeV2`: 唯一终端执行内核。
2. `RoleScopedSessionPool`: 按角色管理长会话池。
3. `ExecutionStateMachine`: 统一命令状态流转。
4. `OutputCapturePipeline`: 统一输出边界、退出码、cwd 采集。
5. `TerminalPolicyEngine`: 角色级权限与并发额度控制。
6. `TerminalEventBus`: 结构化事件总线（供 UI/日志/指标消费）。
7. `TerminalToolFacade`: 对 LLM 暴露统一工具协议。

### 5.2 架构关系

```text docs/runtime/runtime-v2-upgrade-validation-baseline.md
User/LLM Role
    |
    v
TerminalToolFacade (launch/read/write/kill/list)
    |
    v
TerminalRuntimeV2
    |-- RoleScopedSessionPool
    |-- ExecutionStateMachine
    |-- OutputCapturePipeline
    |-- TerminalPolicyEngine
    |
    v
TerminalEventBus ---> UI Console View
                 ---> Metrics/SLO
                 ---> Audit Log
```

## 6. 统一会话模型

### 6.1 会话键

`sessionKey = missionId + role + workerId + laneId`

说明:

1. `role`: `orchestrator | claude | codex | gemini`
2. `workerId`: 同角色多实例时区分实例
3. `laneId`: 同角色并发任务分道

### 6.2 会话隔离规则

1. 同 `sessionKey` 可复用会话。
2. 不同 `role` 不可复用同一 stdin 会话。
3. 任何跨角色协作必须通过结构化事件传递，不可共享终端输入流。

## 7. 统一工具协议

### 7.1 `launch-process`

输入:

1. `command`
2. `cwd`
3. `wait`
4. `max_wait_seconds`
5. `sessionKey`

输出:

1. `terminal_id`
2. `status`
3. `output`
4. `return_code`

### 7.2 `read-process`

输入:

1. `terminal_id`
2. `wait`
3. `max_wait_seconds`

输出:

1. `status`
2. `output`
3. `return_code`
4. `cwd`

### 7.3 `write-process`

输入:

1. `terminal_id`
2. `input_text`

输出:

1. `accepted`
2. `status`

### 7.4 `kill-process`

输入:

1. `terminal_id`

输出:

1. `killed`
2. `final_output`
3. `return_code`

### 7.5 `list-processes`

输出:

1. `terminal_id`
2. `sessionKey`
3. `status`
4. `command`
5. `started_at`

## 8. 统一状态机

### 8.1 状态定义

1. `queued`
2. `starting`
3. `running`
4. `completed`
5. `failed`
6. `killed`
7. `timeout`

### 8.2 强约束

1. 单命令只允许一次终态转换。
2. 终态必须带 `return_code`（若未知则显式 `null`，禁止默认为 `0`）。
3. `running -> timeout` 不代表成功，必须保留可读输出与可继续读取能力。

## 9. 输出采集与完成判定

### 9.1 分层采集策略（同一实现内）

1. Shell Integration 流式读取。
2. Script Capture + ANSI 标记边界。
3. 终端命令复制链路（用于 API 不可用场景）。

说明: 三层策略均受同一状态机驱动，不允许分叉为独立实现。

### 9.2 输出规范

每次读取必须产生:

1. `stdout`
2. `stderr`（若不可区分需明示合并来源）
3. `return_code`
4. `cwd`
5. `is_complete`

## 10. 四角色并发调度策略

### 10.1 调度原则

1. 角色级配额: 每角色可配置并发上限。
2. 会话级串行: 同一 `sessionKey` 同时只允许一条前台命令。
3. 全局保护: 防止总并发超过扩展可承载阈值。

### 10.2 角色权限建议

1. `orchestrator`: 诊断、查询、轻量命令优先。
2. `worker`: 完整执行权限（受 allowlist 与工作目录策略约束）。

## 11. 观测与审计

### 11.1 事件模型

统一事件字段:

1. `eventId`
2. `missionId`
3. `sessionKey`
4. `role`
5. `terminal_id`
6. `command_id`
7. `state`
8. `timestamp`
9. `cwd`
10. `return_code`
11. `output_size`

### 11.2 指标（SLO）

1. 完成判定准确率 >= 99%
2. 退出码一致率 >= 99%
3. 输出完整率 >= 98%
4. 跨角色串扰率 = 0
5. 多角色并发场景回归通过率 >= 95%

## 12. 升级路线（建议 4 阶段）

### 阶段 1: 协议落地

1. 实现 `launch/read/kill/list`（可先不开放 `write`）。
2. 引入 `sessionKey` 与角色隔离。
3. 主链路改用新协议。

完成定义:

1. 主编排链路不再直接依赖旧 `execute_shell` 语义。

### 阶段 2: 状态机与采集统一

1. 统一完成判定与退出码。
2. 统一输出边界提取与结构化事件。
3. 清理旧状态字段和歧义分支。

完成定义:

1. 所有读取接口返回统一结果结构。

### 阶段 3: 调度与并发强化

1. 引入角色并发额度控制。
2. 引入会话占用冲突处理。
3. 支持长会话安全复用。

完成定义:

1. 四角色并发下无串流污染。

### 阶段 4: 门禁与回归体系

1. 增加 CI 门禁: 禁止旧接口回流主链路。
2. 建立真实多角色终端回归脚本。
3. 固化 SLO 报告输出。

完成定义:

1. 门禁阻断率与回归稳定性达到目标阈值。

## 13. 验证矩阵（回归最小集合）

### 13.1 功能用例

1. 单角色短命令（成功/失败/超时）。
2. 单角色长命令 + 中途读取 + 中断。
3. 四角色并发启动与读取。
4. 同角色多 lane 并发与隔离。
5. 会话复用后 cwd 一致性。

### 13.2 可靠性用例

1. Shell Integration 不可用场景。
2. Script Capture 采集异常场景。
3. 终端关闭/重开后的会话恢复场景。

### 13.3 审计用例

1. 每次命令必须有完整状态事件链。
2. 失败命令必须保留完整 return_code 与输出片段。

## 14. CI 门禁规则（必须启用）

1. 禁止主编排链路重新引入旧终端调用入口。
2. 禁止出现同语义双工具并存（旧/新协议同时作为主路径）。
3. 禁止状态机绕过写入（直接写终态字段）。
4. 禁止返回结果隐式“成功化”（如未知 return_code 强置 0）。

## 15. 交付与验收标准

### 15.1 代码验收

1. 新协议工具链路完整可调用。
2. 旧路径清理完成，无主链路残留引用。
3. 状态机与事件模型统一。

### 15.2 质量验收

1. 回归矩阵全通过。
2. SLO 指标满足阈值。
3. 关键日志字段完整可追踪。

## 16. 执行约定

1. 所有终端相关需求先更新本文档，再进入实现。
2. 每次迭代需附带“本次变更对应本文档章节”的映射记录。
3. 若实现与文档冲突，以文档更新评审为准，不允许无文档偏航。
