# Magi 子系统清单（主链路实况）

## 概述

本文基于当前代码主链路整理 **22 个子系统**，分为 6 层。  
主链路起点：`src/extension.ts` -> `src/ui/webview-provider.ts` -> `src/orchestrator/core/mission-driven-engine.ts`。

为避免“代码存在 = 已运行”的误判，本清单使用三种状态：

| 状态 | 定义 |
|---|---|
| `主链路常驻` | 扩展激活后直接初始化，属于常驻运行路径 |
| `条件激活` | 由任务路径、配置开关或外部依赖可用性触发 |
| `当前未接主链路` | 代码存在，但当前激活链路未引用 |

---

## 一、核心基础设施层（5）

| # | 子系统 | 入口文件 | 状态 | 说明 |
|---|---|---|---|---|
| 1 | 扩展入口 | `src/extension.ts` | `主链路常驻` | VSCode 激活入口，创建 WebviewProvider 并注册命令 |
| 2 | 日志系统 | `src/logging/index.ts` | `主链路常驻` | 全链路日志分类与记录 |
| 3 | 配置系统 | `src/config/index.ts` | `主链路常驻` | `ConfigManager` 被运行时模块调用；LLM 配置由 `src/llm/config.ts` 主导 |
| 4 | 错误处理 | `src/errors/index.ts` | `当前未接主链路` | 当前仅见于 DI 内部引用，未进入激活链路 |
| 5 | 依赖注入 | `src/di/index.ts` | `当前未接主链路` | 容器代码存在，但未被扩展主链路使用 |

---

## 二、用户界面层（1）

| # | 子系统 | 入口文件 | 状态 | 说明 |
|---|---|---|---|---|
| 6 | 界面系统 | `src/ui/webview-provider.ts` | `主链路常驻` | 承接会话、引擎、消息与前端通信的核心入口 |

---

## 三、LLM 层（2）

| # | 子系统 | 入口文件 | 状态 | 说明 |
|---|---|---|---|---|
| 7 | LLM 配置 | `src/llm/config.ts` | `主链路常驻` | orchestrator/worker/compressor/skills/mcp 配置加载 |
| 8 | LLM 适配器工厂 | `src/llm/adapter-factory.ts` | `主链路常驻` | 创建 orchestrator + worker 适配器，并统一注入 Tool/MCP/Skills/环境上下文 |

---

## 四、编排层（5）

| # | 子系统 | 入口文件 | 状态 | 说明 |
|---|---|---|---|---|
| 9 | 任务驱动引擎 | `src/orchestrator/core/mission-driven-engine.ts` | `主链路常驻` | 编排主状态机、任务执行与恢复协调 |
| 10 | 消息中枢 | `src/orchestrator/core/message-hub.ts` | `主链路常驻` | 统一消息出口与前端路由 |
| 11 | 画像/路由 | `src/orchestrator/profile/` | `主链路常驻` | 意图分类后的分类、分配、指导注入链路 |
| 12 | LSP 执行器 | `src/orchestrator/lsp/lsp-enforcer.ts` | `条件激活` | 在 assignment 执行阶段启用，非启动即运行 |
| 13 | 智慧管理 | `src/orchestrator/wisdom/wisdom-extractor.ts` | `主链路常驻` | `WisdomManager` 常驻，内部使用提取器进行经验沉淀 |

---

## 五、工具层（4）

| # | 子系统 | 入口文件 | 状态 | 说明 |
|---|---|---|---|---|
| 14 | 工具管理器 | `src/tools/tool-manager.ts` | `主链路常驻` | 统一工具注册、权限与分发 |
| 15 | 技能管理器 | `src/tools/skills-manager.ts` | `条件激活` | 由 `adapter-factory.initialize()` 加载，受配置和加载结果影响 |
| 16 | MCP 管理器 | `src/tools/mcp-manager.ts` | `条件激活` | 由 MCP 执行器初始化并连接外部 MCP 服务 |
| 17 | 执行器集合 | `src/tools/*-executor.ts` | `主链路常驻` | 内置执行器实例在 ToolManager 构造时创建，调用按需触发 |

---

## 六、状态管理层（5）

| # | 子系统 | 入口文件 | 状态 | 说明 |
|---|---|---|---|---|
| 18 | 会话管理器 | `src/session/unified-session-manager.ts` | `主链路常驻` | 会话创建、切换与持久化 |
| 19 | Todo 管理器 | `src/todo/todo-manager.ts` | `条件激活` | 在任务执行/Worker 执行链路中初始化与调用 |
| 20 | 快照管理器 | `src/snapshot-manager.ts` | `主链路常驻` | 变更快照、回滚与变更统计 |
| 21 | 上下文管理器 | `src/context/context-manager.ts` | `主链路常驻` | 统一上下文拼装、压缩、共享池写入 |
| 22 | 项目知识库 | `src/knowledge/project-knowledge-base.ts` | `条件激活` | Webview 启动时尝试初始化，失败不阻断主链路 |

---

## 子模块（关键内核）

| 父系统 | 子模块 | 文件 |
|---|---|---|
| Context Manager | 记忆文档 | `src/context/memory-document.ts` |
| Context Manager | 上下文压缩器 | `src/context/context-compressor.ts` |
| Context Manager | 截断工具 | `src/context/truncation-utils.ts` |
| Orchestrator | 任务编排器 | `src/orchestrator/core/mission-orchestrator.ts` |
| Orchestrator | 分配执行器 | `src/orchestrator/core/executors/assignment-executor.ts` |
| Profile | 分类解析器 | `src/orchestrator/profile/category-resolver.ts` |
| Profile | 分配解析器 | `src/orchestrator/profile/assignment-resolver.ts` |

---

## 实际初始化链

```text
Extension
└── WebviewProvider
    ├── UnifiedSessionManager
    ├── SnapshotManager
    ├── DiffGenerator
    ├── LLMAdapterFactory
    │   ├── ToolManager
    │   │   └── Builtin Executors
    │   ├── loadSkills() [条件激活]
    │   └── loadMCP() [条件激活]
    ├── MissionDrivenEngine
    │   ├── MessageHub
    │   ├── ContextManager
    │   ├── PlanStorage / ExecutionStats / WisdomManager
    │   ├── MissionOrchestrator (Profile/Routing)
    │   └── AssignmentExecutor -> LspEnforcer [任务路径激活]
    └── ProjectKnowledgeBase [条件激活]
```

---

## 状态汇总

| 状态 | 数量 |
|---|---|
| `主链路常驻` | 15 |
| `条件激活` | 5 |
| `当前未接主链路` | 2 |
| `总计` | 22 |

---

**最后更新**: 2026-02-06
