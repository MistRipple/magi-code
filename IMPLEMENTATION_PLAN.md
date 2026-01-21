# MultiCLI 重构计划：从 CLI 编排迁移到 LLM 直接编排

**开始时间**: 2026-01-20
**优先级**: P0

---

## 需求澄清

### 核心需求
1. **模型支持**：OpenAI 格式 + Anthropic 格式（可扩展）
2. **配置简化**：每个模型只需 baseUrl/apiKey/model 三项配置
3. **内置 Shell**：支持命令执行能力
4. **交互模式**：只保留 ask（对话）和 auto（自动执行）两种模式
5. **画像系统**：画像随模型配置走，提供常用画像快速应用
6. **UI 重构**：使用 HTML + Lit 架构，支持多模型配置管理

### 简化原则
- 策略、画像、分类合并为统一的「模型配置」
- 去除复杂的编排层，简化为直接的 LLM 调用
- 配置从弹窗迁移到专用设置页面

---

## 实施阶段

### Stage 1: 基础设施 ✅
**Goal**: 建立 LLM Provider 层和基础类型系统
**Status**: Complete

### Stage 2: Agent + Shell ✅
**Goal**: 实现 Agent 和 Shell 执行能力
**Status**: Complete

### Stage 3: 编排器 ✅
**Goal**: 实现简化的编排逻辑（ask/auto）
**Status**: Complete

### Stage 4: UI 重构（Lit）✅
**Goal**: 使用 Lit 重构 UI
**Status**: Complete

### Stage 5: 集成测试 ✅
**Goal**: 完成整体集成
**Status**: Complete

### Stage 6: CLI 优秀特性迁移 ✅
**Goal**: 将 CLI 版本的优秀特性迁移到 LLM 版本

#### P0 - 必须迁移
- [x] 上下文管理 (ContextManager) - 三层上下文 + 智能截断

#### P1 - 重要迁移
- [x] 统一日志系统 (Logger) - 分级/分类/文件日志
- [x] 文件快照 (SnapshotManager) - 创建/还原/对比快照
- [x] 失败恢复 (RecoveryHandler) - 重试/回滚机制

#### P2 - 建议迁移
- [x] 消息追踪 (MessageTracer) - 全链路追踪
- [x] 文件锁管理 (FileLockManager) - 防冲突
- [x] 代码索引 (IndexManager) - 项目代码索引

**Status**: Complete

### Stage 7: 工具/技能/MCP 集成 ✅
**Goal**: 构建可扩展的工具系统、技能系统和 MCP 协议支持
**Architecture Reference**: https://modelcontextprotocol.io/docs/concepts/architecture

#### Phase 1 - Tool Registry (工具注册表)
- [x] 工具类型定义 (ToolDefinition, ToolContext, ToolResult)
- [x] 工具注册表 (ToolRegistry) - 注册/发现/执行
- [x] 内置工具 (ShellTool, FileTool, WebTool)
- [x] 工具执行器 (ToolExecutor) - 统一执行接口

#### Phase 2 - Skills (技能系统)
- [x] 技能类型定义 (SkillDefinition, SkillContext)
- [x] 技能注册表 (SkillRegistry)
- [x] 技能路由器 (SkillRouter) - 意图匹配
- [x] 内置技能 (CodeReview, Refactor, Debug)

#### Phase 3 - MCP Protocol (MCP 协议)
- [x] MCP 类型定义 (基于 JSON-RPC 2.0)
- [x] MCP 服务端 (MCPServer) - 暴露工具/资源/提示
- [x] MCP 客户端 (MCPClient) - 连接外部 MCP 服务
- [x] 传输层 (StdioTransport, HttpTransport)
- [x] 能力协商和生命周期管理

**Status**: Complete

---

## 进度跟踪

- [x] Stage 1: 基础设施
- [x] Stage 2: Agent + Shell
- [x] Stage 3: 编排器
- [x] Stage 4: UI 重构
- [x] Stage 5: 集成测试
- [x] Stage 6: CLI 特性迁移
- [x] Stage 7: 工具/技能/MCP 集成

**当前阶段**: 全部完成
**完成度**: 100%

---

**最后更新**: 2026-01-20
