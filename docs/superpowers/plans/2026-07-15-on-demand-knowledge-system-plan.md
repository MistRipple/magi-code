# Magi 按需知识系统实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 ADR、FAQ、Learning 收敛为按需检索能力，普通主线和任务执行共享检索规则，并验证无需求时零知识上下文开销。

**Architecture:** 业务知识 tokenizer、查询评分和类型化结果投影继续归 `magi-knowledge-store`，避免工具层依赖上下文层。`magi-context-runtime` 新增本地确定性的意图判定和按需选择器；主线与任务执行分别接入该选择器。事件总线记录知识能力决策，前端把已有运行态字段投影为知识使用诊断。

**Tech Stack:** Rust workspace、Axum、Serde、Svelte、现有 `KnowledgeStore`、现有 `ContextRuntime`、现有事件总线。

## Global Constraints

- 不引入向量数据库、Embedding 服务或新的外部模型依赖。
- 不修改或覆盖其他 agent 当前未提交的改动。
- 普通闲聊和无关任务不查询知识库，不注入知识内容。
- 所有自动查询必须绑定已注册 workspace，禁止默认 workspace 回退和跨 workspace 泄漏。
- 不改变代码索引 tokenizer；只修改业务知识检索 tokenizer。
- 自动经验只生成 Learning；ADR 和 FAQ 仍需用户明确写入。
- 每个生产行为先写失败测试，再写实现。

---

### Task 1: 修复业务知识中文召回和评分

**Files:**
- Modify: `/Users/xie/code/magi-rust-rewrite/crates/magi-knowledge-store/src/normalization.rs`
- Modify: `/Users/xie/code/magi-rust-rewrite/crates/magi-knowledge-store/src/indexer.rs`
- Modify: `/Users/xie/code/magi-rust-rewrite/crates/magi-knowledge-store/src/query.rs`
- Modify: `/Users/xie/code/magi-rust-rewrite/crates/magi-knowledge-store/src/lib.rs`
- Test: `/Users/xie/code/magi-rust-rewrite/crates/magi-knowledge-store/src/tests.rs`

**Interfaces:**
- 保持 `KnowledgeStore::query` 和 `KnowledgeStore::governed_query` 的公开签名兼容。
- 新增业务知识字段命中信息，使标题、正文和标签可以分别计分。
- 保留 `KnowledgeMatch.matched_terms`，新增字段使用 `#[serde(default)]`。

- [ ] **Step 1: 写失败测试**

新增测试覆盖：

```rust
#[test]
fn business_knowledge_query_matches_natural_chinese_phrases() {
    let store = KnowledgeStore::new();
    store.upsert(record(
        "faq-refresh-token",
        KnowledgeKind::Faq,
        "登录失败后如何刷新令牌",
        "先刷新令牌，再重试原请求。",
    ));

    let result = store.query(&KnowledgeQuery {
        kind: None,
        text: Some("登录失败时怎么刷新令牌".to_string()),
        tags: vec![],
        workspace_id: None,
        limit: 5,
    });

    assert_eq!(result.total_matches, 1);
    assert_eq!(result.matches[0].record.knowledge_id, "faq-refresh-token");
}
```

同时增加标题命中高于正文命中、标签命中可补充召回、workspace 不串库的失败测试。

- [ ] **Step 2: 运行测试确认失败**

运行：

```bash
cargo test -p magi-knowledge-store business_knowledge_query_matches_natural_chinese_phrases -- --nocapture
```

预期：当前连续中文 tokenizer 无法命中自然问句，测试失败。

- [ ] **Step 3: 实现最小业务 tokenizer**

在 `normalization.rs` 中让中文连续片段同时产生完整片段、二字窗口和三字窗口；英文、数字和标识符沿用现有切分。`indexer.rs` 统一使用该 tokenizer 为 title、content、tags 建立带字段来源的索引项。

- [ ] **Step 4: 实现字段加权和覆盖率评分**

在 `query.rs` 中以标题、标签、正文分别计算命中，要求至少一个查询词命中；按覆盖率、字段权重和知识类型权重排序，同分再按更新时间和 ID 排序。

- [ ] **Step 5: 运行知识库测试**

运行：

```bash
cargo test -p magi-knowledge-store
```

预期：新增测试与既有测试全部通过。

---

### Task 2: 增加按需意图判定和类型化选择器

**Files:**
- Create: `/Users/xie/code/magi-rust-rewrite/crates/magi-context-runtime/src/knowledge_context.rs`
- Modify: `/Users/xie/code/magi-rust-rewrite/crates/magi-context-runtime/src/lib.rs`
- Test: `/Users/xie/code/magi-rust-rewrite/crates/magi-context-runtime/src/lib.rs`

**Interfaces:**

```rust
pub enum KnowledgeConsumer { Mainline, TaskExecution, KnowledgeQueryTool }
pub enum KnowledgeDecision { NotNeeded, MissingWorkspace, QueriedNoMatch, MatchedNotInjected, Injected }
pub struct KnowledgeContextRequest { pub consumer: KnowledgeConsumer, pub workspace_id: Option<WorkspaceId>, pub query: String }
pub struct KnowledgeContextSelection { pub decision: KnowledgeDecision, pub results: Vec<GovernedKnowledgeOutput>, pub query_terms: Vec<String>, pub injected_chars: usize, pub truncated: bool }
pub fn select_on_demand(&self, request: KnowledgeContextRequest) -> KnowledgeContextSelection;
pub fn render_for_prompt(selection: &KnowledgeContextSelection) -> Option<String>;
```

- [ ] **Step 1: 写失败测试**

覆盖以下行为：闲聊返回 `NotNeeded` 且结果为空；无 workspace 返回 `MissingWorkspace`；架构问题触发 ADR；故障问题触发 FAQ；实现任务触发 Learning；选择结果按类型数量和字符预算截断。

- [ ] **Step 2: 运行测试确认失败**

运行：

```bash
cargo test -p magi-context-runtime knowledge_context -- --nocapture
```

预期：模块和选择器接口不存在，测试无法通过。

- [ ] **Step 3: 实现本地意图判定**

使用确定性的中文和英文关键词组判定 `architecture`、`faq`、`learning` 三类意图；没有命中知识意图时直接返回 `NotNeeded`，不得调用 `KnowledgeStore`。

- [ ] **Step 4: 实现统一选择和渲染**

为主线和任务设置统一默认预算，按 ADR/FAQ/Learning 的类型优先级和结果数量限制选择；渲染时保留 ADR/FAQ 的完整必要内容，不再统一使用 96 字摘要。输出加入“仅作参考，不覆盖当前任务事实”的边界。

- [ ] **Step 5: 运行上下文测试**

运行：

```bash
cargo test -p magi-context-runtime
```

---

### Task 3: 将按需选择器接入普通主线

**Files:**
- Modify: `/Users/xie/code/magi-rust-rewrite/crates/magi-conversation-runtime/src/task_execution_dispatcher.rs`
- Modify: `/Users/xie/code/magi-rust-rewrite/crates/magi-conversation-runtime/src/session_turn_execution.rs`
- Modify: `/Users/xie/code/magi-rust-rewrite/crates/magi-conversation-runtime/src/prompt_utils.rs`
- Test: `/Users/xie/code/magi-rust-rewrite/crates/magi-conversation-runtime/src/session_turn_execution.rs`
- Test: `/Users/xie/code/magi-rust-rewrite/crates/magi-conversation-runtime/src/task_execution_dispatcher.rs`

**Interfaces:**
- `LlmTaskDispatcher::execute_session_turn` 在已有 `ContextRuntime` 和 workspace scope 下调用选择器。
- `SessionTurnExecutionRuntime` 接收可选的当前轮知识参考片段，不改变用户原始 prompt 和历史持久化内容。

- [ ] **Step 1: 写失败测试**

新增主线测试：普通“你好”不会调用知识选择器；“为什么采用这个架构”会在模型请求中出现知识参考系统消息；知识参考不会写入 canonical history；无 workspace 不会查询其他 workspace。

- [ ] **Step 2: 运行测试确认失败**

运行：

```bash
cargo test -p magi-conversation-runtime session_turn -- --nocapture
```

- [ ] **Step 3: 接入当前轮系统片段**

由 dispatcher 根据 `request.prompt` 和 `request.workspace_id` 调用选择器；将渲染结果作为 `PromptFragmentKind::KnowledgeContext` 系统消息传入 `build_session_turn_messages`。不触发时保持现有消息数组完全不增加知识片段。

- [ ] **Step 4: 运行主线测试**

运行：

```bash
cargo test -p magi-conversation-runtime session_turn_execution
```

---

### Task 4: 将同一选择器接入任务执行

**Files:**
- Modify: `/Users/xie/code/magi-rust-rewrite/crates/magi-conversation-runtime/src/task_execution_dispatcher.rs`
- Test: `/Users/xie/code/magi-rust-rewrite/crates/magi-conversation-runtime/src/task_execution_dispatcher.rs`

- [ ] **Step 1: 写失败测试**

验证普通文件读取任务的 `used_knowledge == 0`；架构修改任务命中 ADR；故障修复任务命中 FAQ/Learning；summary 中记录实际知识 ID；知识只作为参考而不覆盖 task facts。

- [ ] **Step 2: 运行测试确认失败**

运行：

```bash
cargo test -p magi-conversation-runtime assemble_prompt -- --nocapture
```

- [ ] **Step 3: 替换无条件 context 查询**

在 `assemble_prompt` 中先调用统一 selector；只有决策为 `Injected` 时才加入 `[reference:knowledge]` 内容；`NotNeeded` 不访问知识 store，summary 的 `used_knowledge` 保持 0。

- [ ] **Step 4: 运行任务上下文测试**

运行：

```bash
cargo test -p magi-conversation-runtime
```

---

### Task 5: 统一工具结果和自动经验质量治理

**Files:**
- Modify: `/Users/xie/code/magi-rust-rewrite/crates/magi-tool-runtime/src/builtin.rs`
- Modify: `/Users/xie/code/magi-rust-rewrite/crates/magi-conversation-runtime/src/task_execution_dispatcher.rs`
- Modify: `/Users/xie/code/magi-rust-rewrite/crates/magi-knowledge-store/src/lib.rs`
- Test: `/Users/xie/code/magi-rust-rewrite/crates/magi-tool-runtime/src/tests.rs`
- Test: `/Users/xie/code/magi-rust-rewrite/crates/magi-conversation-runtime/src/task_execution_dispatcher.rs`

- [ ] **Step 1: 写失败测试**

验证 `knowledge_query` 使用与自动上下文相同的中文召回和类型化字段；自动抽取过滤纯工具步骤、同一会话最多写入 3 条，并对中文近义经验去重；辅助模型失败产生可观察计数而不是伪造成功。

- [ ] **Step 2: 运行测试确认失败**

运行：

```bash
cargo test -p magi-tool-runtime knowledge_query
cargo test -p magi-conversation-runtime extract
```

- [ ] **Step 3: 接入统一投影并收紧抽取**

工具调用复用知识 store 的结果投影；自动经验写入前应用质量过滤、近义去重和 3 条上限；记录来源 session 和抽取失败原因。

- [ ] **Step 4: 运行工具与抽取测试**

运行：

```bash
cargo test -p magi-tool-runtime
cargo test -p magi-conversation-runtime
```

---

### Task 6: 完成知识使用诊断和 workspace 绑定验收

**Files:**
- Modify: `/Users/xie/code/magi-rust-rewrite/crates/magi-event-bus/src/read_model.rs`
- Modify: `/Users/xie/code/magi-rust-rewrite/web/src/shared/bridges/rust-daemon-contract.ts`
- Modify: `/Users/xie/code/magi-rust-rewrite/web/src/types/message.ts`
- Modify: `/Users/xie/code/magi-rust-rewrite/web/src/components/RuntimeStatePanel.svelte`
- Modify: `/Users/xie/code/magi-rust-rewrite/crates/magi-api/src/dto/bootstrap.rs` if bootstrap projection needs the new trace fields
- Test: corresponding Rust read-model tests and `web/scripts` golden/check scripts

- [ ] **Step 1: 写失败测试**

验证 `not_needed`、`missing_workspace`、`queried_no_match`、`matched_not_injected`、`injected` 五种状态从后端事件到前端 `opsView.knowledgeAudit` 的字段映射完整。

- [ ] **Step 2: 运行测试确认失败**

运行：

```bash
cargo test -p magi-event-bus knowledge
npm --prefix web run check
```

- [ ] **Step 3: 接入事件和前端投影**

扩展现有 execution overview 或知识审计事件，保留 consumer、decision、知识 ID、类型、匹配数、注入数、字符数和截断状态；前端只显示公开诊断字段，不显示知识正文或敏感路径。

- [ ] **Step 4: 运行全量验证**

运行：

```bash
npm --prefix web run check
cargo test -p magi-knowledge-store -p magi-context-runtime -p magi-conversation-runtime -p magi-tool-runtime -p magi-event-bus -p magi-api
```

---

### Task 7: 真实 daemon 运行态验收

**Files:**
- No production file changes unless a verified failure requires a focused fix.

- [ ] **Step 1: 启动 daemon 托管入口**

运行：

```bash
./scripts/dev-daemon.sh
```

- [ ] **Step 2: 注册当前 workspace 并读取 scope**

通过 `/api/workspaces/register` 注册 `/Users/xie/code/magi-rust-rewrite`，从 `/api/workspaces` 读取权威 `workspaceId`，再用该 ID 创建或选择 session；禁止手工猜测 ID。

- [ ] **Step 3: 写入三类验证数据**

通过 `/api/knowledge/items` 写入一条 ADR、一条 FAQ 和一条 Learning，均绑定真实 workspace；读取 `/api/knowledge` 验证三类数量和 workspace scope。

- [ ] **Step 4: 验证零开销场景**

发送普通问候和无关任务，检查运行诊断为 `not_needed`，知识查询次数为 0，知识 ID 为空。

- [ ] **Step 5: 验证按需命中场景**

分别发送架构决策、FAQ 故障和经验复用问题，检查返回知识类型、知识 ID、注入字符数和前端知识使用诊断。

- [ ] **Step 6: 验证 workspace 隔离**

创建第二个临时 workspace，写入同名但不同内容的知识，分别查询两个 workspace，确认结果不交叉。

- [ ] **Step 7: 记录最终证据**

保留测试命令结果、API JSON 摘要和运行诊断摘要；只有全部验收项通过后才将目标标记完成。
