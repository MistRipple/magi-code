# 对话性能验证

性能验证只在处理性能问题或明确比较性能收益时执行。日常修改按受影响模块运行定向回归，完整发布门禁见[发布流程](release-process.md)，保留的集成脚本见[验证入口](validation.md)。

## 测量边界

| 阶段 | 观察范围 |
| --- | --- |
| 接纳 | submit_received → accepted_response_sent |
| 本地准备 | accepted_response_sent → provider_request_started |
| Provider | provider_request_started → provider_first_raw_delta；首个可见文本单独记录 |
| 后端完成 | accepted_response_sent → canonical_terminal_published |
| Renderer | frontend_event_received → reducer_completed → projection_completed → dom_painted |

后端与 Renderer 使用各自时钟，不拼接为总延迟。标题、压缩等辅助模型调用与主 Turn 分开；失败、取消、阻塞和恢复单列，不混入正常成功统计。

## 证据要求

- 同一条结果关联 session_id、turn_id、request_id、实际执行 profile、场景和 outcome；事件序号只使用服务端实际给出的值。
- 性能比较记录源码、构建、模型参数、输入和历史形态。先满足终态唯一、幂等、权限、取消和恢复正确，再比较耗时。
- 缺失阶段保留缺失事实；不能补造 Provider 时间、终态或 settlement。取消和关闭需观察实际收口，不能用固定等待或进程消失代替。
- 集成脚本保留原始检查结果和时序，供故障定位。少量行为用例不作为稳定 P95 的证据。
- 如需统计 P50/P95，单独制定本次采样场景、样本量和统计口径，记录失败样本；before/after 必须可比且保留原始输入。
- 本地受控 Provider 证明 Magi 的调用、收口与展示行为；真实上游可用性和延迟须在涉及该问题时独立验证。

## 按问题选择验证

对话、审批、取消和重启恢复使用现有 Electron DOM 入口；Browser 权限和 MCP 使用各自集成入口。上下文压缩使用 DOM 脚本的 compaction 场景：历史预填充后验证 3 个连续 Turn，必须观察到至少一次 completed，并检查后续内容和终态。

不再维护通用的批量真实 Provider 采样、独立重启采样、ledger 派生和交叉性能报告流水线。旧脚本与历史统计从 Git 历史追溯；当前通过结论只来自本次执行。
