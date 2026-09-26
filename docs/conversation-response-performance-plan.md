# Magi 对话响应链路性能开发与验收计划

> 本文只定义怎么测、怎么统计和什么时候可以关闭性能工作包；不保存当前 artifact 或当前通过数。
> 当前状态、证据路径和剩余缺口见[重构进度](conversation-response-core-architecture-progress.md)，职责边界和整体完成定义见[目标架构](conversation-response-core-architecture-redesign.md)。
>
> 任何性能收益都必须建立在终态、权限、幂等、恢复和真实副作用正确的前提上。旧 ledger 若使用「derive.v2」或缺少当前输入身份，必须先按本计划重新派生，不能与当前数据直接比较。

## 1. 测量边界

只测三段，不把它们相加成一个未经定义的“总延迟”：

| 层 | 起点和终点 | 说明 |
| --- | --- | --- |
| 提交控制面 | 「submit_received → accepted_response_sent」 | 输入校验、幂等和最小 durable accepted；不包含模型执行。 |
| 后台执行面 | 「accepted_response_sent → canonical_terminal」 | 准备、Provider、事件投递和 canonical 终态；按阶段拆开。 |
| 呈现面 | 「renderer_received → dom_painted」 | Renderer 接收、reducer、projection 和 DOM paint；使用页面局部时钟。 |

Provider TTFT、Magi 本地准备、事件传输和 Renderer 绘制分别统计。标题、分类、上下文压缩等 sidecar 必须按 query_source 分开，不能混入主 Turn。

性能采样不得跳过权限、Git、幂等、审计、恢复或取消 settlement；不得用 sleep、轮询、loading、bootstrap 刷新或删除失败样本制造结果。异常结果是 correctness evidence 和异常性能样本，不得混入正常成功统计。

## 2. 统一轨迹合同

### 2.1 身份、版本和来源

规范化 trajectory ledger 的每条记录至少包含：

```text
fixture
schema_version / derive_version
session_id / turn_id / request_id
query_source / execution_profile
phase / timestamp_ms / event_sequence*
payload_hash / input_hash? / source / outcome
settlement? / settlement_required?
```

当前派生版本由脚本固定为：

```text
schema_version = magi.trajectory.v1
derive_version = magi-trajectory-derive.v5
electron_renderer_derive_version = magi-electron-renderer-derive.v2
electron_context_compaction_derive_version = magi-electron-context-compaction-metrics.v1
performance_envelope_derive_version = magi-performance-envelope-derive.v1
permission_schema_version = magi.permission.v5
permission_derive_version = magi-permission-derive.v8
```

`event_sequence` 对后端 `event_bus` 和 `canonical_terminal` 是必填；`accepted`、Provider 阶段和 Electron 本地阶段使用同一 `turn_id` 关联，但本地阶段没有独立的 durable EventBus 序号时保留 `null`，不得用估算序号填充。Electron sample 仍必须保留该 outcome 所需的后端 durable 序号；正常样本包含 EventBus/终态序号，前置阻断样本至少包含终态序号，供同轮校验。

原始 evidence 可以使用 camelCase（如 trajectoryMode、expectedOutcome、inputHash、settlementRequired）；派生 ledger 统一为 snake_case。字段或校验规则改变时必须增加版本并更新 golden test，不能静默复用旧 artifact。

真实 daemon 和打包 Electron evidence 还必须记录 source_commit、worktree_fingerprint_sha256，以及适用时的 executable_sha256；打包 Electron 若比较多文件资源，还必须记录覆盖 `app.asar`、Web bundle 和 daemon payload 的 `app_artifact_sha256`，并验证采样期间稳定。只记录 branch、dirty 标记或二进制路径不足以绑定证据。工作区有其他 Agent 修改时，dirty=true 是事实，不是通过条件；关键是 before/after fingerprint 在同一采样内相等。

settlement 的含义必须写清观察层级：

- 普通成功 timing 只证明同一 Turn 有唯一 canonical_terminal，不自动证明 daemon/Electron 进程关闭 settlement。
- 取消、恢复、关闭闸门、异常样本或显式 settlement_required=true 必须记录已观察的 settlement。
- session_terminal_finalize_core_completed 只表示 finalizer core 完成，可能早于 Sink 发布 terminal；若要证明阻塞 Turn 已释放接纳槽，还要记录下一轮接纳检查。
- canonical_terminal_ignored 只表示迟到结果被终态闸门拒绝，不能当第二个终态。

### 2.2 阶段和主链路

原始日志可以保留更细的 stage；规范化 ledger 使用以下阶段：

| 层 | ledger phase | 原始 stage 示例 |
| --- | --- | --- |
| 后端 | accepted | accepted_response_sent |
| 后端 | provider_request | provider_request_started |
| 后端 | provider_raw_delta | provider_first_raw_delta |
| 后端 | provider_visible_delta | provider_first_delta |
| 后端 | event_bus | event_bus_first_event / event_bus_item_published |
| 后端 | canonical_terminal | canonical_terminal_published |
| 业务事实 | tool_call、tool_result、approval_requested、approval_resolved | Provider、工具和审批事件 |
| Electron | renderer_received | frontend_event_received |
| Electron | reducer_completed | reducer_completed |
| Electron | projection_completed | projection_completed |
| Electron | dom_painted | dom_painted |

主链路：

```text
submit_received
  -> accepted_persisted
  -> accepted_response_sent
  -> preparation_started/completed
  -> provider_request_started
  -> provider_first_raw_delta
  -> provider_first_visible_delta
  -> event_bus_first_event
  -> canonical_terminal
  -> renderer_received
  -> reducer_completed
  -> projection_completed
  -> dom_painted
```

provider_raw_delta 包括只有 tool-call 的首 chunk；provider_visible_delta 只表示首个可见 content/thinking。不能用可见首 delta 代替 raw 首 chunk，也不能把工具往返时间伪装成 Magi 本地开销。

必需阶段按 outcome 分支：

| 轨迹 | 必需阶段 | 规则 |
| --- | --- | --- |
| 正常后端 | accepted + provider_request + event_bus + canonical_terminal | raw/visible 按实际记录；正常结果必须有连续身份和序号。 |
| 已 dispatch 的异常后端 | accepted + provider_request + canonical_terminal | 缺少 raw、visible 或 EventBus 时记录缺席原因，不合成事件。 |
| 正常或异常 Electron | 对应后端阶段 + renderer_received + reducer_completed + projection_completed + dom_painted | 所有阶段关联同一 `turn_id`；后端 durable 阶段保留序号，Renderer 阶段使用页面局部时钟，不伪造 EventBus 序号；异常还须写 `expectedOutcome`。 |
| Provider 未 dispatch 的前置阻断 | accepted + canonical_terminal | trajectoryMode=abnormal、expectedOutcome=failed|blocked、providerDispatch=not_dispatched、非空 providerBlockReason、inputHash、settlementRequired=true 和已观察 settlement；不得出现 Provider 阶段。 |

若阻断 Turn 的唯一 EventBus envelope 同时是终态 envelope，event_bus 与 canonical_terminal 可以共享同一 event_sequence，但终态不能早于 EventBus。

### 2.3 派生和证据规则

1. 主 Turn 与标题、分类、压缩等 sidecar 按 query_source 分离；sidecar 不计入主 Turn 的 Provider TTFT 或 terminal。
2. trajectory 必须 append-only；正文、thinking、工具调用和工具结果被改写时失败或分段，不能只忽略传输元数据差异。
3. 同一 turn_id 只能有一个 canonical terminal；重复 request replay 可以再次观察同一终态，但不能派生第二个 Turn。
4. 正常、失败、取消、超时和 blocked 都要 flush 原始轨迹并写 outcome；缺阶段必须解释，不能补造。
5. 缺少身份、后端必需序号、源 fingerprint、必需 hash 或 settlement 时 fail-closed。Renderer 本地阶段的 `event_sequence=null` 只有在同一 sample 已有该 outcome 所需的后端 durable 序号且阶段顺序可校验时才允许；通过 ledger 只表示该声明切片通过，不扩大为完整场景矩阵。
6. Electron paired comparison 必须在同一 Turn sample 内同时验证后端阶段和 Renderer 四阶段，保留五个原始输入文件的 SHA-256 与 canonical input hash；后端使用 `sinceAcceptedMs`，Renderer 使用页面局部 `elapsedMs`，输出两组统计和非加和声明，不能把它们拼成总延迟。
7. 分层性能 envelope 可以绑定 daemon sidecar 与 Electron comparison 的输入 hash、场景映射、source identity 和时钟合同，但必须显式保留不同 fixture 的未配对场景；envelope 不是新的事实源，也不能制造 total latency。

### 2.4 性能 interval sidecar

trajectory ledger 保存事件事实；跨事件的局部耗时可以由同一 raw evidence/log 派生为 `magi.performance.v1` sidecar，但不能用 sidecar 替代 accepted、Provider、EventBus 或终态记录。sidecar 是有意分离的性能读模型，不再把 interval metric 重复写入事件 ledger；daemon 与 Electron 使用各自的 derive，均通过相同 raw evidence、canonical `input_payload_hash`、fixture、sample identity 和 source identity 关联。sidecar 必须保留 input evidence 的 canonical `input_payload_hash`（须等于 trajectory `inputs[].payload_hash`）以及 raw input/log 的 SHA-256；每条 metric 必须保留 `fixture`、`comparison_role`、`scenario`、`sample_index`、`session_id`、`turn_id`、`request_id`、`outcome` 和 source identity，并固定以下区间：

```text
accepted_response_sent -> provider_request_started
provider_request_started -> provider_first_raw_delta
accepted_response_sent -> provider_first_raw_delta
accepted_response_sent -> provider_first_delta
accepted_response_sent -> event_bus_item_published
accepted_response_sent -> canonical_terminal_published
```

`derive-magi-performance-metrics.mjs` 使用绝对 daemon 时间、每个正常场景 20 条样本和 nearest-rank 派生 P50/P95/最大值；它遇到缺失阶段、身份或 source 稳定性时失败，不从 `elapsed_ms` 估算跨阶段间隔。sidecar 与 trajectory ledger 必须用同一 raw evidence、fixture 和 sample identity 关联；两者统计不能相加为未定义的“总延迟”。

## 3. 指标、预算和采样

### 3.1 正确性先决条件

只有在终态唯一、内容可恢复一致、幂等冲突拒绝、Session 隔离、取消后真实 settlement，以及权限/Git/审批/恢复边界不退化时，延迟才可用于比较。先决条件失败时，延迟只作诊断。

### 3.2 候选预算

以下是需要用固定 before/after fixture 冻结的候选预算，不是当前达标声明：

| 指标 | 起点 → 终点 | 候选 P95 | 备注 |
| --- | --- | ---: | --- |
| conversation accepted | submit_received → accepted_response_sent | ≤ 200 ms | 控制面门槛。 |
| task accepted | submit_received → accepted_response_sent | ≤ 500 ms | 控制面门槛。 |
| conversation 本地准备 | accepted_response_sent → provider_request_started | ≤ 500 ms | 只约束本地段。 |
| task 本地准备 | accepted_response_sent → provider_request_started | ≤ 1,000 ms | 只约束本地段。 |
| 可计算的 Magi 首内容开销 | accepted_response_sent → provider_first_visible_delta，扣除 Provider TTFT | ≤ 300 ms | 首 raw 直接产生可见内容时才计算。 |
| Renderer 局部绘制 | renderer_received → dom_painted | ≤ 50 ms | 不包含 Provider 和 EventBus。 |

Provider 等待和完整 terminal 受上游、输出长度和任务复杂度影响，只作同口径观察和 before/after 比较。若 fixture 证明预算不合理，必须记录原因、版本和新基线，不能静默放宽。

Task 的本地准备预算包含 workspace snapshot、code context、execution materialization 和 checkpoint；这些步骤承载工具权限、Git/变更账本和恢复事实，不能为了满足候选延迟而跳过。若其候选预算持续超出，应优化不改变事实边界的实现，或按版本化证据重新基线。

统计口径：

- Provider TTFT 使用同一机器的 provider_request_started → provider_first_raw_delta；request-to-headers 另报。
- 只有 request、raw、visible 使用可比较时钟且首 raw 直接产生可见内容时，才计算 Magi 首内容开销；否则按阶段分别报告。
- 后端统一使用同一 accepted 基准的 sinceAcceptedMs；Renderer 使用 timing registry 的局部 elapsedMs，两者不可相加。
- P50/P95/最大值使用固定 nearest-rank。失败、空流、超时、取消、reconnect、restart、replay 和阻塞恢复单独报告，不混入正常成功 P50/P95。

### 3.3 采样规则

- 每个正常性能场景至少 20 个独立 Turn；不足 20 只能报告 correctness，不能称为稳定 P95。
- 异常、恢复、关闭和权限/Git/审批阻塞按声明切片采样。异常 evidence 必须有 trajectoryMode、expectedOutcome；需要证明 settlement 的切片还必须有 inputHash 和 settlement。
- 固定模型/参数、工具面、历史形态、fixture、源码/可执行文件 fingerprint、schema、derive 和统计方法；保留原始输入及 payload hash。
- baseline 无法产生当前合同要求的阶段时必须 fail-closed；不得用 null、估算值或后续版本的事件补造 before 阶段。失败 evidence 可以保留作诊断，但不进入 before/after 统计。
- `MAGI_PERF_CONTINUE_AFTER_SAMPLE_FAILURE=1` 只能让采样器在真实 settlement 后继续收集剩余样本；它不改变单条失败、缺阶段或最终 evidence 的失败状态，也不允许把失败样本纳入统计。
- 空流/idle timeout 可以使用可重复 HTTP fault-injection fixture，但只能证明 transport 错误归一化、Coordinator 收口和声明的 settlement，不能证明上游 Provider 业务可用性。
- 上下文压缩 fixture 必须把 `context_compaction=completed` 与合法的 `context_compaction=skipped` 区分：连续 Turn 在历史已经满足预算时可以 skipped；`running` 只是中间事实，不能计作样本终态。每条样本都必须有 completed/skipped 终态事实，且整个 fixture 至少有一条 completed，才能关闭真实压缩 correctness。daemon 专项固定为新建 Chat 20 条 + 长历史 20 条；Electron 专项固定为同一 Session 预填充历史后的长历史 20 条。达到样本数且专用 derive 输出固定 schema、保留 input/source identity 时，可以报告该 fixture 的专项 P50/P95；只有存在同口径配对 baseline，才能形成 before/after 或性能收益结论。
- 审批取消必须证明唯一 turn.cancelled、无 approval.resolved、无副作用以及 settlement 后下一轮可接纳；取消不是 deny/allow 决定。
- 审批过期必须证明 tool_approval_expired、唯一 failed terminal、无副作用；不要求 Provider 再生成成功文本。
- 观察超时是采样控制参数，不是性能指标；复杂 Task/Agent 可以延长观察窗，但取消后的 settlement 必须单独记录。

## 4. 验收顺序与性能退出条件

当前每个工作包的状态只见[重构进度](conversation-response-core-architecture-progress.md)，本节不复制 artifact。

1. 将 D、E、F 的真实入口 correctness 作为性能前置，覆盖正常、异常、取消、恢复和权限/Git/审批边界；主 Turn 与 sidecar 分开。
2. 对 F，异常和 recovery evidence 必须把同一 `turn_id` 的后端、Renderer、outcome 以及所需 settlement 接入同一 ledger；timing-only 不能关闭 F。
3. D、E、F 关闭后，固定所有输入和版本，采集 before 与 after；before/after 必须使用同一 fixture、模型/参数、工具面、历史形态、ledger schema、derive 版本、采样轮数和 nearest-rank。
4. 后端 evidence 通过只关闭“后端轨迹可复算”这一子项；还必须从同一原始日志重算 accepted、局部准备、Provider TTFT 和适用的 Magi overhead，并用同口径 Renderer before/after 证据覆盖 DOM paint。
5. 只有前四步和正确性硬门槛通过后，才评估 CPU、分配、事件数量、长历史主线程或连接复用。细分指标不能替代核心链路。

G 只有同时满足以下条件才能关闭：

1. before/after 原始输入可独立复算，并有 hash、源码/可执行文件或多文件 App artifact 身份和版本；
2. 正常与声明异常场景都有 outcome；缺席阶段有原因，主 Turn/sidecar 不混淆；
3. accepted、本地准备、Provider TTFT、可计算的 Magi overhead、Renderer paint 的 P50/P95/最大值可重算，时间基准不混加；每个候选预算若未满足，必须在进度文档记录原因、版本、复测或重新基线决定，不能静默放宽；
4. 终态、内容、幂等、隔离、取消 settlement、恢复和真实副作用硬门槛通过；
5. 性能收益没有以权限、Git、审批、审计或恢复退化为代价。

同一 Electron evidence 内的 `source.before == source.after` 只证明该次采样稳定，不能代替架构 before/after；没有独立 baseline 时，DOM paint 只能作当前观察，G 保持未关闭。

同一限制也适用于 daemon sidecar：`source_commit` 相同而 worktree fingerprint、可执行文件或 App artifact 不同，可以证明两组输入可独立复算、采样期间身份稳定以及候选预算观察，但不能单独证明重构带来的因果收益。若要宣称“性能收益”，必须补充独立源码/可执行文件身份的 before baseline；在此之前，报告使用“可复算 before/after 观察”和“预算满足”，不使用“整体性能提升”。

在 D、E、F、G 全部关闭前，不得宣称性能目标达标或整体架构完成。

## 5. 验证入口

### 5.1 复采与派生流程

性能复采必须使用新的 run-id、空闲端口和稳定的输入/source identity；不得覆盖旧 artifact。正常场景固定每类 20 个独立 Turn，失败或不完整 evidence 保留但不进入统计。最小流程如下：

```bash
MAGI_PERF_DAEMON_BIN=target/release/magi-daemon-app \
MAGI_PERF_PORT=<free-port> \
MAGI_PERF_FIXTURE=real-provider-performance-v1 \
MAGI_PERF_CONTINUE_AFTER_SAMPLE_FAILURE=1 \
MAGI_PERF_TURN_TIMEOUT_MS=300000 \
MAGI_PERF_SAMPLES=20 \
MAGI_PERF_EVIDENCE=/tmp/magi-real-provider-performance-after-<run-id>.json \
  node scripts/verify-real-provider-performance.mjs

node scripts/derive-magi-trajectory-ledger.mjs \
  --output /tmp/magi-trajectory-ledger-after-<run-id>.json \
  /tmp/magi-real-provider-performance-after-<run-id>.json
node scripts/derive-magi-performance-metrics.mjs \
  --role after \
  --input /tmp/magi-real-provider-performance-after-<run-id>.json \
  --output /tmp/magi-performance-metrics-after-<run-id>.json
# 只有 before evidence/sidecar 已通过同一合同后，才执行以下 compare。
node scripts/derive-magi-performance-metrics.mjs \
  --compare /tmp/magi-performance-metrics-before-<run-id>.json \
  /tmp/magi-performance-metrics-after-<run-id>.json \
  --output /tmp/magi-performance-comparison-<run-id>.json

# 上下文压缩专项：每条长历史样本必须有 completed/skipped，且至少一条 completed。
node scripts/derive-magi-context-compaction-metrics.mjs \
  --role after \
  --input /tmp/magi-real-provider-performance-compression-<run-id>.json \
  --output /tmp/magi-performance-metrics-context-compaction-after-<run-id>.json
# daemon 压缩 compare 要求 before/after 都是 20 条新建 Chat + 20 条长历史的通过输入。
node scripts/derive-magi-context-compaction-metrics.mjs \
  --compare /tmp/magi-performance-metrics-context-compaction-before-<run-id>.json \
  /tmp/magi-performance-metrics-context-compaction-after-<run-id>.json \
  --output /tmp/magi-performance-comparison-context-compaction-<run-id>.json

# Electron 上下文压缩专项：同一 Session 预填充历史后固定采集 20 条长历史 Turn。
# 该 evidence/sidecar 只能形成 Electron after-only 观察；独立 before 存在后再执行 compare。
MAGI_ELECTRON_DOM_CDP_PORT=<free-port> \
MAGI_ELECTRON_DOM_COMPACTION_ONLY=1 \
MAGI_ELECTRON_DOM_COMPACTION_SAMPLES=20 \
MAGI_ELECTRON_DOM_COMPACTION_PREFILL_TURNS=12 \
MAGI_ELECTRON_DOM_EVIDENCE_PATH=/tmp/magi-electron-context-compaction-after-<run-id>.json \
  npm run test:electron-conversation-dom
node scripts/derive-electron-context-compaction-metrics.mjs \
  --role after \
  --input /tmp/magi-electron-context-compaction-after-<run-id>.json \
  --output /tmp/magi-performance-metrics-electron-context-compaction-after-<run-id>.json
node scripts/derive-magi-trajectory-ledger.mjs \
  --output /tmp/magi-trajectory-ledger-electron-context-compaction-after-<run-id>.json \
  /tmp/magi-electron-context-compaction-after-<run-id>.json
# 只有 before evidence/sidecar 已通过同一合同后，才执行以下 compare。
node scripts/derive-electron-context-compaction-metrics.mjs \
  --compare /tmp/magi-performance-metrics-electron-context-compaction-before-<run-id>.json \
  /tmp/magi-performance-metrics-electron-context-compaction-after-<run-id>.json \
  --output /tmp/magi-performance-comparison-electron-context-compaction-<run-id>.json
```

Renderer 的五类输入先分别完成 20 轮，再由 `derive-electron-renderer-comparison.mjs` 生成 before/after comparison；该脚本同时重算同一 Turn 的 backendStats、Renderer stats、原始输入 hash 和 before/after delta，随后分别用 trajectory derive 校验其后端阶段、Renderer 阶段和 outcome。before/after 的完整命令可按该脚本的 `--help` 及当前进度中的证据命名执行；不要把 daemon 与 Electron 两组 fixture 的指标相加。

完成 daemon sidecar 与 Electron comparison 后，可生成分层 envelope：

```bash
node scripts/derive-magi-performance-envelope.mjs \
  --daemon-before /tmp/magi-performance-metrics-before-<run-id>.json \
  --daemon-after /tmp/magi-performance-metrics-after-<run-id>.json \
  --electron-before /tmp/magi-electron-dom-renderer-before-<run-id>.json \
  --electron-after /tmp/magi-electron-dom-renderer-after-<run-id>.json \
  --electron-comparison /tmp/magi-electron-dom-renderer-cross-layer-comparison-<run-id>.json \
  --output /tmp/magi-performance-envelope-<run-id>.json
```

### 5.2 最小验证命令

按改动面执行最小充分检查；只有整体审计或发布前才运行全量命令。文档、ledger 或采样脚本改变时至少执行：

```bash
node --check scripts/derive-magi-trajectory-ledger.mjs
node --check scripts/derive-electron-renderer-comparison.mjs
node --check scripts/derive-electron-renderer-comparison-golden.mjs
node --check scripts/derive-magi-performance-metrics.mjs
node --check scripts/derive-magi-context-compaction-metrics.mjs
node --check scripts/derive-electron-context-compaction-metrics.mjs
node --check scripts/derive-magi-context-compaction-metrics-golden.mjs
node --check scripts/derive-electron-context-compaction-metrics-golden.mjs
node --check scripts/derive-magi-trajectory-ledger-golden.mjs
npm run test:electron-renderer-comparison
npm run test:context-compaction-metrics
npm run test:electron-context-compaction-metrics
npm run test:trajectory-ledger
node --check scripts/derive-magi-permission-ledger.mjs
node --check scripts/derive-magi-permission-ledger-golden.mjs
npm run test:permission-ledger
node --check scripts/verify-real-provider-performance.mjs
node --check scripts/verify-real-provider-restart-replay.mjs
node --check scripts/verify-electron-conversation-dom.mjs
node --check scripts/verify-electron-browser-permission-matrix.mjs
git diff --check
```

涉及 Rust/API/daemon 的实现时补充：

```bash
cargo fmt --all -- --check
cargo check -p magi-daemon -p magi-api -p magi-conversation-runtime -p magi-bridge-client
```

涉及协议、Web 或打包 Electron 时补充：

```bash
npm run protocol:check
npm --prefix web run check
npm --prefix web run build
npm run test:electron-conversation-dom
npm run desktop:package -- --dir
```

整体审计再执行：

```bash
cargo test --workspace --all-targets --quiet -- --test-threads=1
npm test
```

派生命令：

```bash
node scripts/derive-magi-trajectory-ledger.mjs \
  --output /tmp/magi-trajectory-ledger-<run-id>.json \
  /tmp/magi-real-provider-<slice>-<run-id>.json

node scripts/derive-magi-permission-ledger.mjs \
  --output /tmp/magi-permission-ledger-<run-id>.json \
  /tmp/magi-permission-evidence-<run-id>.json
```

每次采样使用新的 run-id 和空闲端口，不覆盖旧 artifact。失败或不完整 artifact 保留作诊断，但不得计入关闭统计；实际采样入口和当前缺口只记录在[重构进度](conversation-response-core-architecture-progress.md)。
