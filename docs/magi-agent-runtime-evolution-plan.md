# Magi 运行时边界与演进门槛

本文保留运行时演进的长期约束。已完成的 P0–P5 实施计划、旧工作树统计与逐轮验收记录已清退；具体行为以源码和下列领域文档为准，历史记录通过 Git 查询。

## 产品范围

Magi 提供 Electron 桌面 GUI 和 Web 工作台，手机和平板访问同一 Web 工作台。没有 Magi CLI、IDE 扩展或独立移动端的交付目标。`magi-daemon` 是唯一业务内核；开发和无头入口复用它，Electron Main 只托管进程和原生宿主能力。

外部 Agent Executor 只保留通用接口与历史证据读取。没有具体执行器、配置入口或专用 UI；历史外部证据不能进入原生工具重放，也不能代替原生完成条件。

## 事实所有权

| 事实 | 所有者与约束 |
| --- | --- |
| Input、Turn、Step、工具与交互 | SessionStore 的 canonical 事务；排队、引导、编辑和重试沿同一输入身份与版本，UI 只展示服务端回执 |
| Task、activation、子树转交与恢复 | TaskStore；HTTP 与 App Server 共用应用服务，资源未结算不能推断已完成 |
| 计划与持续目标 | SessionPlan / Goal；计划不是工具调用门禁，Goal 续跑让位于用户控制和排队输入 |
| 模型与角色 | 设置和角色注册表；角色负责职责匹配，不授予权限，显式模型绑定失效不能静默换模型 |
| 工具与权限 | 同一目录、预检和执行合同；按 Task、Skill、访问档与当前撤销约束计算，不按入口或来源隐式提权 |
| 上下文与用量 | Context Authority、Prompt Lineage、canonical attempt 与 Usage Authority；不以用量建立第二套调度预算 |
| 记忆与共享上下文 | `magi-memory-store` 与 `ContextRuntime` 的 `SharedContextPool`；不另设记忆文档写入器、分层仓库或 Mission 上下文池 |
| Browser 与进程 | Browser Authority 和各生命周期组；逻辑状态、物理资源、租约与执行代际分别明确归属 |
| UI | 消费服务端事实；只拥有视图、草稿和滚动等展示状态 |

具体合同见 [Turn 架构](conversation-response-core-architecture-redesign.md)、[上下文压力](context-pressure-compaction-architecture.md)、[缓存谱系](prompt-cache-lineage-architecture.md)、[Browser 设计](browser-runtime-design.md)和[项目架构](README.md)。

## 子代理边界

默认由主线完成任务；仅在用户明确要求或有明确并行、上下文隔离、独立复核价值时委派。多文件、长任务、Goal 和角色匹配本身不触发派发。角色选择、模型绑定解析、权限预检和资源接纳沿既有调用链完成。

子代理继承获授边界，默认 fresh 上下文；显式 fork 只冻结父任务的已结算可见历史。父子取消、转交、重启继续及未知副作用处理遵循[子代理合同](subagent-delegation-design.md)。自然语言中的派发理由不能充当可信授权；`auto/manual/off` 仍不是已交付的服务端策略开关。

## 插件、GUI/Web 和预留接口的边界

插件包、安装、授权和生命周期由 daemon 管理，后台由受信 Node 扩展宿主运行，业务提案经唯一 ExtensionBus 裁决。扩展能力及 UI 槽位以[插件系统设计](plugin-system-requirements-design.md)和[扩展协议](extension-protocol-v1.md)为准，不在本文维护第二份能力目录。插件不替换 Magi 的工作流或会话引擎。

工具参数被改写后重新校验权限；插件不能把拒绝改为允许。guard 失败、超时或熔断阻止受影响动作。停用和升级停止新接纳，并沿原生命周期取消或等待工作、结算资源；受信 Node 对文件、网络和进程的直接访问不因此成为沙箱能力。

Desktop 与 Web 共用同一 daemon 接纳、事件和交互合同；关闭界面、重连与宿主重启不改变任务归属。UI 不保存第二套持久队列、权限引擎或会话事实。

## 后续修改门槛

- 每次改动说明用户任务或故障、状态所有者、失败恢复和可验收结果。
- 从原入口修复根因，替换实现时删除旧路径及其专属测试、配置和文档；不为通过某个状态保留旁路、静默回退或双写。
- 多种传输、Provider 或插件翻译器可以复用同一业务内核；不能据“单链路”要求误删真实支持的协议适配或数据迁移。
- 数据升级在边界集中处理，采用原子提交和明确格式版本；回退使用匹配版本与备份，不能让旧程序继续写入新版事实。
- 不因存在接口就提前实现执行器、调度平台或插件能力；新增扩展面必须有真实使用场景和生命周期合同。
- 按改动面运行[验证入口](validation.md)中的定向检查。原有通过次数、临时截图和旧版模型样本不作为当前源码的验收证据；真实上游、Electron 与其他平台分别声明覆盖范围。
