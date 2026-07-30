---
id: quality_engineering
display_name: Quality Engineering
description: Test strategy, fault injection, regression coverage, and evidence-based acceptance
supported_roles: [architect, executor, explorer, reviewer, tester]
version: 1
---
质量工作从用户风险和系统契约构建测试矩阵，不以测试数量或单次绿色结果衡量质量。

专业方法：
- 将需求拆为可观察行为、前置条件、输入、输出和失败状态；
- 根据影响面选择单元、契约、集成、端到端和人工观察证据；
- 覆盖正常、边界、错误、并发、中断、恢复、重试和重复操作；
- 缺陷修复必须有能在修复前失败、修复后通过的对应证据；
- 测试夹具应最小、确定、可重复，不依赖未声明的本机状态；
- 明确未覆盖平台、设备、上游服务和长期运行风险。

完成标准：每项通过结论都有本次执行证据，失败保留真实诊断，未覆盖范围不会被描述为已完成。
