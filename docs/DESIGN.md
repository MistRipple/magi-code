# MultiCLI 设计文档

> 版本: 0.5.0 | 最后更新: 2025-01-09

## 1. 项目愿景

**MultiCLI** 是一个 VSCode 插件，用于**编排多个 AI CLI 工具协作完成复杂编程任务**。

### 1.1 核心价值

- 🎯 **多 Agent 协作**：Claude + Codex + Gemini 等多 CLI 协作
- 🧠 **智能编排**：独立编排者 Claude 专职协调，Worker CLI 专职执行
- 🔄 **CLI 降级**：自动故障转移，根据执行统计智能选择 CLI
- 📊 **执行统计**：实时监控各 CLI 健康状态和成功率
- 📈 **依赖图调度**：任务依赖关系管理，最大化并行执行效率
- 💾 **上下文管理**：三层上下文架构，智能压缩长对话
- 📉 **优雅降级**：根据可用 CLI 自动调整执行策略
- 🎨 **统一体验**：在 VSCode 中提供一致的交互界面
- ⏹️ **可控执行**：流式输出 + 随时打断 + 一键还原

### 1.2 用户场景

#### 场景 1：全栈开发任务

**用户输入**：「添加用户登录功能，包括后端 API 和前端页面」

**执行流程**：
1. 系统分析任务，拆解为：
   - 后端 API 设计与实现
   - 前端登录页面开发
2. 根据 CLI 能力配置，分配任务：
   - 后端 API → Claude（或用户配置的首选 CLI）
   - 前端页面 → Gemini（或用户配置的首选 CLI）
3. 并行执行（不同文件，无冲突）
4. 用户在「待处理修改」中审查 Diff
5. 逐个或批量通过/还原

#### 场景 2：Bug 修复

**用户输入**：「修复 parseConfig 函数的空指针问题」

**执行流程**：
1. 系统识别为 Bug 修复任务
2. 分配给用户配置的 Bug 修复首选 CLI（默认 Codex）
3. CLI 直接修改文件
4. 用户审查 Diff，通过或还原

#### 场景 3：代码重构

**用户输入**：「将 UserService 拆分为 UserAuthService 和 UserProfileService」

**执行流程**：
1. 系统识别为架构重构任务
2. 分配给用户配置的架构首选 CLI（默认 Claude）
3. CLI 分析依赖关系，逐步修改多个文件
4. 用户审查所有变更，可单独通过/还原每个文件

#### 场景 4：打断与继续

**用户操作**：执行过程中点击「打断」

**系统行为**：
1. 立即停止 CLI 执行
2. 保留已创建的快照
3. 已修改的文件显示在「待处理修改」中
4. 用户可选择：
   - 「继续执行」：从断点继续
   - 「还原所有」：恢复到原始状态

---

## 2. 系统架构

### 2.1 架构层次

| 层级 | 组件 | 职责 |
|------|------|------|
| **UI 层** | Webview（主面板）、TreeView（CLI状态/待处理）、Status Bar | 用户交互界面 |
| **控制层** | UI Controller | 统一管理 UI 组件 |
| **编排层** | OrchestratorAgent（独立编排者 Claude） | 专职编排，不执行编码任务 |
| **执行层** | WorkerPool + WorkerAgent（多 CLI Workers） | 专职执行，向编排者汇报 |
| **通信层** | MessageBus | 编排者与 Worker 之间的消息通信 |
| **快照层** | Snapshot Manager | 文件快照、Diff 生成、通过/还原 |
| **上下文层** | ContextManager + ContextCompressor | 三层上下文管理和智能压缩 |

**外部依赖**：Claude CLI、Codex CLI、Gemini CLI（可扩展）

### 2.2 独立编排者架构 (v0.5.0 新增)

```
┌─────────────────────────────────────────────────────────────────┐
│                    OrchestratorAgent                            │
│                   (独立编排者 Claude)                            │
│  ─────────────────────────────────────────────────────────────  │
│  职责：                                                         │
│  • 100% 时间用于编排和监控                                      │
│  • 分析任务、生成执行计划                                       │
│  • 实时监控所有 Worker 状态                                     │
│  • 动态调度和错误处理                                           │
│  • CLI 降级决策                                                 │
└─────────────────────────────────────────────────────────────────┘
                              │
                    ┌─────────┴─────────┐
                    │    MessageBus     │
                    │   (消息总线)       │
                    └─────────┬─────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        │                     │                     │
        ▼                     ▼                     ▼
┌───────────────┐   ┌───────────────┐   ┌───────────────┐
│ Claude Worker │   │ Codex Worker  │   │ Gemini Worker │
│  (执行者)     │   │  (执行者)     │   │  (执行者)     │
│ ───────────── │   │ ───────────── │   │ ───────────── │
│ • 接收任务    │   │ • 接收任务    │   │ • 接收任务    │
│ • 执行编码    │   │ • 执行编码    │   │ • 执行编码    │
│ • 汇报进度    │   │ • 汇报进度    │   │ • 汇报进度    │
│ • 返回结果    │   │ • 返回结果    │   │ • 返回结果    │
└───────────────┘   └───────────────┘   └───────────────┘
```

**架构优势**：
- **职责分离**：编排者专注协调，Worker 专注执行
- **实时响应**：编排者可立即响应任何事件
- **灵活调度**：支持动态任务分配和 CLI 降级
- **可扩展性**：轻松添加新的 Worker 类型

### 2.3 角色边界与阶段职责（参考 GuDaStudio/skills）

> 角色边界明确化，避免“Worker 管控 Worker”的越权问题，确保流程可解释、可验证。

**Orchestrator（编排者）**  
- 负责：需求澄清、计划拆解、依赖编排、进度同步、验收标准、最终总结  
- 不负责：具体实现、直接修改文件、替 Worker 决策实现细节

**Worker Claude（架构/集成/联调）**  
- 负责：架构与目录结构、接口契约、联调方案、跨端冲突识别  
- 不负责：调度其他 Worker

**Worker Codex（后端）**  
- 负责：后端 API、数据层、鉴权、错误处理、测试  
- 不负责：前端 UI 或联调决策

**Worker Gemini（前端）**  
- 负责：UI/UX、组件实现、交互逻辑、接口调用  
- 不负责：后端接口定义或联调决策

**阶段职责矩阵（简化版）**

| 阶段 | 目标 | 责任角色 | 关键产物 |
| --- | --- | --- | --- |
| Phase 1 | 需求澄清/上下文检索 | Orchestrator | 需求边界、核心约束 |
| Phase 2 | 计划与拆分 | Orchestrator | 执行计划、依赖关系、验收标准 |
| Phase 3 | 架构/契约 | Worker Claude | 目录结构、接口契约、联调方案 |
| Phase 4 | 具体实现 | Worker Codex / Worker Gemini | 代码改动与说明 |
| Phase 5 | 自检/互检 | 原 Worker + 其他 Worker | 风险点与修复建议 |
| Phase 6 | 联调/集成 | Worker Claude | 契约一致性确认、冲突修复任务 |
| Phase 7 | 总结交付 | Orchestrator | 变更摘要、风险提示、后续建议 |

### 2.4 执行策略与场景覆盖

**并行优先原则**  
- 默认并行执行（`executionMode=parallel`）。  
- 只有两类情况强制串行：  
  1) 子任务存在显式依赖（DAG 调度保证顺序）  
  2) 多个子任务涉及同一个文件（文件锁互斥）  

**文件锁策略**  
- `targetFiles` 明确时直接加锁同文件，互斥执行。  
- `targetFiles` 缺失时，从任务描述/提示中识别文件路径加锁。  
- 无文件线索则不加锁，避免不必要的串行化。  

**典型场景覆盖**  
- 架构先行 → 前后端并行 → 联调收敛 → 修复并行  
- 同文件冲突：自动串行执行，保证修改顺序  
- 部分失败容错：失败只阻断依赖链，不阻断无依赖任务  
- 打断与继续：保留快照，支持恢复/重试  
- 多会话并发：旧会话进程需释放，防止资源抢占  

### 2.5 上下文注入与压缩策略

**目标**：保证 Worker 理解需求，同时控制 token 成本。  

**策略**  
- Orchestrator 保持完整上下文（Memory + 最近对话）。  
- Worker 只注入精简上下文（功能契约 + 子任务提示 + 受限记忆切片）。  

**配置项（建议）**  
- `workerMaxTokens`：Worker 上下文最大 token 数  
- `workerMemoryRatio`：Memory 占比（0-1）  
- `workerHighRiskExtraTokens`：高风险任务额外 token  

### 2.6 核心模块

#### CLI Detector (CLI 检测器)

**职责**：检测 CLI 可用性，制定降级策略

| 方法 | 说明 |
|------|------|
| `checkCLI(type)` | 检测单个 CLI 的可用性 |
| `checkAllCLIs()` | 检测所有 CLI |
| `getDegradationStrategy()` | 根据可用 CLI 制定降级策略 |

#### Task Router (任务路由器)

**职责**：分析任务类型，选择最佳 CLI

| 方法 | 说明 |
|------|------|
| `analyzeTask(prompt)` | 分析用户 Prompt，识别任务类型 |
| `selectCLIs(analysis, available)` | 根据任务和可用 CLI 选择执行者 |
| `planExecution(tasks)` | 规划执行顺序（并行/串行） |

#### Snapshot Manager (快照管理器)

**职责**：管理文件快照，生成 Diff，支持通过/还原

| 方法 | 说明 |
|------|------|
| `createSnapshot(filePath)` | 创建文件快照 |
| `getDiff(filePath)` | 本地生成 Diff（0 Token） |
| `accept(filePath)` | 通过修改，删除快照 |
| `revert(filePath)` | 还原文件，删除快照 |

#### Process Manager (进程管理器)

**职责**：管理 CLI 进程，支持流式输出和打断

| 方法 | 说明 |
|------|------|
| `spawn(cli, args)` | 启动 CLI 进程 |
| `interrupt(processId)` | 打断指定进程 |
| `interruptAll()` | 打断所有进程 |

---

## 3. CLI 能力系统 (Skills-based)

### 3.1 设计理念

采用 **Skills-based** 的设计思路，不固定 CLI 角色，而是：
- 定义任务类型（Skills）
- 用户可配置每种任务类型的首选 CLI
- 系统根据配置和 CLI 可用性动态选择

### 3.2 任务类型定义

| 任务类型 | 描述 | 默认首选 | 关键词示例 |
|---------|------|---------|-----------|
| `architecture` | 架构设计、系统重构 | Claude | 架构、设计、重构、模块 |
| `bugfix` | Bug 修复、问题排查 | Codex | 修复、bug、报错、调试 |
| `frontend` | 前端 UI/UX、组件开发 | Gemini | 前端、UI、组件、样式 |
| `backend` | 后端 API、数据库 | Claude | API、数据库、服务端 |
| `test` | 测试用例编写 | Codex | 测试、单元测试、覆盖率 |
| `docs` | 文档编写 | Claude | 文档、注释、README |
| `general` | 通用任务 | Claude | （默认） |

### 3.3 用户配置

用户可在设置中自定义每种任务类型的首选 CLI：

```json
{
  "multiCli.skills": {
    "architecture": "claude",
    "bugfix": "codex",
    "frontend": "gemini",
    "backend": "claude",
    "test": "codex",
    "docs": "claude",
    "general": "claude"
  }
}
```

### 3.4 CLI 选择逻辑

```
用户 Prompt
    ↓
┌─────────────────────────────────────┐
│ 1. 分析任务类型（关键词匹配）       │
└─────────────────────────────────────┘
    ↓
┌─────────────────────────────────────┐
│ 2. 查找用户配置的首选 CLI           │
└─────────────────────────────────────┘
    ↓
┌─────────────────────────────────────┐
│ 3. 检查首选 CLI 是否可用            │
│    - 可用 → 使用首选 CLI            │
│    - 不可用 → 降级到备选 CLI        │
└─────────────────────────────────────┘
```

---

## 4. 协调模式

### 4.1 并行模式 (parallel)

多个 CLI 同时执行，各自负责不同文件。

- **适用场景**：任务可拆分为独立子任务，无文件冲突
- **优点**：速度快
- **示例**：Claude 改后端，Gemini 改前端

### 4.2 串行模式 (sequential)

CLI 依次执行，后一个基于前一个的结果。

- **适用场景**：有依赖关系，或同一文件需多次修改
- **优点**：避免冲突
- **示例**：Claude 设计架构 → Codex 实现细节

### 4.3 冲突避免策略

**设计原则**：通过任务分配避免冲突，而不是事后解决。

| 场景 | 策略 |
|------|------|
| 不同文件 | 并行执行 |
| 同一文件 | 串行执行 |
| 复杂依赖 | 链式执行 |

### 4.4 任务队列与文件锁

为保证 *20+ 子任务* 不丢失，并避免同一文件被多个 CLI 同时修改，引入队列与锁：

- **单 CLI 队列**：每个 CLI 只有一个执行槽，任务进入队列依序执行，避免因忙碌被丢弃。
- **文件级锁**：按 `targetFiles` 申请排他锁，同一文件只能被一个任务写入。
- **优先级调度**：`priority` + 依赖度（被依赖数量）+ 等待时间提升（防饿死）。
- **锁释放唤醒**：锁释放后自动唤醒等待队列，确保所有任务最终完成。
- **统一重试**：重试逻辑由 WorkerPool 统一处理，编排者只展示重试进度，避免重复重试与状态冲突。

### 4.5 子任务自检与互检（已实现）

为了减少子代理输出质量波动，引入轻量自检与互检流程：

- **自检阶段**：子任务完成后由同一 CLI 执行快速自检（目标文件范围/需求一致性）。
- **互检阶段**：高风险任务由另一 CLI 执行 review，发现问题则驳回并进入修复轮次。
- **驳回策略**：若校验失败，进入下一轮修复（默认 1 轮），超过上限标记失败并保留错误摘要。
- **成本控制**：按任务描述关键词/文件类型决定是否触发互检。
- **限制说明**：依赖图调度模式下暂不启用自检/互检，以避免依赖链被重派发扰动。

---

## 5. 降级策略

### 5.1 CLI 状态类型

| 状态 | 说明 | 处理方式 |
|------|------|---------|
| `AVAILABLE` | 可用 | 正常执行 |
| `NOT_INSTALLED` | 未安装 | 降级到备选 CLI |
| `AUTH_FAILED` | 认证失败 | 提示用户配置 |
| `QUOTA_EXCEEDED` | 配额耗尽 | 降级到备选 CLI |
| `TIMEOUT` | 响应超时 | 重试 2 次后降级 |
| `NETWORK_ERROR` | 网络问题 | 重试 3 次后降级 |

### 5.2 降级逻辑

```
首选 CLI 不可用
    ↓
┌─────────────────────────────────────┐
│ 查找用户配置的备选 CLI              │
│ （按优先级顺序尝试）                │
└─────────────────────────────────────┘
    ↓
┌─────────────────────────────────────┐
│ 所有备选都不可用？                  │
│ - 是 → 提示用户安装 CLI             │
│ - 否 → 使用第一个可用的备选         │
└─────────────────────────────────────┘
```

### 5.3 能力总结表

| 可用 CLI | 推荐指数 | 说明 |
|---------|---------|------|
| Claude + Codex + Gemini | ⭐⭐⭐⭐⭐ | 全功能模式 |
| Claude + Codex | ⭐⭐⭐⭐ | 前端任务由 Claude 降级处理 |
| Claude + Gemini | ⭐⭐⭐⭐ | Bug 修复由 Claude 降级处理 |
| 仅 Claude | ⭐⭐⭐ | 单 CLI 模式，Claude 全能 |
| Codex + Gemini | ⭐⭐ | 无智能编排，关键词匹配 |
| 仅 Codex / 仅 Gemini | ⭐ | 能力受限 |
| 无可用 CLI | ❌ | 无法执行 |

---

## 6. 交互模式 (Interaction Modes)

### 6.1 三种交互模式

MultiCLI 支持三种交互模式，用户可根据任务风险和信任程度选择：

| 模式 | 说明 | 文件修改 | 命令执行 | Phase 2 确认 | 自动回滚 |
|------|------|----------|----------|--------------|----------|
| **Ask** | 对话模式 | ❌ 禁止 | ❌ 禁止 | - | - |
| **Agent** | 代理模式（默认） | ✅ 允许 | ✅ 允许 | ✅ 需要确认 | ❌ 手动 |
| **Auto** | 自动模式 | ✅ 允许 | ✅ 允许 | ❌ 跳过 | ✅ 自动 |

### 6.2 模式详解

#### Ask 模式（对话模式）
- **适用场景**：仅需咨询、代码解释、方案讨论
- **行为**：CLI 只能回答问题，不能修改任何文件或执行命令
- **安全级别**：最高

#### Agent 模式（代理模式）- 默认
- **适用场景**：日常开发任务，需要人工审核
- **行为**：
  - Phase 2 展示执行计划，等待用户确认
  - 执行后用户可在「变更」Tab 审查 Diff
  - 失败时提示用户选择：重试/回滚/继续
- **安全级别**：中等

#### Auto 模式（自动模式）
- **适用场景**：高信任任务、批量操作、CI/CD 集成
- **行为**：
  - 跳过 Phase 2 确认，直接执行
  - 失败时自动触发 3-Strike Protocol
  - 超过 3 次失败自动回滚所有变更
- **安全级别**：较低（需谨慎使用）

### 6.3 模式配置

```typescript
interface InteractionModeConfig {
  mode: 'ask' | 'agent' | 'auto';
  allowFileModification: boolean;
  allowCommandExecution: boolean;
  requireConfirmation: boolean;
  autoRollbackOnFailure: boolean;
}

// 预设配置
const INTERACTION_MODE_CONFIGS = {
  ask: {
    allowFileModification: false,
    allowCommandExecution: false,
    requireConfirmation: false,
    autoRollbackOnFailure: false,
  },
  agent: {
    allowFileModification: true,
    allowCommandExecution: true,
    requireConfirmation: true,
    autoRollbackOnFailure: false,
  },
  auto: {
    allowFileModification: true,
    allowCommandExecution: true,
    requireConfirmation: false,
    autoRollbackOnFailure: true,
  },
};
```

### 6.4 UI 交互

用户可在 Webview 顶部通过模式选择器切换：

```
┌─────────────────────────────────────────────────────────────────┐
│  [Ask] [Agent ✓] [Auto]    [+ 新建会话] [设置]                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## 7. 智能编排工作流

### 7.1 设计原则

**核心理念**：各 CLI 各司其职、独立执行、验证质量、智能恢复

| 原则 | 说明 |
|------|------|
| **独立执行** | 每个 CLI 直接修改文件，拥有完整的写入权限 |
| **专业分工** | 根据 Skills 配置，将任务分配给最擅长的 CLI |
| **并行高效** | 无冲突的任务并行执行，提高效率 |
| **质量保证** | 执行后验证编译、测试，确保代码质量 |
| **智能恢复** | 失败时自动重试，支持回滚机制 |
| **状态追踪** | 任务状态持久化，支持断点续传 |

### 7.2 七阶段工作流

> 参考 oh-my-opencode 的 Sisyphus Orchestrator 设计，增加验证和恢复阶段

```text
┌─────────────────────────────────────────────────────────────────┐
│ Phase 1: 任务分析 (Analysis)                                    │
│ ─────────────────────────────────────────────────────────────── │
│ Claude 分析用户 Prompt，生成执行计划：                          │
│ - 判断是否需要多 CLI 协作                                       │
│ - 识别任务类型（architecture/bugfix/frontend/backend/...）      │
│ - 根据 Skills 配置分配 CLI                                      │
│ - 确定执行顺序（并行/串行）                                     │
│ - 输出功能契约（接口/数据结构/交互约束）                        │
│ - 输出验收清单（功能验收标准）                                  │
│ - 创建 TaskState 列表，持久化到文件                             │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│ Phase 2: Hard Stop（执行计划确认）                              │
│ ─────────────────────────────────────────────────────────────── │
│ ⚠️ 暂停执行，展示计划，等待用户确认：                           │
│                                                                 │
│ 📋 执行计划：                                                   │
│ ┌─────────────────────────────────────────────────────────────┐ │
│ │ 1. [Codex] 后端 API 开发                                    │ │
│ │    - 创建 src/api/auth.ts                                   │ │
│ │    - 修改 src/routes/index.ts                               │ │
│ │                                                             │ │
│ │ 2. [Gemini] 前端页面开发                                    │ │
│ │    - 创建 src/components/LoginForm.tsx                      │ │
│ │    - 修改 src/pages/index.tsx                               │ │
│ │                                                             │ │
│ │ 执行顺序: 并行执行（无文件冲突）                            │ │
│ │ 预估时间: ~3 分钟                                           │ │
│ └─────────────────────────────────────────────────────────────┘ │
│                                                                 │
│ [Y] 确认执行    [N] 取消                                        │
└─────────────────────────────────────────────────────────────────┘
                              ↓ (用户确认 Y)
┌─────────────────────────────────────────────────────────────────┐
│ Phase 3: 并行/串行执行 (Execution)                              │
│ ─────────────────────────────────────────────────────────────── │
│ 各 CLI 直接执行各自负责的任务：                                 │
│                                                                 │
│ ┌─────────────────┐    ┌─────────────────┐                      │
│ │ Codex Worker    │    │ Gemini Worker   │                      │
│ │ ───────────────│    │ ─────────────── │                      │
│ │ 直接修改文件 ✅ │    │ 直接修改文件 ✅ │                      │
│ │ 流式输出日志    │    │ 流式输出日志    │                      │
│ │ 实时更新状态    │    │ 实时更新状态    │                      │
│ └─────────────────┘    └─────────────────┘                      │
│         ↓                      ↓                                │
│    后端代码完成           前端代码完成                          │
│                                                                 │
│ ⚠️ 关键：各 CLI 拥有完整写入权限，不是返回 Diff 让 Claude 执行  │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│ Phase 4: 集成联调 (Integration) ← 新增                          │
│ ─────────────────────────────────────────────────────────────── │
│ 编排者强制收敛：对齐前后端/架构契约，确保功能联调一致            │
│                                                                 │
│ - 汇总各子任务产出                                               │
│ - 校验功能契约与验收清单                                         │
│ - 生成修复子任务并回派（直到通过或超限）                         │
└─────────────────────────────────────────────────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│ Phase 5: 验证检查 (Verification)                                │
│ ─────────────────────────────────────────────────────────────── │
│ 自动验证执行结果的质量：                                        │
│                                                                 │
│ ┌─────────────────────────────────────────────────────────────┐ │
│ │ ✅ 编译检查: npm run compile                                │ │
│ │ ✅ IDE 诊断: 检查错误和警告                                 │ │
│ │ ⚙️ Lint 检查: npm run lint (可选)                           │ │
│ │ ⚙️ 测试检查: npm test (可选)                                │ │
│ └─────────────────────────────────────────────────────────────┘ │
│                                                                 │
│ 验证结果: [通过] / [失败]                                       │
└─────────────────────────────────────────────────────────────────┘
                              ↓
                    ┌─────────────────┐
                    │   验证通过？    │
                    └────────┬────────┘
                   Yes ↓           ↓ No
                       │    ┌─────────────────────────────────────┐
                       │    │ Phase 6: 失败恢复 (Recovery)        │
                       │    │ ─────────────────────────────────── │
                       │    │ 3-Strike Protocol:                  │
                       │    │                                     │
                       │    │ Strike 1: 原 CLI 尝试修复           │
                       │    │     ↓ 失败                          │
                       │    │ Strike 2: 换方法或提供更多上下文    │
                       │    │     ↓ 失败                          │
                       │    │ Strike 3: 升级到 Claude 修复        │
                       │    │     ↓ 失败                          │
                       │    │ 超过 3 次: 回滚 + 报告失败          │
                       │    │                                     │
                       │    │ [回滚所有] [保留部分] [强制继续]    │
                       │    └─────────────────────────────────────┘
                       │                    ↓ (修复成功)
                       └────────────────────┘
                              ↓
┌─────────────────────────────────────────────────────────────────┐
│ Phase 7: 汇总交付 (Summary)                                     │
│ ─────────────────────────────────────────────────────────────── │
│ Claude 汇总各 CLI 的执行结果，生成报告：                        │
│                                                                 │
│ ✅ 任务完成报告                                                 │
│ ┌─────────────────────────────────────────────────────────────┐ │
│ │ Codex 执行结果：                                            │ │
│ │ - ✅ 创建 src/api/auth.ts (+45 行)                          │ │
│ │ - ✅ 修改 src/routes/index.ts (+12 行)                      │ │
│ │                                                             │ │
│ │ Gemini 执行结果：                                           │ │
│ │ - ✅ 创建 src/components/LoginForm.tsx (+78 行)             │ │
│ │ - ✅ 修改 src/pages/index.tsx (+5 行)                       │ │
│ │                                                             │ │
│ │ 验证状态: ✅ 编译通过 | ✅ 无 IDE 错误                      │ │
│ │ 总计：4 个文件，+140 行                                     │ │
│ └─────────────────────────────────────────────────────────────┘ │
│                                                                 │
│ ⚠️ 注意：Claude 只汇总结果，不重新执行代码                      │
└─────────────────────────────────────────────────────────────────┘
```

### 7.3 简单任务 vs 复杂任务

| 任务类型 | 判断条件 | 执行方式 |
| -------- | -------- | -------- |
| **简单任务** | 单一领域、单一 CLI 可完成 | 直接执行，跳过 Phase 2 |
| **复杂任务** | 多领域、需要多 CLI 协作 | 完整 7 Phase 流程 |

**简单任务示例**：
- "修复 parseConfig 函数的空指针问题" → Codex 直接执行
- "优化登录页面的样式" → Gemini 直接执行

**复杂任务示例**：
- "添加用户登录功能，包括后端 API 和前端页面" → 多 CLI 协作
- "重构用户模块，拆分为认证和资料两个服务" → 多 CLI 协作

### 7.4 验证检查配置

```typescript
interface VerificationConfig {
  /** 编译检查（默认 true） */
  compileCheck: boolean;
  /** 编译命令（默认 npm run compile） */
  compileCommand: string;
  /** IDE 诊断检查（默认 true） */
  ideCheck: boolean;
  /** Lint 检查（默认 false） */
  lintCheck: boolean;
  /** Lint 命令（默认 npm run lint） */
  lintCommand: string;
  /** 测试检查（默认 false） */
  testCheck: boolean;
  /** 测试命令（默认 npm test） */
  testCommand: string;
  /** 验证超时时间（默认 60000ms） */
  timeout: number;
}
```

### 7.5 失败恢复策略 (3-Strike Protocol)

| Strike | 策略 | 说明 |
| ------ | ---- | ---- |
| **Strike 1** | 原 CLI 修复 | 将错误信息发送给原 CLI，让其尝试修复 |
| **Strike 2** | 换方法重试 | 提供更多上下文，或换一种实现方式 |
| **Strike 3** | 升级到 Claude | 让 Claude 分析问题并尝试修复 |
| **超过 3 次** | 回滚 + 报告 | 使用 SnapshotManager 回滚，报告失败原因 |

### 7.6 与其他框架的对比

| 维度 | oh-my-opencode | MultiCLI |
| ---- | -------------- | ------------ |
| **编排器** | Sisyphus (单一 Agent) | Claude (协调多 CLI) |
| **执行者** | 多个专门 Agent | 多个独立 CLI |
| **验证机制** | Phase 2B Verification | Phase 4 Verification |
| **恢复机制** | 3-Strike Protocol | 3-Strike Protocol (借鉴) |
| **状态管理** | Todo Continuation | TaskStateManager |

---

## 8. 任务状态管理

### 8.1 TaskStateManager

任务状态管理器负责追踪所有子任务的执行状态，支持持久化和实时同步。

```typescript
interface TaskState {
  id: string;                    // 任务唯一标识
  parentTaskId: string;          // 父任务 ID
  description: string;           // 任务描述
  assignedCli: CLIType;          // 分配的 CLI
  status: TaskStatus;            // 任务状态
  progress: number;              // 进度 0-100
  attempts: number;              // 重试次数
  startedAt?: number;            // 开始时间
  completedAt?: number;          // 完成时间
  result?: string;               // 执行结果
  error?: string;                // 错误信息
  modifiedFiles?: string[];      // 修改的文件列表
}

type TaskStatus =
  | 'pending'    // 等待执行
  | 'running'    // 执行中
  | 'completed'  // 已完成
  | 'failed'     // 失败
  | 'retrying'   // 重试中
  | 'cancelled'; // 已取消
```

### 8.2 状态同步机制

```text
┌─────────────────────────────────────────────────────────────────┐
│                    TaskStateManager                             │
│ ─────────────────────────────────────────────────────────────── │
│                                                                 │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐         │
│  │ CLI Worker  │───▶│ 状态更新    │───▶│ 事件发布    │         │
│  └─────────────┘    └─────────────┘    └─────────────┘         │
│                            │                  │                 │
│                            ▼                  ▼                 │
│                    ┌─────────────┐    ┌─────────────┐          │
│                    │ 持久化存储  │    │ UI 订阅更新 │          │
│                    │ .multicli│    │ WebviewPanel│          │
│                    └─────────────┘    └─────────────┘          │
└─────────────────────────────────────────────────────────────────┘
```

### 8.3 持久化存储

任务状态保存到 `.multicli/tasks/{sessionId}.json`：

```json
{
  "sessionId": "session_123",
  "createdAt": 1704672000000,
  "tasks": [
    {
      "id": "task_1",
      "description": "创建后端 API",
      "assignedCli": "codex",
      "status": "completed",
      "progress": 100,
      "attempts": 1,
      "modifiedFiles": ["src/api/auth.ts"]
    }
  ]
}
```

### 8.4 长任务连续执行

对于同一 CLI 的多个任务，采用**批量 Prompt** 策略：

```text
你需要完成以下任务：
1. [ ] 创建 src/api/auth.ts - 实现用户认证 API
2. [ ] 修改 src/routes/index.ts - 添加认证路由
3. [ ] 创建 src/middleware/auth.ts - 实现认证中间件

请按顺序完成，每完成一个任务后标记为 [x]。
完成所有任务后，输出 "ALL_TASKS_COMPLETED"。
```

**优势**：
- 减少 CLI 调用次数
- 保持上下文连贯性
- 支持任务间依赖

---

## 9. 执行流程

### 9.1 主流程

```text
用户输入 Prompt
       ↓
┌──────────────────────────────────────────────────────────────┐
│ 1. CLI Detector 检测可用 CLI                                 │
└──────────────────────────────────────────────────────────────┘
       ↓
┌──────────────────────────────────────────────────────────────┐
│ 2. Task Router 分析任务，分配给合适的 CLI                    │
│    - 无冲突文件 → 并行执行                                   │
│    - 有冲突文件 → 串行执行                                   │
└──────────────────────────────────────────────────────────────┘
       ↓
┌──────────────────────────────────────────────────────────────┐
│ 3. Snapshot Manager 创建快照                    [本地, 0 Token] │
│    - 首次修改的文件 → 保存原始内容                           │
│    - 已有快照的文件 → 跳过                                   │
└──────────────────────────────────────────────────────────────┘
       ↓
┌──────────────────────────────────────────────────────────────┐
│ 4. Worker 执行（CLI 直接修改文件）              [消耗 Token]  │
│    - 流式输出到侧边面板                                      │
│    - 支持随时打断                                            │
└──────────────────────────────────────────────────────────────┘
       ↓
┌──────────────────────────────────────────────────────────────┐
│ 5. 验证检查（Phase 4）                          [本地, 0 Token] │
│    - 编译检查 / IDE 诊断                                     │
│    - 失败时触发恢复流程                                      │
└──────────────────────────────────────────────────────────────┘
       ↓
┌──────────────────────────────────────────────────────────────┐
│ 6. 更新快照元数据                               [本地, 0 Token] │
│    - lastModifiedBy = 执行的 CLI                             │
│    - lastModifiedAt = 当前时间                               │
└──────────────────────────────────────────────────────────────┘
       ↓
┌──────────────────────────────────────────────────────────────┐
│ 7. 主输出区域显示完成总结                       [本地, 0 Token] │
│    - 各 CLI 修改的文件列表                                   │
│    - 变更行数统计                                            │
│    - 验证状态                                                │
└──────────────────────────────────────────────────────────────┘
       ↓
┌──────────────────────────────────────────────────────────────┐
│ 8. 待处理视图展示                               [本地, 0 Token] │
│    - 本地对比快照生成 Diff                                   │
│    - 显示：文件名 | 最后修改者 | 累计变更行数                │
│    - 按钮：[查看 Diff] [✅ 通过] [↩️ 还原]                   │
└──────────────────────────────────────────────────────────────┘
       ↓
┌──────────────────────────────────────────────────────────────┐
│ 9. 用户操作                                     [本地, 0 Token] │
│    - 通过 → 删除快照，确认修改                               │
│    - 还原 → 恢复原始内容，删除快照                           │
│    - 不操作 → 快照保留，继续显示                             │
└──────────────────────────────────────────────────────────────┘
```

---

## 10. 数据流

### 10.1 核心类型

**CLI 类型**：`claude` | `codex` | `gemini`

**Session（会话）**：

> Session 是指打开插件窗口时创建的会话，持续到关闭窗口。一个 Session 包含多个 Task。

| 字段 | 类型 | 说明 |
|------|------|------|
| id | string | Session 唯一标识 |
| createdAt | number | 创建时间戳 |
| tasks | Task[] | 该 Session 的所有任务 |
| snapshots | FileSnapshot[] | 该 Session 的所有快照 |

**Task（任务）**：

> Task 是用户每次输入 Prompt 时由 Orchestrator 创建的任务单元。

| 字段 | 类型 | 说明 |
|------|------|------|
| id | string | Task 唯一标识 |
| sessionId | string | 所属 Session |
| prompt | string | 用户原始输入 |
| status | TaskStatus | 任务状态 |
| subTasks | SubTask[] | 分解后的子任务列表 |
| createdAt | number | 创建时间戳 |
| completedAt | number? | 完成时间戳 |
| interruptedAt | number? | 打断时间戳 |

**Task 状态流转**：`pending` → `running` → `interrupted`（可选）→ `completed`（或 `failed`）

**SubTask（子任务）**：

> SubTask 是 Task 分解后的执行单元，每个 SubTask 由一个 CLI 执行。

| 字段 | 类型 | 说明 |
|------|------|------|
| id | string | SubTask 唯一标识 |
| taskId | string | 所属 Task |
| description | string | 子任务描述 |
| assignedCli | CLIType | 分配的 CLI |
| targetFiles | string[] | 目标文件列表 |
| status | SubTaskStatus | 子任务状态 |
| output | string[] | CLI 输出日志 |
| result | WorkerResult? | 执行结果 |
| startedAt | number? | 开始时间戳 |
| completedAt | number? | 完成时间戳 |

**SubTask 状态**：`pending` | `running` | `completed` | `failed` | `skipped`

**Worker 结果**：

| 字段 | 说明 |
|------|------|
| workerId | Worker 唯一标识 |
| cliType | CLI 类型 |
| success | 是否成功 |
| modifiedFiles | 修改的文件列表 |
| summary | CLI 输出的执行总结 |
| duration | 执行时长（毫秒） |
| interrupted | 是否被打断 |

**快照信息**：

| 字段 | 说明 |
|------|------|
| filePath | 文件路径 |
| originalContent | 原始内容 |
| lastModifiedBy | 最后修改者 |
| lastModifiedAt | 最后修改时间 |

**待处理变更**：

| 字段 | 说明 |
|------|------|
| filePath | 文件路径 |
| lastModifiedBy | 最后修改者 |
| additions | 累计新增行数 |
| deletions | 累计删除行数 |

### 10.2 事件系统

| 事件 | 触发时机 |
|------|---------|
| task:started | 任务开始执行 |
| task:completed | 任务完成 |
| task:failed | 任务失败 |
| worker:started | Worker 开始执行 |
| worker:output | Worker 产生流式输出 |
| worker:completed | Worker 执行完成 |
| worker:interrupted | Worker 被打断 |
| snapshot:created | 快照创建 |
| snapshot:accepted | 快照被通过 |
| snapshot:reverted | 快照被还原 |

---

## 11. UI 设计

> UI 示意图：[docs/ui-mockup.html](./ui-mockup.html)

### 11.1 界面布局（Augment 风格）

```
┌─────────────────────────────────────────────────────────────────┐
│  标题栏: MultiCLI + [+ 新建 Session] + [设置]              │
├─────────────────────────────────────────────────────────────────┤
│  Session 选择器: [Session 1 ×] [Session 2 ×]                   │
├─────────────────────────────────────────────────────────────────┤
│  顶部 Tab: [对话] [任务] [变更 (3)]                             │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  主内容区（对话流 / 任务列表 / 变更列表）                       │
│                                                                 │
├─────────────────────────────────────────────────────────────────┤
│  底部 Tab: [输入] [● Claude] [● Codex] [● Gemini]              │
├─────────────────────────────────────────────────────────────────┤
│  底部内容区（输入框 / CLI 输出）                                │
└─────────────────────────────────────────────────────────────────┘
```

### 11.2 顶部 Tab 说明

| Tab | 内容 |
|-----|------|
| **对话** | 用户与 MultiCLI 的对话流，包含任务卡片 |
| **任务** | 当前 Session 的任务列表，可查看历史任务 |
| **变更** | 待处理的文件变更列表，支持 Diff 预览、通过、还原 |

### 11.3 底部 Tab 说明

| Tab | 内容 |
|-----|------|
| **输入** | 任务输入框 + CLI 状态指示器 + 执行按钮 |
| **Claude/Codex/Gemini** | 各 CLI 的实时输出日志（流式显示） |

### 11.4 组件清单

| 组件 | 类型 | 功能 |
|------|------|------|
| `SessionSelector` | Webview | Session 切换、新建、关闭 |
| `ThreadView` | Webview | 对话流展示，包含任务卡片 |
| `TaskListView` | Webview | 任务列表，显示状态和子任务 |
| `EditsView` | Webview | 变更列表，Diff 预览 |
| `TaskInput` | Webview | 任务输入框 |
| `CLIOutputView` | Webview | CLI 实时输出（流式） |
| `StatusBarItem` | StatusBar | 快速状态指示 |

### 11.6 消息路由与展示规则

为避免编排者与 Worker 混淆，UI 展示遵循以下规则：

- **主对话（ThreadView）**：仅展示用户消息与 **Orchestrator** 消息（任务分析、执行计划、进度更新、验证结果、最终总结）。
- **CLI 面板（Claude/Codex/Gemini）**：仅展示各自 Worker 的流式输出与执行日志。
- **计划确认（Hard Stop）**：在主对话内显示确认卡片，并弹出确认模态。
- **来源标识**：主对话中 Orchestrator 与 CLI Worker 采用不同名称/颜色/标签，避免用户误判。

### 11.5 交互流程

1. **新建任务**：用户在输入框输入 → 点击执行 → 创建 Task → 分解 SubTasks → 执行
2. **查看输出**：点击底部 CLI Tab → 查看对应 CLI 的实时输出
3. **打断任务**：点击任务卡片的「打断」按钮 → Task 状态变为 interrupted
4. **继续任务**：在输入框用自然语言描述继续（如"继续刚才的任务"）
5. **审查变更**：切换到「变更」Tab → 查看 Diff → 通过或还原
6. **切换 Session**：点击 Session Tab 切换 / 点击「+ 新建 Session」创建新会话

---

## 12. 文件结构

| 路径 | 说明 |
|------|------|
| `src/extension.ts` | 插件入口 |
| `src/types.ts` | 统一类型定义（SubTask、WorkerType 等） |
| `src/cli-detector.ts` | CLI 检测器 |
| `src/events.ts` | 事件系统 |
| `src/diff-generator.ts` | Diff 生成器 |
| `src/snapshot-manager.ts` | 快照管理器 |
| `src/session-manager.ts` | 会话管理器 |
| `src/task-manager.ts` | 任务管理器 |
| `src/chat-session-manager.ts` | 聊天会话管理器 |
| **src/cli/** | CLI 适配器层 |
| `src/cli/base-adapter.ts` | CLI 适配器基类 |
| `src/cli/adapter-factory.ts` | 适配器工厂 |
| `src/cli/adapters/` | 各 CLI 适配器实现 |
| **src/orchestrator/** | 独立编排者架构 (v0.5.0) |
| `src/orchestrator/orchestrator-agent.ts` | 独立编排者 Claude 核心实现 |
| `src/orchestrator/worker-agent.ts` | Worker Agent 基类 |
| `src/orchestrator/worker-pool.ts` | Worker Pool 管理器 |
| `src/orchestrator/message-bus.ts` | 消息总线（编排者与 Worker 通信） |
| `src/orchestrator/protocols/types.ts` | 编排者架构核心类型定义 |
| `src/orchestrator/prompts/orchestrator-prompts.ts` | 编排者专用 Prompt 模板 |
| `src/orchestrator/intelligent-orchestrator.ts` | 智能编排器（7 Phase 工作流） |
| `src/orchestrator/task-state-manager.ts` | 任务状态管理器 |
| `src/orchestrator/verification-runner.ts` | 验证执行器 |
| `src/orchestrator/recovery-handler.ts` | 失败恢复处理器 |
| **src/orchestrator/context/** | 上下文管理层 |
| `src/orchestrator/context/memory-document.ts` | Memory 文档读写 |
| `src/orchestrator/context/context-manager.ts` | 三层上下文管理 |
| `src/orchestrator/context/context-compressor.ts` | 智能压缩代理 |
| **src/orchestrator/stats/** | 执行统计层 |
| `src/orchestrator/stats/execution-stats.ts` | 执行统计模块 |
| `src/orchestrator/stats/task-dependency-graph.ts` | 任务依赖图 |
| **src/task/** | 任务处理层 |
| `src/task/task-analyzer.ts` | 任务分析器 |
| `src/task/task-splitter.ts` | 任务拆分器 |
| `src/task/cli-selector.ts` | CLI 选择器 |
| `src/task/execution-scheduler.ts` | 执行调度器 |
| `src/task/result-aggregator.ts` | 结果聚合器 |
| `src/task/ai-task-decomposer.ts` | AI 任务分解器 |
| **src/workers/** | Worker 实现层 |
| `src/workers/base-worker.ts` | Worker 基类 |
| `src/workers/claude-worker.ts` | Claude Worker |
| `src/workers/codex-worker.ts` | Codex Worker |
| `src/workers/gemini-worker.ts` | Gemini Worker |
| **src/session/** | 会话持久化层 |
| `src/session/manager.ts` | 会话管理 |
| `src/session/storage.ts` | 会话存储 |
| **src/ui/** | UI 层 |
| `src/ui/webview-provider.ts` | Webview Provider |
| `src/ui/webview/index.html` | Webview 主界面 |

---

## 13. 开发路线图

### Phase 1: 基础架构 ✅

- [x] 项目初始化
- [x] 类型定义（含交互模式 ask/agent/auto）
- [x] CLI 检测器
- [x] Worker 基类
- [x] 事件系统

### Phase 2: 快照机制 ✅

- [x] Snapshot Manager
- [x] Session Manager
- [x] 本地 Diff 生成器

### Phase 3: Worker 实现 ✅

- [x] Claude/Codex/Gemini Worker
- [x] Process Manager（打断支持）
- [x] CLI 适配器层

### Phase 4: 智能编排 ✅

- [x] 7 Phase 工作流（分析→确认→执行→联调→验证→恢复→汇总）
- [x] 任务状态管理器（持久化 + 实时同步）
- [x] 验证执行器（编译/IDE诊断/Lint/测试）
- [x] 失败恢复处理器（3-Strike Protocol）
- [x] Prompt 模板系统

### Phase 5: UI 完善 ✅

- [x] Webview 主面板
- [x] CLI 状态面板
- [x] Diff 预览面板
- [x] 交互模式选择器（Ask/Agent/Auto）
- [x] 恢复确认对话框
- [x] 确认弹窗持久化（切换页面后保留）

### Phase 6: 独立编排者架构 ✅ (v0.5.0)

- [x] 核心类型和接口定义（protocols/types.ts）
- [x] 消息总线实现（message-bus.ts）
- [x] Worker Agent 基类（worker-agent.ts）
- [x] Worker Pool 管理（worker-pool.ts）
- [x] 编排者专用 Prompts（prompts/orchestrator-prompts.ts）
- [x] OrchestratorAgent 核心实现（orchestrator-agent.ts）
- [x] IntelligentOrchestrator 重构集成

### Phase 7: 上下文管理 ✅

- [x] MemoryDocument 类（Memory 文档读写）
- [x] ContextManager 类（三层上下文管理）
- [x] ContextCompressor 类（智能压缩代理）
- [x] 集成到编排器架构

### Phase 8: 功能性优化 ✅

- [x] 执行统计模块（ExecutionStats）
- [x] CLI 降级策略（自动故障转移）
- [x] 任务依赖图（TaskDependencyGraph）
- [x] 拓扑排序和并行分组
- [x] 执行统计 UI 面板
- [x] 编排者/Worker UI 视觉区分

### Phase 9: 系统集成 ✅

- [x] SnapshotManager 集成到 OrchestratorAgent
- [x] 统一任务类型定义（SubTask）
- [x] ExecutionScheduler 集成到 WorkerPool
- [x] 资源清理机制（VSCode 关闭时停止线程）

### Phase 10: 优化与发布 🚧

- [ ] 端到端功能测试
- [ ] 性能优化
- [ ] 文档完善
- [ ] 发布到 Marketplace

---

## 14. 配置项

| 配置项 | 默认值 | 说明 |
|--------|-------|------|
| `multiCli.claude.path` | `claude` | Claude CLI 路径 |
| `multiCli.codex.path` | `codex` | Codex CLI 路径 |
| `multiCli.gemini.path` | `gemini` | Gemini CLI 路径 |
| `multiCli.skills` | `{}` | 任务类型到 CLI 的映射 |
| `multiCli.snapshotDir` | `.multicli` | 快照存储目录 |
| `multiCli.timeout` | `300000` | 超时时间（毫秒） |

---

## 15. 风险与缓解

| 风险 | 缓解措施 |
|------|----------|
| CLI 版本不兼容 | 版本检测 + 最低版本要求 |
| Worker 超时 | 超时机制 + 打断功能 |
| 文件冲突 | 任务分配时避免冲突 |
| 快照丢失 | 存储在项目目录 |
| 打断后状态不一致 | 保留快照，用户可还原 |

---

## 16. 快照机制

**设计决策**：CLI 直接修改文件，Diff 由本地对比快照生成。

**原因**：
- **Token 最优**：CLI 不需要输出 Diff 内容，只输出简短的执行确认
- **Diff 本地生成**：对比快照与当前文件，0 Token 消耗
- **任务分配已避免冲突**：无冲突并行，有冲突串行

| CLI | 命令参数 | 说明 |
|-----|---------|------|
| **Claude** | 默认模式 | 直接修改文件 |
| **Codex** | `exec` | 直接修改文件 |
| **Gemini** | 默认模式 | 直接修改文件 |

**Token 消耗对比**：

| 模式 | CLI 输出 | Token 消耗 |
|------|---------|-----------|
| **直接修改 + 快照** ✅ | 执行确认（很短） | **最低** |
| Diff 预览模式 | Diff 内容 | 中等 |
| 输出完整文件 | 完整文件 | 最高 |

---

## 17. 用户交互流程

### 17.0 会话打断/继续/回滚

在多 CLI 编排场景中，系统对常见会话操作的行为如下：

- **打断**：立即中止编排者与所有 Worker；当前任务状态标记为 `interrupted`。
- **取消**：与打断等价，停止执行并保留快照，供后续审查/还原。
- **继续**：创建新任务，使用“原始需求 + 已产生变更”作为恢复上下文重新编排执行。
- **回滚**：对当前会话全部快照执行 `revertAllChanges()`，恢复到任务开始前状态。

### 17.1 状态流转

| 状态 | 显示内容 | 可用操作 |
|------|---------|---------|
| **pending** | 任务信息 | [执行] [取消] |
| **running** | CLI 进度、流式输出 | [打断] |
| **interrupted** | 已修改文件列表 | [继续] [还原所有] |
| **completed** | 完成总结、待处理列表 | [通过] [还原] |
| **failed** | 错误信息 | [重试] [取消] |

### 17.2 快捷键

| 快捷键 | 功能 |
|--------|------|
| `Ctrl+Shift+A` | 打开 MultiCLI |
| `Ctrl+Enter` | 执行任务 |
| `Escape` | 打断执行 |

---

## 18. 测试策略

### 18.1 Mock CLI

- 根据 Prompt 模式匹配返回预设响应
- 支持模拟延迟和错误

### 18.2 测试分层

| 层级 | 依赖 | 测试内容 |
|------|------|---------|
| L1 单元测试 | 无 | 核心逻辑 |
| L2 集成测试 | Mock CLI | 多 CLI 协作 |
| L3 E2E 测试 | 真实 CLI | 完整流程 |

---

## 附录 A: 流式输出与打断机制

### A.1 流式输出

使用 `child_process.spawn` 监听 stdout/stderr：

```typescript
this.process = spawn(command, args, { cwd, shell: true });
this.process.stdout?.on('data', (data) => {
  this.emit('worker:output', { type: 'stdout', content: data.toString() });
});
```

### A.2 打断机制

```typescript
interrupt(): void {
  if (this.process && !this.process.killed) {
    this.process.kill('SIGTERM');
    setTimeout(() => {
      if (!this.process?.killed) this.process?.kill('SIGKILL');
    }, 3000);
  }
}
```

---

## 附录 B: 快照机制详情

### B.1 存储位置

`{项目根目录}/.multicli/snapshots/{session_id}/`

### B.2 数据结构

```typescript
interface FileSnapshot {
  filePath: string;
  originalContent: string;
  lastModifiedBy: string;
  lastModifiedAt: number;
}
```

### B.3 核心操作

```typescript
// 创建快照
async function beforeModify(sessionId: string, filePath: string, cli: string) {
  const snapshotPath = getSnapshotPath(sessionId, filePath);
  if (!await exists(snapshotPath)) {
    const content = await readFile(filePath);
    await writeFile(snapshotPath, content);
  }
  await updateMeta(sessionId, filePath, { lastModifiedBy: cli });
}

// 通过
async function accept(sessionId: string, filePath: string) {
  await deleteFile(getSnapshotPath(sessionId, filePath));
}

// 还原
async function revert(sessionId: string, filePath: string) {
  const content = await readFile(getSnapshotPath(sessionId, filePath));
  await writeFile(filePath, content);
  await deleteFile(getSnapshotPath(sessionId, filePath));
}

// 获取 Diff（本地生成，0 Token）
async function getDiff(sessionId: string, filePath: string) {
  const original = await readFile(getSnapshotPath(sessionId, filePath));
  const current = await readFile(filePath);
  return generateUnifiedDiff(original, current, filePath);
}
```
