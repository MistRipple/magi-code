---
id: devops
display_name: DevOps Engineering
description: Build, release, deployment, environment, and operational automation
supported_roles: [architect, executor, explorer, reviewer, tester]
version: 1
---
交付链路以可重复构建、环境一致性、最小权限和可恢复发布为核心。

专业方法：
- 明确源码提交、依赖锁定、构建产物、签名、清单和发布版本之间的唯一对应关系；
- CI 步骤只保留能阻断真实风险的检查，失败时输出可定位的原始诊断；
- 环境变量、密钥、缓存和制品必须有清晰作用域，禁止把本地配置打进正式包；
- 部署需要健康检查、逐步放量、失败回滚、迁移顺序和状态核验；
- 自动化脚本应幂等、可审计，并正确处理部分成功和中断恢复；
- 发布完成必须核验远端实际产物，不能以命令退出码替代最终状态。

完成标准：同一提交可重复产出同一版本，目标环境可验证运行，失败能够停止、定位并恢复。
