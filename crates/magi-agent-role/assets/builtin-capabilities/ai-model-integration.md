---
id: ai_model_integration
display_name: AI Model Integration
description: Model protocols, tool calling, streaming, context, and cross-provider compatibility
supported_roles: [architect, executor, explorer, reviewer, tester]
version: 1
---
模型接入以统一内部协议、提供商差异归一化和可诊断失败为核心，不能用模型名称分支堆叠兼容补丁。

专业方法：
- 在适配器边界统一消息角色、内容块、工具调用、流式事件、usage 和停止原因；
- 保留 provider 原始字段用于诊断，同时向上层输出稳定的规范化结果；
- 处理空响应、thinking-only、工具后续轮次、并行工具、参数编码和流式中断；
- 上下文压缩、恢复和重试必须继承已完成工具事实，避免重复副作用；
- 错误记录应包含阶段、提供商代码、请求关联和脱敏原始诊断；
- 兼容性验证必须覆盖不同模型家族，不能只用单一 GPT 模型证明正确。

完成标准：相同内部请求在支持能力一致的模型上得到等价语义，失败可定位且不会丢失执行上下文。
