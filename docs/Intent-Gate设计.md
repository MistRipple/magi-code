# Intent Gate 设计文档（AI 决策版）

> 版本: 2.0 | 日期: 2026-01-18

## 1. 设计背景

我们是“智能编排工具”。核心价值是让系统像“智能项目经理”：理解用户意图并选择最合适的流程路径，尽可能减少不必要的澄清与阻塞。

本版本不使用规则分类作为主流程，仅保留执行控制与安全约束；意图判断由 AI 决策完成。

## 2. 设计原则

1. **AI 主导意图选择**：ASK / DIRECT / EXPLORE / TASK / CLARIFY 由 AI 决策。
2. **少打断，多推进**：能分析就进入 TASK；只有无法推进时才 CLARIFY。
3. **澄清必须具体问题**：没有问题就不允许进入 CLARIFY。
4. **能力咨询直答**：能力/可行性类问题直接 ASK。
5. **置信度可观测**：记录置信度与决策理由，便于追踪体验问题。

## 3. 决策输出规范（JSON）

```json
{
  "intent": "question|trivial|exploratory|task|ambiguous|open_ended",
  "recommendedMode": "ask|direct|explore|task|clarify",
  "confidence": 0-1,
  "needsClarification": true/false,
  "clarificationQuestions": ["问题1","问题2"],
  "reason": "一句话原因"
}
```

约束：
- 能力/可行性询问 → 推荐 `ask`
- 有明确执行意图但信息不全 → 推荐 `task`，可将 `needsClarification=true`
- 只有在确实无法推进时才推荐 `clarify`
- `clarificationQuestions` 最多 3 个；不澄清则为空数组

## 4. 流程路由

```
用户输入
  ↓
AI Intent Decision (JSON)
  ↓
┌─────────────────────────────────────┐
│ ask      → executeAskMode()         │
│ direct   → 直接执行(轻量)           │
│ explore  → executeExploreMode()     │
│ task     → 任务分析 → 执行           │
│ clarify  → clarifying questions     │
└─────────────────────────────────────┘
```

## 5. 澄清策略

- **轻度澄清**：1 个问题，目标/范围/模块
- **重度澄清**：2–3 个问题，仅在 AI 明确无法推进时
- **不允许空澄清**：问题为空直接回退为 task

## 6. 组件关系

```
OrchestratorAgent
 ├─ IntentGate (AI Decision)
 ├─ TaskAnalyzer (仅用于 task 细分/分析)
 └─ ClarificationCallback (UI)
```

## 7. 兼容性策略

本版本为 **AI 决策主路径**，不保留规则分类的兼容入口。

