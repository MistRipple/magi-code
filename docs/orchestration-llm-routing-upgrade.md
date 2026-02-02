# 编排升级方案：LLM 统一路由与分配（单一路径）

> 目的：彻底消除“意图理解 vs 规则匹配”错位，所有路由与分配由编排者 LLM 一次性输出；规则仅做验证与降级，不再参与决策。

---

## 1. 升级背景与问题

当前系统存在两层决策链：

1) **意图理解**由编排者 LLM 决定（ASK/DIRECT/EXPLORE/TASK/CLARIFY）。
2) **Worker 分配**由规则匹配/关键词/分类默认值决定（profile/category matcher）。

问题在于：
- LLM 输出的语义是“真实意图”，但规则匹配可能因关键词模糊命中错误分类。
- 结果就是**意图正确、分配错误**，表现为不该派 Worker 的任务被派发，或派错 Worker。

**结论**：这是一类结构性错误，不可能通过“补丁/兼容”完全解决。

---

## 2. 目标

- **单一路由链**：意图理解 + 分类 + Worker 选择由 LLM 一次性输出。
- **规则仅做约束**：验证合法性/可用性；不再做分类与分配决策。
- **主流程一致**：ASK 直答；需要 Worker 则进入 MissionOrchestrator；不允许双轨制/快速路径。

---

## 3. 统一路由协议（LLM 输出）

编排者 LLM 统一输出如下 JSON（必须为唯一决定）：

```json
{
  "mode": "ask|direct|explore|task|clarify",
  "needsWorker": true,
  "category": "architecture|bugfix|frontend|data_analysis|...|none",
  "worker": "claude|codex|gemini|orchestrator",
  "reason": "简短理由",
  "clarificationQuestions": []
}
```

**解释：**
- `mode`：意图处理模式。
- `needsWorker`：是否需要 Worker。
- `category`：任务类型（仅作为显示与后续验证）。
- `worker`：最终执行者。
- `clarificationQuestions`：仅 CLARIFY 时提供。

---

## 4. 单一路径执行规则（唯一正确路径）

### 4.1 ASK
- 编排者直接回答。
- 不创建 Mission。
- 不派 Worker。

### 4.2 DIRECT/EXPLORE/TASK
- 由 LLM 判断 `needsWorker`：
  - `false` → 编排者直答。
  - `true` → **进入 MissionOrchestrator 完整流程**（规划、分配、执行、总结）。

### 4.3 CLARIFY
- 由 LLM 提供澄清问题。
- 用户回答后再次由 LLM 决策。

---

## 5. 规则的角色（降级为约束/验证）

**规则不再用于决策，只用于验证：**

1) **合法性验证**
- `worker` 是否存在于配置中
- `category` 是否为已注册类型

2) **可用性验证**
- Worker 是否可连接
- 模型/配置是否可用

3) **失败降级策略（保留）**
- Worker 不可用时按既定降级策略替换执行者
- 降级只改变 worker，不改 mode/category

---

## 6. 预期效果

- **消除错配**：LLM 意图与执行一致，不再出现“理解正确/分配错误”。
- **主对话更干净**：ASK 类问题只由编排者回答。
- **用户操作更少**：AI 自己完成分类与分配。
- **流程可解释**：每次分配都附带 `reason`。

---

## 7. 代码落地原则

- **删除规则分配逻辑**：不再让 category-matcher/assignment-manager 参与决策。
- **保留约束验证**：仅用于校验与降级。
- **单一通道**：所有执行进入 MissionOrchestrator，无 QuickExecutor。
- **无兼容策略**：不存在旧逻辑分支。

---

## 8. 验收标准

- “你是谁/画流程图”只走编排者回答。
- Direct/Explore 只有在 LLM 输出 needsWorker=true 时进入 Mission。
- Worker 分配与意图理解一致，不再出现“关键词误派”。
- 配置规则仅影响合法性和降级，不影响分类。

---

## 9. 风险与控制

**风险**：LLM 输出非 JSON 或不合法字段。  
**控制**：强制 JSON 输出 + 失败重试；超过重试即报错。  
**不允许**：回退到硬编码规则进行决策。

---

## 10. 总结

该升级方案将“意图理解 + 任务分配”统一为**编排者 LLM 单点决策**，规则仅作为约束存在，彻底消除规则匹配带来的语义偏航问题，并与产品定位（AI 自动执行、用户操作最少）完全一致。

