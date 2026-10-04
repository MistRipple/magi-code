# 持久日志的追加写入与压缩设计

本文覆盖 daemon 中两类只增不减的持久日志：审计/用量账本（P2-4）与会话 canonical 事件日志（P2-5）。两者的共同问题是写入或启动成本随历史总量线性增长；本设计让写入成本只与新增量相关，启动成本只与最近一次压缩之后的增量相关。

## 1. 审计/用量账本（P2-4）

### 1.1 现状与问题

- 内存：`InMemoryEventBus` 持有 `AuditUsageLedgerSnapshot`（`audit_entries` / `usage_entries` 两个按 sequence 递增的数组）。
- 磁盘：`state_root/audit-usage-ledger.json` 单文件。运行时维护每 5 秒检查一次 `pending_flush`，有新条目就把**整本账本**序列化后原子重写。账本已有数万条、数十 MB，一次刷盘就要整体序列化并写满文件，拖慢 daemon（心跳超时、工具调用卡顿）。
- 「重置执行统计」通过删除内存中的 `model.usage.recorded` 条目再整本重写实现。

### 1.2 目标结构

```
state_root/audit-usage-ledger/
  segment-00000000000000000001.jsonl
  segment-00000000000000052113.jsonl   ← 活动段（最后一个）
```

- **只追加**：每行一条 `{"category":"audit"|"usage", ...AuditUsageLedgerEntry}`。刷盘只把内存中 sequence 大于「已落盘水位」的条目按 sequence 顺序追加到活动段，然后 `sync_data`。水位与刷盘互斥锁放在一起，由事件总线持有；不再有整本序列化。
- **分段**：活动段超过 8 MiB 时，新条目写入以其首个 sequence 命名的新段。段是保留策略的删除单位。
- **保留**：保留最近 180 天。启动加载时，最新条目早于保留期的整段被删除，内存只装载保留下来的段。保留只在启动时执行；daemon 长时间运行期间内存仍只增，但单次刷盘成本与历史量无关。
- **崩溃语义**：追加写可能在最后一行中途中断。加载时只有**最后一段**允许出现不以换行结尾的残行，按 WAL 惯例截断到最后一个完整行；任何完整但无法解析的行、或非最后一段的残行，都视为损坏，拒绝以空账本继续（与现有状态文件的严格策略一致）。
- **重置执行统计**：不再删除历史条目，而是经事件总线发布一条用量事件 `usage.stats.reset`（与其他条目走同一写入路径）。执行统计只统计最后一次重置之后的 `model.usage.recorded`。会话用量观测（重启后回填预算）不受统计重置影响。

### 1.3 接口变化

| 位置 | 变化 |
| --- | --- |
| `magi-event-bus::ledger` | 新增按目录读写分段的函数：加载（含残行截断与保留删除）与追加。删除整本 `export_json` / `import_json` / `persist_to_path` / `load_from_path`。 |
| `InMemoryEventBus` | `set_audit_usage_ledger_persistence(dir)` 指向段目录；`refresh_audit_usage_ledger_persistence` 改为增量追加；`import_audit_usage_ledger_snapshot` 改名为 `restore_persisted_audit_usage_ledger`，同时把水位推进到已落盘的最大 sequence。删除 `export/import_audit_usage_ledger_json`、`persist/restore/reset_audit_usage_ledger`。 |
| `StateRepository` | `audit_usage_ledger_path()` 返回段目录；`load_audit_usage_ledger()` 从段目录加载。 |
| 设置 `reset_stats` | 改为发布 `usage.stats.reset` 后刷盘。 |

### 1.4 一次性数据迁移

已有安装存在 `state_root/audit-usage-ledger.json`。`StateRepository::load_audit_usage_ledger` 在段目录不存在、旧文件存在时，把旧文件内容写成第一个段，确认落盘后删除旧文件。迁移完成后只存在段目录这一种格式，运行路径不读取旧文件。

## 2. 会话 canonical 事件日志（P2-5）

### 2.1 现状与问题

- 每个会话的 `state_root/session-events/<session>/` 下，每次提交写一个事务文件 `{first:020}-{last:020}.json`，从不合并。
- `SessionConversationProjection::load` 每次都从第一个事务文件开始全量重放；启动时每个会话都会加载一次。历史一多，文件数与重放时间线性增长，可能超过 60 秒启动超时。

### 2.2 目标结构

```
state_root/session-events/<session>/
  checkpoint-00000000000000004096.json   ← 最近一次检查点
  00000000000000004097-00000000000000004102.json
  ...
```

- **检查点**：`CanonicalEventCheckpoint { schemaVersion, sessionId, lastEventSeq, canonicalTurns, acceptedSubmissions }`，表示重放到 `lastEventSeq`（总是某个事务的末尾）后的完整结果。`acceptedSubmissions` 原样携带，启动时的 accepted 合并与 task 收敛语义不变（合并按 `updated_at` 幂等）。
- **加载**：取序号最大的检查点作为起点，只重放 `first_event_seq > lastEventSeq` 的事务。完全被检查点覆盖的事务文件（截断中途崩溃的遗留）直接跳过；跨越检查点边界的事务视为损坏。
- **写入与截断**：`append_transaction` 写入事务后，如果自上一个检查点以来的事务数达到 128，则原子写入新检查点，再删除被它覆盖的事务文件和旧检查点。检查点先于删除落盘，因此任意时刻崩溃都不会丢事实；删除失败只记录告警，遗留文件在加载时被跳过、在下一次检查点时被删除，不影响本次已成功的提交。
- **会话身份读取**：`read_session_id_from_root` 只读取目录中排序第一个文件的 `sessionId` 字段，事务与检查点都带该字段。
- 事件目录仍是 canonical 事实的唯一权威源；检查点是该目录内部的压缩形式，`session-projections` 缓存与它的校验关系不变。

## 3. 验证

- 账本：分段追加只写增量、跨段轮转、残行截断、非末段损坏拒绝、保留删除、旧文件迁移、重置标记后的统计口径。
- 事件日志：达到阈值后生成检查点并删除被覆盖事务；从检查点加载结果与全量重放一致；截断中途崩溃（遗留被覆盖事务）仍能加载；跨边界事务被拒绝。
