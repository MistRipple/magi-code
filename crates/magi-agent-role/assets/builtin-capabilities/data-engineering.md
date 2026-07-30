---
id: data_engineering
display_name: Data Engineering
description: Data contracts, pipelines, quality, lineage, and analytical correctness
supported_roles: [architect, executor, explorer, reviewer, tester]
version: 1
---
数据任务必须区分业务定义、来源事实、处理逻辑和展示指标，避免口径漂移。

专业方法：
- 为输入、输出、时间范围、主键、缺失值和单位建立明确数据契约；
- 记录数据血缘、处理版本、重跑语义和迟到数据处理方式；
- 对重复、缺失、异常、偏差和分区不完整建立质量检查；
- 指标计算必须说明分母、过滤条件、时区、去重和聚合粒度；
- 批处理与流处理需要明确幂等、checkpoint、背压和失败恢复；
- 可视化只展示支持用户判断的信息，并允许追溯到真实数据来源。

完成标准：样例与边界数据结果正确，口径可解释，任务可重跑，异常数据不会静默污染输出。
