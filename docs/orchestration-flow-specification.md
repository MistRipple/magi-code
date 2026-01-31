# MultiCLI 编排流程规范

> 版本: 1.0
> 日期: 2025-01-31
> 状态: **正式规范**

## 1. 产品定位

**MultiCLI**: 编排多个 AI Worker (Claude/Codex/Gemini) 协作完成复杂任务的 VS Code 扩展。

### 1.1 核心理念

| 原则 | 描述 |
|------|------|
| **编排者专注编排** | 编排者负责意图分析、任务分解、结果汇总，不执行代码操作 |
| **Worker 专注执行** | Worker 负责具体的代码读取、修改、创建等操作 |
| **AI 决策优先** | 意图分类由 AI (LLM) 完成，非硬编码规则 |
| **配置驱动** | Worker 选择、任务分类基于画像配置，可灵活调整 |

### 1.2 关键区分：编排者回答 vs Worker 执行

```
┌─────────────────────────────────────────────────────────────────────┐
│                     决策核心：是否涉及代码操作                         │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  不涉及代码操作                    涉及代码操作                       │
│  ──────────────                    ──────────────                   │
│  • 知识问答                        • 代码修改                        │
│  • 身份能力问询                    • 代码分析                        │
│  • 简单问候                        • 文件创建/删除                   │
│  • 生成文本/方案                   • 重构/优化代码                   │
│  • 概念解释                        • Bug 修复                        │
│         ↓                                ↓                          │
│  编排者直接回答                    调用 Worker 执行                  │
│  (executeAskMode)                  (QuickExecutor/MissionOrchestrator)│
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 2. 编排架构概览

```
┌──────────────────────────────────────────────────────────────────────┐
│                          用户输入                                     │
└─────────────────────────────┬────────────────────────────────────────┘
                              ↓
┌──────────────────────────────────────────────────────────────────────┐
│                    MissionDrivenEngine.execute()                      │
│                         (统一入口)                                    │
└─────────────────────────────┬────────────────────────────────────────┘
                              ↓
┌──────────────────────────────────────────────────────────────────────┐
│                     IntentGate.process()                              │
│                    (AI 意图分类 - LLM)                                │
│  ┌────────────────────────────────────────────────────────────────┐  │
│  │  输出: ASK | DIRECT | EXPLORE | TASK | CLARIFY                 │  │
│  └────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────┬────────────────────────────────────────┘
                              ↓
          ┌───────────────────┴───────────────────┐
          ↓                   ↓                   ↓
   ┌─────────────┐    ┌──────────────┐    ┌──────────────┐
   │  ASK 模式   │    │ DIRECT/EXPLORE│    │  TASK 模式   │
   │ 编排者回答  │    │   条件判断    │    │ 完整编排流程 │
   └──────┬──────┘    └──────┬───────┘    └──────┬───────┘
          ↓                   ↓                   ↓
   executeAskMode    hasCodeOperationIntent?   MissionOrchestrator
          ↓              ↓         ↓                   ↓
     直接回答        否:编排者  是:Worker         Worker 协作
                      回答       执行
```

---

## 3. 意图分类层 (AI 决策)

### 3.1 意图类型定义

| 意图类型 | 定义 | 典型场景 |
|---------|------|----------|
| **question** | 不涉及代码操作的问询/生成 | "你是谁"、"给我生成一个流程图" |
| **trivial** | 极其简单的代码操作 | "把函数名改成 handleClick" |
| **exploratory** | 代码分析，不修改 | "分析一下这段代码的逻辑" |
| **task** | 复杂代码任务 | "重构用户认证模块" |
| **ambiguous** | 意图不明确 | "优化一下性能" |
| **open_ended** | 开放性讨论 | "你觉得这个设计怎么样" |

### 3.2 处理模式映射

| 意图类型 | 推荐模式 | 执行方式 |
|---------|---------|---------|
| question | **ASK** | 编排者直接回答 |
| trivial | **DIRECT** | 检查后决定 |
| exploratory | **EXPLORE** | 检查后决定 |
| task | **TASK** | 完整 Mission 流程 |
| ambiguous/open_ended | **CLARIFY** | 返回澄清问题 |

### 3.3 配置文件

**文件**: `src/orchestrator/prompts/intent-classification.ts`

该文件定义了 AI 意图分类的 Prompt 模板，包含：
- 意图类型定义
- 判断准则
- 示例（Few-shot Learning）

---

## 4. 执行路径详解

### 4.1 ASK 模式 (编排者直接回答)

```typescript
// 代码位置: src/orchestrator/core/mission-driven-engine.ts
if (intentResult.mode === IntentHandlerMode.ASK) {
  const result = await this.executeAskMode(userPrompt, taskId, sessionId);
  return result;
}
```

**特点**:
- 不调用 Worker
- 编排者 LLM 直接生成回答
- 适用于问答、问候、文本生成等

**适用场景**:
- "你是谁" / "你能做什么"
- "什么是 TypeScript"
- "给我生成一个流程图"
- "帮我写一个营销方案"

### 4.2 DIRECT/EXPLORE 模式 (条件判断)

```typescript
// 代码位置: src/orchestrator/core/mission-driven-engine.ts
if (intentResult.mode === IntentHandlerMode.DIRECT) {
  const needsWorker = hasCodeOperationIntent(this.profileLoader, userPrompt);
  if (needsWorker) {
    // 涉及代码操作 → 调用 Worker 执行
    const result = await this.executeQuickPath(userPrompt, mode, taskId, sessionId);
  } else {
    // 不涉及代码操作 → 编排者直接回答
    const result = await this.executeAskMode(userPrompt, taskId, sessionId);
  }
}
```

**判断依据**: `hasCodeOperationIntent()` 函数

```typescript
// 代码位置: src/orchestrator/profile/category-matcher.ts
export function hasCodeOperationIntent(profileLoader, prompt): boolean {
  // 遍历 categories.ts 中定义的所有任务分类关键词
  // 匹配任何关键词 → 返回 true (涉及代码操作)
  // 不匹配任何关键词 → 返回 false (不涉及代码操作)
}
```

**任务分类配置**: `src/orchestrator/profile/defaults/categories.ts`

| 分类 | 关键词示例 | 默认 Worker |
| ---- | ---------- | ----------- |
| architecture | 架构、设计、模块、重构 | claude |
| backend | 后端、API、数据库 | claude |
| frontend | 前端、UI、组件、页面 | gemini |
| bugfix | 修复、bug、fix、错误 | codex |
| test | 测试、test、单元测试 | codex |
| refactor | 重构、优化、改进 | claude |

### 4.3 TASK 模式 (完整编排流程)

```
用户请求
    ↓
IntentGate → TASK
    ↓
MissionOrchestrator.processRequest()
    ↓
创建 Mission
    ↓
任务分解 (理解目标 + 规划协作)
    ↓
分配 Assignments (基于画像配置选择 Worker)
    ↓
Worker 执行 (带进度汇报)
    ↓
结果验证与总结
```

**适用场景**:
- "重构用户认证模块，提取公共逻辑"
- "实现新的登录功能"
- "修复 #123 issue 并编写测试"

### 4.4 CLARIFY 模式 (澄清)

当意图不明确时，返回澄清问题让用户补充信息：

```typescript
if (intentResult.mode === IntentHandlerMode.CLARIFY) {
  // 调用 clarificationCallback 获取用户补充信息
  // 拼接补充信息后重新执行
  const clarifiedPrompt = `${userPrompt}\n\n补充信息：${answers}`;
  return this.execute(clarifiedPrompt, taskId, sessionId);
}
```

---

## 5. Worker 选择机制

### 5.1 设计原则

- **画像驱动**: 所有 Worker 选择基于 `ProfileLoader` 配置
- **无硬编码**: 不存在 `return 'claude'` 或 `switch-case` 硬编码
- **动态降级**: Worker 不可用时，根据画像配置选择替代

### 5.2 选择流程

```
任务请求
    ↓
matchCategoryWithProfile(prompt)
    ↓
匹配任务分类 (categories.ts 关键词)
    ↓
获取分类的 defaultWorker
    ↓
检查 Worker 可用性
    ↓
可用 → 使用该 Worker
不可用 → selectFallbackWorker() 从画像获取替代
```

### 5.3 配置文件

| 文件 | 作用 |
|------|------|
| `src/orchestrator/profile/defaults/categories.ts` | 任务分类 → 默认 Worker 映射 |
| `src/orchestrator/profile/defaults/claude.json` | Claude 画像：擅长分类、关键词 |
| `src/orchestrator/profile/defaults/codex.json` | Codex 画像 |
| `src/orchestrator/profile/defaults/gemini.json` | Gemini 画像 |

---

## 6. 消息路由规范

### 6.1 消息来源与区域

| 来源 | 显示区域 | 说明 |
|------|---------|------|
| orchestrator | 主对话区 | 编排者叙事、规划、总结 |
| worker (claude/codex/gemini) | Worker Tab | 执行过程、代码变更 |
| system | 主对话区 | 系统通知、阶段变化 |
| subTaskCard | 主对话区 | 子任务完成摘要卡片 |

### 6.2 主对话区内容规范

**必须包含**:
- 编排者分析/规划摘要
- 任务分配说明
- SubTaskCard (子任务完成摘要)
- 最终总结

**禁止包含**:
- Worker 执行过程细节
- 工具调用详情
- 中间状态变化

---

## 7. 完整流程图

```mermaid
flowchart TB
    subgraph Input["🎯 用户输入"]
        A[用户消息] --> B[MissionDrivenEngine.execute]
    end

    subgraph IntentAnalysis["🧠 意图分析层 (AI 决策)"]
        B --> C[IntentGate.process]
        C --> D{LLM 意图分类}
        D --> E{推荐模式?}
    end

    subgraph AskPath["💬 问答路径 (编排者直接回答)"]
        E -->|ASK| F1[executeAskMode]
        F1 --> F2[编排者 LLM 直接回答]
    end

    subgraph DirectExplorePath["⚡ 快速路径 (条件判断)"]
        E -->|DIRECT/EXPLORE| G1{hasCodeOperationIntent?}
        G1 -->|否: 不涉及代码| G2[executeAskMode]
        G1 -->|是: 涉及代码| G4[executeQuickPath]
        G4 --> G5[QuickExecutor → Worker]
    end

    subgraph TaskPath["🔧 完整编排路径"]
        E -->|TASK| H1[MissionOrchestrator]
        H1 --> H2[Mission 生命周期]
        H2 --> H3[Worker 协作执行]
    end

    subgraph ClarifyPath["❓ 澄清路径"]
        E -->|CLARIFY| I1[返回澄清问题]
        I1 --> I2[用户补充] --> B
    end

    F2 --> Output[响应输出]
    G2 --> Output
    G5 --> Output
    H3 --> Output
```

---

## 8. 关键代码位置索引

| 功能 | 文件路径 |
|------|---------|
| 统一入口 | `src/orchestrator/core/mission-driven-engine.ts` |
| 意图分析 | `src/orchestrator/intent-gate.ts` |
| 意图分类 Prompt | `src/orchestrator/prompts/intent-classification.ts` |
| 快速执行器 | `src/orchestrator/core/quick-executor.ts` |
| Mission 编排 | `src/orchestrator/core/mission-orchestrator.ts` |
| 任务分类配置 | `src/orchestrator/profile/defaults/categories.ts` |
| 分类匹配器 | `src/orchestrator/profile/category-matcher.ts` |
| 画像加载器 | `src/orchestrator/profile/profile-loader.ts` |
| 消息中心 | `src/orchestrator/core/message-hub.ts` |

---

## 9. 验收检查清单

### 9.1 意图分类

- [ ] "你是谁" → ASK 模式 → 编排者直接回答
- [ ] "给我生成一个流程图" → ASK 模式 → 编排者直接回答
- [ ] "把这个函数改名" → DIRECT 模式 → Worker 执行
- [ ] "分析这段代码" → EXPLORE 模式 → Worker 执行
- [ ] "重构登录模块" → TASK 模式 → 完整 Mission 流程

### 9.2 Worker 选择

- [ ] 无硬编码的 Worker 名称作为选择逻辑
- [ ] 所有 Worker 选择来自 ProfileLoader
- [ ] Worker 不可用时从画像配置获取替代

### 9.3 消息路由

- [ ] 主对话区只有编排者叙事
- [ ] Worker 输出只在对应 Tab
- [ ] 无重复/丢失消息

---

## 10. 版本历史

| 版本 | 日期 | 变更 |
|------|------|------|
| 1.0 | 2025-01-31 | 初始版本：完整编排流程规范 |


