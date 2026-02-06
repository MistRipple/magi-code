# 上下文压缩格式参考

> 对比 Claude Code 的压缩格式与我们系统的 MemoryContent 结构，分析差异与优化空间
>
> **状态**：✅ 已完成升级

---

## 一、Claude Code 压缩格式（9 个章节）

```
1. Primary Request and Intent
   - 用户核心需求
   - 用户强调的原则/约束

2. Key Technical Concepts
   - 核心系统/模块
   - 技术术语

3. Files and Code Sections
   - 文件路径
   - 关键代码修改（含代码块）

4. Errors and fixes
   - 问题描述
   - 根因分析
   - 修复方案

5. Problem Solving
   - 解决了什么
   - 关键洞察

6. All user messages
   - 用户原话（保留原文）

7. Pending Tasks
   - 未完成的工作

8. Current Work
   - 最后在做什么

9. Optional Next Step
   - 下一步建议
   - 需要修改的文件列表
```

---

## 二、我们系统的 MemoryContent 结构（已升级）

```typescript
// src/context/types.ts
interface MemoryContent {
  // ========== 元数据 ==========
  sessionId: string;
  sessionName: string;
  created: string;
  lastUpdated: string;
  tokenEstimate: number;

  // ========== 用户意图（核心）==========
  primaryIntent: string;           // 用户核心需求（一句话）
  userConstraints: string[];       // 用户明确的约束条件
  userMessages: UserMessage[];     // 用户关键原话（保留原文）

  // ========== 任务状态 ==========
  currentTasks: TaskRecord[];      // 进行中的任务
  completedTasks: TaskRecord[];    // 已完成的任务
  currentWork: string;             // 当前正在做什么
  nextSteps: string[];             // 下一步建议

  // ========== 技术上下文 ==========
  keyDecisions: Decision[];        // 关键决策
  codeChanges: CodeChange[];       // 代码变更
  importantContext: string[];      // 重要上下文

  // ========== 问题跟踪 ==========
  pendingIssues: Issue[];          // 待解决问题
  resolvedIssues: ResolvedIssue[]; // 已解决问题及方案
  rejectedApproaches: RejectedApproach[]; // 被拒绝的方案
}
```

---

## 三、对比分析（升级后）

| Claude Code 章节 | 我们的字段 | 状态 |
|-----------------|-----------|------|
| Primary Request and Intent | `primaryIntent` + `userConstraints` | ✅ 已实现 |
| Key Technical Concepts | `importantContext` + `keyDecisions` | ✅ 已实现 |
| Files and Code Sections | `codeChanges` | ✅ 已实现 |
| Errors and fixes | `pendingIssues` + `resolvedIssues` | ✅ 已实现 |
| Problem Solving | `rejectedApproaches` | ✅ 已实现 |
| All user messages | `userMessages` | ✅ 已实现 |
| Pending Tasks | `currentTasks` | ✅ 已实现 |
| Current Work | `currentWork` | ✅ 已实现 |
| Optional Next Step | `nextSteps` | ✅ 已实现 |

---

## 四、新增的类型定义

### UserMessage - 用户消息记录
```typescript
interface UserMessage {
  content: string;           // 消息原文
  timestamp: string;         // 消息时间
  isKeyInstruction: boolean; // 是否为关键指令
}
```

### Issue - 待解决问题
```typescript
interface Issue {
  id: string;
  description: string;
  source: 'user' | 'system' | 'ai';
  timestamp: string;
}
```

### ResolvedIssue - 已解决问题
```typescript
interface ResolvedIssue {
  id: string;
  problem: string;      // 问题描述
  rootCause: string;    // 根因分析
  solution: string;     // 解决方案
  timestamp: string;
}
```

### RejectedApproach - 被拒绝的方案
```typescript
interface RejectedApproach {
  id: string;
  approach: string;                    // 方案描述
  reason: string;                      // 拒绝原因
  rejectedBy: 'user' | 'technical';   // 拒绝来源
  timestamp: string;
}
```

---

## 五、压缩优先级（已更新）

```typescript
retentionPriority: [
  'primaryIntent',       // 🔴 最重要：用户核心意图
  'userConstraints',     // 🔴 重要：用户约束条件
  'currentTasks',        // 🔴 重要：进行中的任务
  'currentWork',         // 🟡 中等：当前工作状态
  'keyDecisions',        // 🟡 中等：关键决策
  'userMessages',        // 🟡 中等：用户原话
  'nextSteps',           // 🟡 中等：下一步建议
  'rejectedApproaches',  // 🟡 中等：被拒绝的方案
  'importantContext',    // 🟢 次要：重要上下文
  'codeChanges',         // 🟢 次要：代码变更
  'pendingIssues',       // 🟢 次要：待解决问题
  'resolvedIssues',      // 🟢 次要：已解决问题
  'completedTasks'       // 🟢 最后：已完成任务（可压缩）
]
```

---

## 六、涉及的文件修改

| 文件 | 修改内容 |
|------|---------|
| `src/context/types.ts` | 新增 7 个字段 + 4 个接口 |
| `src/context/memory-document.ts` | 更新 `addPendingIssue`、`resolvePendingIssue`、`normalizeContent` |
| `src/context/context-compressor.ts` | 更新压缩提示词，支持新字段 |
| `src/orchestrator/core/mission-orchestrator.ts` | 适配 Issue 类型变更 |

