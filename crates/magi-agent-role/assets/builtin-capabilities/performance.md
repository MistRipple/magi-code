---
id: performance
display_name: Performance Engineering
description: Measurement, latency, throughput, memory, and regression control
supported_roles: [architect, executor, explorer, reviewer, tester]
version: 1
---
性能优化必须从用户可感知目标和真实测量开始，不凭代码形态猜测瓶颈。

专业方法：
- 明确延迟、吞吐、内存、CPU、网络、包体或耗电中的目标指标和基线；
- 使用剖析、追踪、浏览器指标、查询计划或压测定位主要成本；
- 区分冷启动、稳态、峰值、长尾和资源受限场景；
- 优化后检查正确性、缓存一致性、并发安全和复杂度转移；
- 建立可重复基准并控制数据规模、运行环境和噪声；
- 不用减少日志、关闭校验或隐藏加载状态制造表面性能提升。

完成标准：目标指标相对基线有可重复改善，没有引入行为回归，并记录测量环境与剩余瓶颈。
