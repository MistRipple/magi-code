# MultiCLI 子系统清单 / Subsystem Catalog

## 概述 / Overview

MultiCLI 项目共包含 **22 个子系统**，分为 6 个层级。

MultiCLI project contains **22 subsystems** organized into 6 layers.

---

## 一、核心基础设施层 / Core Infrastructure Layer (5)

| # | 中文名称 | English Name | 入口文件 / Entry File | 职责 / Responsibility |
|---|----------|--------------|----------------------|----------------------|
| 1 | 扩展入口 | Extension Entry | `src/extension.ts` | VSCode 扩展激活和生命周期管理 |
| 2 | 日志系统 | Logging System | `src/logging/index.ts` | 统一日志记录和分类 |
| 3 | 配置系统 | Config System | `src/config/index.ts` | 配置文件加载和管理 |
| 4 | 错误处理 | Error Handling | `src/errors/index.ts` | 统一错误类型和处理 |
| 5 | 依赖注入 | DI Container | `src/di/index.ts` | 依赖注入容器 |

---

## 二、用户界面层 / User Interface Layer (1)

| # | 中文名称 | English Name | 入口文件 / Entry File | 职责 / Responsibility |
|---|----------|--------------|----------------------|----------------------|
| 6 | 界面系统 | UI/Webview System | `src/ui/webview-provider.ts` | Webview 面板、消息通信、前端交互 |

---

## 三、LLM 层 / LLM Layer (2)

| # | 中文名称 | English Name | 入口文件 / Entry File | 职责 / Responsibility |
|---|----------|--------------|----------------------|----------------------|
| 7 | LLM 配置 | LLM Config | `src/llm/config.ts` | LLM 配置加载 (orchestrator/worker/compressor) |
| 8 | LLM 适配器工厂 | LLM Adapter Factory | `src/llm/adapter-factory.ts` | Worker 适配器创建和管理 |

---

## 四、编排层 / Orchestration Layer (5)

| # | 中文名称 | English Name | 入口文件 / Entry File | 职责 / Responsibility |
|---|----------|--------------|----------------------|----------------------|
| 9 | 任务驱动引擎 | Mission-Driven Engine | `src/orchestrator/core/mission-driven-engine.ts` | 核心编排引擎，任务执行和协调 |
| 10 | 消息中枢 | Message Hub | `src/orchestrator/core/message-hub.ts` | 统一消息路由和分发 |
| 11 | 配置文件/路由 | Profile/Routing | `src/orchestrator/profile/` | Worker 分配、分类规则、Guidance 注入 |
| 12 | LSP 执行器 | LSP Enforcer | `src/orchestrator/lsp/lsp-enforcer.ts` | LSP 预检和代码分析 |
| 13 | 智慧提取器 | Wisdom Extractor | `src/orchestrator/wisdom/wisdom-extractor.ts` | 经验学习和知识提取 |

---

## 五、工具层 / Tool Layer (4)

| # | 中文名称 | English Name | 入口文件 / Entry File | 职责 / Responsibility |
|---|----------|--------------|----------------------|----------------------|
| 14 | 工具管理器 | Tool Manager | `src/tools/tool-manager.ts` | 工具注册、执行和权限控制 |
| 15 | 技能管理器 | Skills Manager | `src/tools/skills-manager.ts` | 技能加载和执行 |
| 16 | MCP 管理器 | MCP Manager | `src/tools/mcp-manager.ts` | MCP 服务器连接和工具调用 |
| 17 | 执行器集合 | Executors | `src/tools/*-executor.ts` | 文件/搜索/终端/Web/LSP/ACE 执行器 |

---

## 六、状态管理层 / State Management Layer (5)

| # | 中文名称 | English Name | 入口文件 / Entry File | 职责 / Responsibility |
|---|----------|--------------|----------------------|----------------------|
| 18 | 会话管理器 | Session Manager | `src/session/unified-session-manager.ts` | 会话创建、切换、持久化 |
| 19 | 任务管理器 | Task Manager | `src/task/unified-task-manager.ts` | 任务跟踪和状态管理 |
| 20 | 快照管理器 | Snapshot Manager | `src/snapshot-manager.ts` | 代码快照和回滚 |
| 21 | 上下文管理器 | Context Manager | `src/context/context-manager.ts` | 三层上下文管理、Memory、压缩 |
| 22 | 项目知识库 | Project Knowledge Base | `src/knowledge/project-knowledge-base.ts` | ADR、FAQ、Learning 管理 |

---

## 子模块 / Submodules

以下子模块作为父系统的内部组件：

The following submodules are internal components of parent systems:

| 父系统 / Parent | 中文名称 | English Name | 文件 / File |
|-----------------|----------|--------------|-------------|
| Context Manager | 记忆文档 | Memory Document | `src/context/memory-document.ts` |
| Context Manager | 上下文压缩器 | Context Compressor | `src/context/context-compressor.ts` |
| Context Manager | 截断工具 | Truncation Utils | `src/context/truncation-utils.ts` |
| Orchestrator | 任务编排器 | Mission Orchestrator | `src/orchestrator/core/mission-orchestrator.ts` |
| Orchestrator | 分配执行器 | Assignment Executor | `src/orchestrator/core/executors/assignment-executor.ts` |
| Profile | 分类解析器 | Category Resolver | `src/orchestrator/profile/category-resolver.ts` |
| Profile | 分配解析器 | Assignment Resolver | `src/orchestrator/profile/assignment-resolver.ts` |

---

## 注册链 / Registration Chain

```
Extension (扩展入口)
└── WebviewProvider (界面系统)
    ├── UnifiedSessionManager (会话管理器)
    ├── SnapshotManager (快照管理器)
    ├── LLMAdapterFactory (LLM 适配器工厂)
    │   └── ToolManager (工具管理器)
    │       ├── SkillsManager (技能管理器)
    │       ├── MCPManager (MCP 管理器)
    │       └── Executors (执行器集合)
    ├── MissionDrivenEngine (任务驱动引擎)
    │   ├── MessageHub (消息中枢)
    │   ├── ContextManager (上下文管理器)
    │   │   ├── MemoryDocument (记忆文档)
    │   │   └── ContextCompressor (上下文压缩器)
    │   ├── ProfileLoader (配置文件加载器)
    │   ├── WisdomExtractor (智慧提取器)
    │   └── LspEnforcer (LSP 执行器)
    ├── ProjectKnowledgeBase (项目知识库)
    └── UnifiedTaskManager (任务管理器)
```

---

## 状态 / Status

| 状态 | Status | 数量 | Count |
|------|--------|------|-------|
| ✅ 已注册运行 | Registered & Running | 22 | 22 |
| ❌ 未注册 | Not Registered | 0 | 0 |

---

**最后更新 / Last Updated**: 2026-02-04
