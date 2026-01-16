# Intent Gate 设计文档

> 版本: 1.0 | 日期: 2025-01-16

## 1. 设计背景

### 1.1 当前问题

用户输入直接进入任务分析流程，缺少意图判断：
- 用户问"你可以做什么" → 系统生成执行计划 → 弹出确认卡片 ❌
- 正确行为：识别为问答 → 直接回答 ✅

### 1.2 参考：Oh-My-OpenCode 的 Intent Gate

```
Phase 0 - Intent Gate (EVERY message)
├── Step 1: Classify Request Type
├── Step 2: Check for Ambiguity  
└── Step 3: Validate Before Acting
```

**核心原则**：NEVER START IMPLEMENTING, UNLESS USER WANTS YOU TO IMPLEMENT SOMETHING EXPLICITLY

## 2. 意图类型定义

| 类型 | 英文 | 信号特征 | 处理方式 |
|------|------|----------|----------|
| 问答咨询 | QUESTION | 问号、"是什么"、"为什么"、短文本 | 直接回答 |
| 简单操作 | TRIVIAL | 单文件、已知位置、简单修改 | 直接执行 |
| 明确任务 | EXPLICIT | 具体文件、明确命令、代码块 | 任务分析→执行 |
| 探索分析 | EXPLORATORY | "怎么工作"、"找到X"、"分析" | 探索→回答 |
| 开放需求 | OPEN_ENDED | "改进"、"重构"、"添加功能" | 评估→计划→执行 |
| 模糊请求 | AMBIGUOUS | 范围不清、多种解释 | **询问澄清** |

## 3. 分类规则

### 3.1 快速路径（规则匹配）

```typescript
// 明显问答
if (hasQuestionMark || hasQuestionKeyword && !hasTaskKeyword) {
  return IntentType.QUESTION;
}

// 明显任务
if (hasCodeBlock || hasFilePath || hasExplicitTaskKeyword) {
  return IntentType.EXPLICIT;
}

// 短文本无任务词
if (length <= 30 && !hasTaskKeyword) {
  return IntentType.QUESTION;
}
```

### 3.2 LLM 辅助分类（复杂情况）

当规则无法确定时，使用轻量级 LLM 调用进行意图分类。

## 4. 歧义检查规则

| 情况 | 行动 |
|------|------|
| 单一有效解释 | 继续执行 |
| 多种解释，工作量相似 | 继续，注明假设 |
| 多种解释，工作量差2倍+ | **必须询问** |
| 缺少关键信息（文件、错误、上下文） | **必须询问** |
| 用户设计有明显问题 | **提出关注** |

## 5. 处理路由

```
IntentType.QUESTION     → executeAskMode()      // 直接回答
IntentType.TRIVIAL      → executeDirectMode()   // 直接执行
IntentType.EXPLICIT     → executeTaskMode()     // 任务分析→执行
IntentType.EXPLORATORY  → executeExploreMode()  // 探索→回答
IntentType.OPEN_ENDED   → executeTaskMode()     // 评估→计划→执行
IntentType.AMBIGUOUS    → executeClarifyMode()  // 询问澄清
```

## 6. 架构集成

### 6.1 新增组件

```
src/orchestrator/
├── intent-gate.ts           # 🆕 意图门控
├── intent-classifier.ts     # 🆕 意图分类器
├── clarification-handler.ts # 🆕 澄清处理器
└── orchestrator-agent.ts    # 修改：集成 Intent Gate
```

### 6.2 执行流程

```
用户输入
    ↓
IntentGate.classify(prompt)
    ↓
┌─────────────────────────────────────┐
│ QUESTION → Ask Mode (直接回答)       │
│ TRIVIAL  → Direct Mode (直接执行)    │
│ EXPLICIT → Task Mode (任务分析)      │
│ EXPLORATORY → Explore Mode (探索)   │
│ OPEN_ENDED → Task Mode (计划执行)   │
│ AMBIGUOUS → Clarify Mode (询问澄清) │
└─────────────────────────────────────┘
    ↓
执行对应处理器
```

## 7. 与现有架构的关系

### 7.1 保留的组件
- OrchestratorAgent：编排者核心逻辑
- WorkerAgent：执行者
- TaskAnalyzer：任务分析（用于 EXPLICIT/OPEN_ENDED）
- RiskPolicy：风险评估

### 7.2 修改的组件
- IntelligentOrchestrator.execute()：入口处集成 Intent Gate
- TaskAnalyzer：增强 isQuestion 为完整的意图分类

### 7.3 新增的组件
- IntentGate：意图门控入口
- IntentClassifier：意图分类器
- ClarificationHandler：澄清对话处理

## 8. 实现优先级

1. **P0**：IntentClassifier 基础实现（规则分类）
2. **P1**：IntentGate 集成到 execute() 入口
3. **P2**：ClarificationHandler 澄清机制
4. **P3**：LLM 辅助分类（复杂情况）

