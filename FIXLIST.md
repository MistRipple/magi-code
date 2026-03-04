# Magi 修复清单（核验版）

> 最后更新：2026-03-03  
> 核验范围：当前 `main` 工作区代码（非历史快照）  
> 结论统计：10 条中「成立 9」「部分成立 0」「已过时 1」

---

## 🔴 P0 — 致命 / 阻塞性

### #1 ✅ `buildTodoFingerprint` 空值崩溃（成立）

- **位置**：
  - `src/orchestrator/worker/worker-session.ts:132`
  - `src/orchestrator/worker/worker-session.ts:255`
  - `src/orchestrator/worker/autonomous-worker.ts:1066`
- **现状问题**：`content` 直接 `.trim()`，缺少空值防御。
- **修复目标**：
  - 指纹构建统一改为安全归一化函数（`typeof content === 'string'` 校验）。
  - 空内容不参与指纹匹配，写日志并跳过。
- **验收标准**：
  - 构造 `content=undefined/null/''` 的 session 更新与恢复流程不崩溃。
  - 日志可追踪被跳过的空 Todo 内容，不出现 `Cannot read properties of undefined (reading 'trim')`。

---

### #2 ✅ Todo 状态双写不一致（成立）

- **位置**：`src/orchestrator/worker/autonomous-worker.ts:535`
- **现状问题**：`catch` 分支直接改 `currentTodo.status='failed'`，绕过 `todoManager.fail()`。
- **修复目标**：
  - 移除直接写状态；失败状态只允许通过 `todoManager.fail()` 进入持久层。
  - 本地对象状态必须由持久层回读结果同步，不做旁路写入。
- **验收标准**：
  - 异常路径后，内存状态与 repository 状态一致（均为 failed）。
  - 崩溃恢复后不出现“持久层 running、内存 failed”的分裂。

---

### #3 ✅ DispatchManager 上帝类膨胀（成立，已分层收口）

- **位置**：
  - `src/orchestrator/core/dispatch-manager.ts`（当前约 1998 行）
  - `src/orchestrator/core/dispatch-routing-service.ts`
  - `src/orchestrator/core/dispatch-resume-context-store.ts`
- **现状问题**：调度、恢复、路由、批次管理状态集中在单类，演进风险高。
- **修复目标**：
  - 按职责拆分为路由、调度、恢复三个组件（可分阶段落地）。
  - 保持外部 API 不变，先做内部模块化迁移。
- **验收标准**：
  - `dispatch-manager.ts` 仅保留编排入口与组合逻辑。
  - 新增组件具备独立单元测试或链路回归脚本。

---

## 🟠 P1 — 重要 / 可靠性风险

### #4 ✅ Assignment/Contract 状态机无防御检查（成立）

- **位置**：
  - `src/orchestrator/mission/assignment-manager.ts:409`
  - `src/orchestrator/mission/contract-manager.ts:300`
- **现状问题**：`validTransitions[current].includes(...)` 未防御非法状态 key。
- **修复目标**：
  - 在状态迁移前增加 `currentTransitions` 存在性校验。
  - 非法旧状态输出可诊断错误（含实体 ID、当前状态、目标状态）。
- **验收标准**：
  - 脏数据状态不会触发 `includes` 空引用崩溃。
  - 状态错误可被上层捕获并中止当前实体迁移，不拖垮整条编排链路。

---

### #5 ✅ MCP 连接健康管理不足（成立，原路径已修正）

- **正确位置**：`src/tools/mcp-manager.ts`（非 `src/mcp/mcp-manager.ts`）
- **现状问题**：`callTool` 失败后直接抛错；缺少自动重连与失活探测。
- **修复目标**：
  - 增加 server 健康状态机（`connected/degraded/disconnected`）。
  - `callTool` 在连接类错误时触发一次受控重连并重试。
  - 对外暴露最近失败原因与最后健康检查时间。
- **验收标准**：
  - MCP 服务重启后，无需手工 reload 即可恢复工具调用。
  - 失败卡片包含“重连尝试次数、最终失败原因”。

---

### #6 ✅ SharedContextPool 去重复杂度高（成立）

- **位置**：`src/context/shared-context-pool.ts:323`
- **现状问题**：`add()` 时全量 `findDuplicate + similarity` 扫描，规模上升后开销明显。
- **修复目标**：
  - 引入两级索引（`missionId+type` 分桶 + 内容哈希/前缀签名）。
  - 仅在候选桶内做相似度精比对。
- **验收标准**：
  - 50~200 条条目场景下新增耗时明显下降。
  - 去重准确率不低于现有实现，且无明显误合并。

---

### #7 ✅ Worker Lane 粗粒度串行（成立，已并发推进）

- **位置**：`src/orchestrator/core/dispatch-manager.ts:142`
- **现状问题**：同一 `WorkerSlot` 使用全局互斥，降低理论并行度。
- **修复目标**：
  - 引入“同 Worker 多 lane（受配额限制）”或“同 Worker 队列并发窗口”。
  - 保留冲突文件串行机制，避免改写竞态。
- **验收标准**：
  - 无文件冲突任务可并发推进。
  - 有冲突任务仍自动串行，不引入文件覆盖回归。

---

## 🟡 P2 — 次要 / 技术债

### #8 ❌ `parentCompletionLocks` 泄漏（已过时）

- **原结论状态**：已过时，不成立。
- **原因**：当前代码中不存在 `parentCompletionLocks` 字段；对应描述来自旧版本实现。
- **处理动作**：
  - 从修复优先级中移除。
  - 若后续重引入锁机制，需同步补充生命周期清理策略。

---

### #9 ✅ 缓存与持久层写入非原子（成立）

- **位置**：`src/todo/todo-manager.ts` `complete()` 等状态写入流程
- **现状问题**：先改对象再 `save`，失败时无补偿，可能污染内存态。
- **修复目标**：
  - 采用“先构建 nextState，不直接改原对象；save 成功后再替换缓存”。
  - 或引入事务式 `applyStateTransition` 封装，统一回滚策略。
- **验收标准**：
  - 模拟 repository.save 失败时，缓存与持久层状态保持一致。
  - 重试后可继续执行，不出现不可恢复状态分裂。

---

### #10 ✅ 错误静默吞没（成立，分场景治理完成）

- **位置**：
  - `src/orchestrator/worker/autonomous-worker.ts`（验收检查异常“视为通过”）
  - `src/orchestrator/core/dispatch-batch.ts:47`（回调保护性空 catch）
- **现状判断**：
  - `autonomous-worker` 的“视为通过”会掩盖可见失败，成立。
  - `dispatch-batch` 的空 catch 属取消链路保护，合理保留。
- **修复目标**：
  - 对验收检查异常增加降级等级与前端可见提示（非直接判通过）。
  - 对保护性空 catch 保留，但统一增加指标计数（便于观测）。
- **验收标准**：
  - 验收检查异常时，用户可见“检查降级”状态，不误判完全通过。
  - 取消链路稳定，不因回调异常中断主流程。

---

## 📊 核验汇总

| 优先级 | 条目数 | 成立 | 部分成立 | 已过时 |
|--------|--------|------|----------|--------|
| 🔴 P0 | 3 | 3 | 0 | 0 |
| 🟠 P1 | 4 | 4 | 0 | 0 |
| 🟡 P2 | 3 | 2 | 0 | 1 |
| **合计** | **10** | **9** | **0** | **1** |

---

## 执行结果（按原风险顺序）

1. `#1` 指纹空值防御（已完成）
2. `#2` Todo 失败状态单写收口（已完成）
3. `#4` 状态机防御校验（已完成）
4. `#9` Todo 状态写入原子化（已完成）
5. `#5` MCP 重连与健康状态（已完成）
6. `#6` ContextPool 去重索引化（已完成）
7. `#10` 错误分级治理（已完成）
8. `#7` Worker Lane 并发策略升级（已完成）
9. `#3` DispatchManager 职责拆分（已完成）
