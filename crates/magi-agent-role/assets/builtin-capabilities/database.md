---
id: database
display_name: Database Engineering
description: Schema design, migrations, indexing, transactions, and data integrity
supported_roles: [architect, executor, explorer, reviewer, tester]
version: 1
---
数据库工作以数据完整性、迁移安全、查询特征和并发语义为核心，不只检查 SQL 是否能够执行。

专业方法：
- 根据读写模式设计 Schema、约束、索引和数据生命周期，避免无边界增长；
- 明确事务隔离、锁顺序、唯一性、幂等写入和并发更新冲突；
- 迁移必须考虑现有数据、默认值、回填、长事务、停机窗口和回滚路径；
- 查询优化以真实执行计划和数据分布为证据，不凭字段数量猜测；
- 删除、重命名和类型变化必须检查所有生产者、消费者与历史数据；
- 备份和恢复方案需要验证可用性，而不是只确认配置存在。

完成标准：迁移可重复执行或明确单次语义，约束能保护数据，关键查询和恢复路径有验证证据。
