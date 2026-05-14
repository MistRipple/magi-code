# Task System v2 未决问题与风险

更新时间：2026-05-14
状态：设计稿

本文档记录 v2 设计中**尚未确定**的部分，以及落地过程中需要持续监控的风险。每项都标注了"延后决策"还是"必须 Slice X 前定"——Slice 编号见 `02-migration-plan.md::Slice 候选清单`。

## 1. 模型作为 Coordinator 是一次押注

**问题**：Coordinator 协调能力完全依赖模型自己读 CoordinatorPrompt 并按 prompt 行事。如果模型质量回退（API 切到弱模型、context 压缩损失指令记忆），Coordinator 行为会退化。

**已知 mitigation**：
- claude-code 实际运行验证了这条路径在 Claude Sonnet 4+ 上稳定可用
- Coordinator prompt 用 prompt cache，命中后语义稳定

**未决**：
- 是否在 Coordinator Conversation 中**强制锁定模型版本**（不允许该 Conversation 中途换模型）
- 弱模型 fallback 时是否**自动降级到非 Coordinator 模式**

**决策时点**：S7（CoordinatorPrompt + Task trait）落地前。

## 2. Mailbox 没有 pull 机制

**问题**：Mailbox 是 push 模型——外部信号入栈、Conversation 在 Turn 边界 drain。Conversation **不能主动询问** Mailbox "有没有人要回复我"。

这在 C 档可能成为问题：长跑的 worker 想知道是否有更新指令，目前只能等到自己的下一个 Turn 才知道。

**候选方案**：
- A) 保持现状，依赖 Turn 边界轮询（与 codex 一致）
- B) 引入 `mailbox.peek()` 工具，让模型主动查看（但破坏 Turn 边界的确定性）
- C) Coordinator 主动 `SendMessage` 取代 worker pull（语义更清晰）

**当前倾向**：A + C 组合。但保留 B 的可能性。

**决策时点**：S7 落地后观察实际行为，最晚 S11~S15（Tier 4 第一波）前定。

## 3. KnowledgeGraph 查询接口未定

**问题**：L18 KG 在设计中是"带版本的事实表"，但查询接口形态没确定：
- SQL-like？（结构化查询）
- 自然语言 + RAG？（向量检索）
- 两者并存？

每种选择有不同的代价：
- SQL 严格但模型可能不会用
- RAG 模型友好但召回不稳定

**已知 mitigation**：
- 先做结构化（symbol_map、decision_log、risk_register 都是表）
- 向量索引作为可选 Layer，trait 化，先内置 BM25，后插 faiss/qdrant

**决策时点**：S11~S15（Tier 4 第一波）落地前。

## 4. ValidationRunner 的增量 vs 全量

**问题**：C 档每次 Plan 节点完成都跑 Validation。Java→Python 重构跑完整测试套件可能 10+ 分钟。每节点都跑会拖慢整个 Mission。

**候选方案**：
- A) 跑全量（最稳但最慢）
- B) 节点声明影响范围，按范围跑（快但漏检概率高）
- C) 阶段末跑全量、节点内跑相关子集（折中）

**当前倾向**：C。

**决策时点**：S11~S15（Tier 4 第一波）落地前。

## 5. 并发未明确

**问题**：B 档明确允许多 Conversation 并发（Coordinator spawn 多 worker）。但：
- 同一 Mission 下能否并行多个 Plan 节点？
- 跨 Mission 能否并发？
- 单 magi 实例的 Conversation 上限？

**已知边界**：
- 一个 Conversation 内不能并发 Turn（不变式）
- SpawnGraph 限制最大深度（默认 3）

**未决**：上述三问。

**决策时点**：S7（CoordinatorPrompt）落地后、S11~S15 前。

## 6. 编排失败的 fallback 未定

**问题**：如果 Coordinator 自身崩溃（模型胡说八道、提示词 jailbreak），整个 Mission 进度可能损坏。

**候选方案**：
- A) 自动 Checkpoint，崩溃后回滚到最近 Checkpoint
- B) HumanCheckpoint 兜底——任何"看起来不对"的状态触发人审
- C) 双 Coordinator 互审（一个跑、一个 review，造价高）

**当前倾向**：A + B，不做 C。

**决策时点**：S11~S15（Tier 4 第一波）落地前。

## 7. Multi-Mission 隔离

**问题**：一台机器同时跑多个 C 档 Mission（Mission α 重构 Java→Python，Mission β 写新功能），如何隔离？

**已知边界**：
- Workspace 独立（不同 git worktree）
- KG 独立
- MEMORY.md 独立

**未决**：
- ProjectMemory（L14）跨 Mission 共享还是隔离？
- Permission 是 magi 全局还是 Mission 范围？

**决策时点**：S7（CoordinatorPrompt）落地后、S11~S15 前。

## 8. HumanCheckpoint UI 形态

**问题**：Mission 命中 HumanCheckpoint 时，用户在哪里、看到什么、怎么回复？

**当前空白**：
- CLI / Web UI / 桌面通知，哪些组合？
- 是否支持远程批准（移动设备）？
- "暂停"和"等待用户"在 UI 上的视觉区别？

**决策时点**：S16~S17（Checkpoint/HumanCheckpoint）落地前 + 前端配套设计。

## 9. Checkpoint diff/merge 语义

**问题**：两个 Checkpoint 之间的 KG / Plan 差异如何表达？用户能否在 UI 看到"两小时前到现在 KG 长了什么"？

**当前空白**：仅设计了 Checkpoint 是快照，没设计 diff。

**候选方案**：
- A) 不做 diff，仅做 "回滚到 Checkpoint X" 操作
- B) 做 KG 三向 diff（base / current / target）
- C) Plan 节点级 diff，KG 不 diff

**当前倾向**：A 先上线，B/C 看用户反馈再加。

**决策时点**：S16~S17 落地之后的迭代。

## 10. 模型成本预算

**未在设计中讨论但重要**：C 档 Mission 长跑数日意味着大量 token 消耗。

**当前空白**：
- 每个 Mission 是否有 token 预算上限？
- 超预算行为：暂停 / 降级 / 通知用户？
- 子代理 token 是否纳入 parent 预算？

**决策时点**：S7（CoordinatorPrompt）落地后、S11~S15 前。

## 风险总结

按严重性排序的设计风险：

| # | 风险 | 严重性 | 监控信号 |
|---|------|--------|----------|
| 1 | 模型 Coordinator 退化 | 高 | A/B test 同 prompt 不同模型成功率 |
| 6 | 编排失败无 fallback | 高 | 跑出 demo Mission 后人工注入故障观察恢复 |
| 4 | Validation 拖慢 Mission | 中 | demo Mission 端到端用时统计 |
| 9 | Checkpoint diff 缺失 | 中 | 用户反馈"想看进度变化"的频次 |
| 10 | Token 预算溢出 | 中 | demo Mission 实际 token 消耗 |
| 2 | Mailbox pull 缺失 | 低 | worker 主动询问场景的实际频次 |
| 8 | HumanCheckpoint UI 不完整 | 低 | UI 配套 PR 进度 |

## 决策时点汇总

- **S7（CoordinatorPrompt）前需定**：模型版本锁定（#1）
- **S7 后、S11~S15 前需定**：并发上限（#5）、Multi-Mission 隔离（#7）、Token 预算（#10）
- **S11~S15（Tier 4 第一波）前需定**：KG 查询接口（#3）、Validation 策略（#4）、编排 fallback（#6）
- **S16~S17（Checkpoint）前需定**：Checkpoint diff（#9）、HumanCheckpoint UI 形态（#8）
- **整体落地后迭代**：Mailbox pull（#2）
