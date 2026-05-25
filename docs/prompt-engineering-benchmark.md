# 提示词工程横向对比基线（2026-05）

> 取证日期：2026-05-20
> 对比对象：claude-code 2.1.87（Anthropic 商业版）、codex（OpenAI Codex CLI）、本仓 magi-rust-rewrite（Phase 1–5 改造完成后）
> 用途：固化 Phase 5 完工时的横向位置，作为后续提示词改造的参照基线；不作为强制规范

## 一、取证方式

- 三方代码并行考察（claude-code、codex 由 Explore agent 取证；magi 由本仓直接核对 commit 后状态）
- 每条结论均附文件路径 / 行号 / 代码片段
- claude-code 是已发布的反编译/重构源码，部分内部状态不可知（标注为「证据有限」）

## 二、十个维度对比

| # | 维度 | claude-code 2.1.87 | codex (gpt-5.x) | magi (Phase 5 后) |
|---|---|---|---|---|
| 1 | 架构分层与缓存边界 | ★★★★★ 显式 `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` 常量 + 两层缓存 API | ★★★☆☆ 多模型 `.md` 分层，无显式 cache 边界 | ★★★★☆ 17 段 S1–S17 拼装 + 段间锚点常量；缓存边界**只到注释层** |
| 2 | 工具描述质量 | ★★★★☆ 36 个独立 prompt.ts，部分含反例但格式不统一 | ★★★☆☆ 多段但少反例对照 | ★★★★☆ 6 大核心工具统一三段式「何时用/何时不用/反例」 |
| 3 | 安全防御 | ★★★★★ 三层显式：通用 injection / 恶意代码 / 网络风险 | ★★☆☆☆ 无集中防御段，全靠 exec_policy 兜底 | ★★★★☆ `INJECTION_DEFENSE_BASELINE` 6 条 + SafetyGate 模式 |
| 4 | 角色 / persona | ★★☆☆☆ 单 agent + AgentTool 委托 | ★★★★☆ `personalities/` + `agents/` 双目录 md | ★★★★☆ 12 个 builtin-roles md + front-matter |
| 5 | 运行时注入信道 | ★★★★★ `<system-reminder>` 原生 + 系统提示词显式说明标签语义 | ★★★☆☆ 有运行时注入但非显式 system-reminder 风格 | ★★★★☆ `prompt_reminder.rs` 仿 system-reminder + 幂等保护 |
| 6 | 版本 / 模型变体 | ★★★☆☆ 模型 ID 矩阵，prompt 本身无版本字段 | ★★★★☆ 每模型一个 prompt 文件，无 version 字段化 | ★★★☆☆ role front-matter `version: 1`，prompt 变体未拆 |
| 7 | 多语言 | ★★★☆☆ `getLanguageSection()` 条件注入 | ★☆☆☆☆ 英文 only | ★★★★☆ 全中文一致性 |
| 8 | 可测试性 | ★★★☆☆ 无 prompt 拼装 snapshot 证据 | ★★★☆☆ 有 prompt_caching.rs 但 snapshot 不显式 | ★★★★☆ `insta` snapshots 覆盖 bridge-client / conversation-runtime |
| 9 | 文档化（外置 .md） | ★★☆☆☆ hardcoded TS 字符串（prompts.ts 集中 914 行） | ★★★★★ 全部外置 `.md` + `include_str!` | ★★★★★ 4 个 crate 用 `include_str!`，全外置 |
| 10 | 工程结构 | ★★★★☆ 三层集中 + 36 个 tool prompts | ★★★★☆ `templates/` 9 个子目录组织良好 | ★★★★☆ 多 crate 分散自管 |

## 三、综合评分（0–100，每项 10 分等权）

| 项目 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | **总分** |
|---|---|---|---|---|---|---|---|---|---|---|---|
| claude-code 2.1.87 | 9 | 8 | 9 | 5 | 9 | 6 | 6 | 6 | 5 | 8 | **71** |
| codex | 7 | 6 | 5 | 8 | 6 | 7 | 4 | 6 | 9 | 8 | **66** |
| **magi (Phase 5 后)** | 8 | 8 | 8.5 | 8.5 | 8 | 7 | 7 | 7 | 9 | 8 | **79** |

## 四、风格画像

- **claude-code**：安全与运行时机制标杆。缓存边界常量化、`<system-reminder>` 标签语义在系统提示词里向模型显式解释。短板是文档化（全 hardcoded）和无 persona 库。
- **codex**：文档化与多模型版本管理最强。每个模型一个 prompt 文件、persona 双目录组织。短板是安全防御薄弱、工具描述缺反例。
- **magi**：工程规范度最高，无明显短板，但单项天花板未触顶。

## 五、推到 80+ 的两个具体杠杆（**有触发条件，触发前不做**）

### 杠杆 1 · 缓存边界常量化

- **当前状态**：仅在 `conversation_loop.rs` 有注释 `// === 静态段落分界 ===`，无常量
- **触发条件**：magi 开始接入 Anthropic `cache_control` API（或等价的国产模型 prompt caching）
- **改造动作**：把段间注释升格为 `const PROMPT_CACHE_BOUNDARY: &str = "..."`，参与拼装并打到 `cache_control: {type: "ephemeral"}`
- **不做的理由（Phase 5 期）**：magi 当前**零 `cache_control` 消费者**，无消费者的常量是脚手架，违反 cn-engineering-standard
- **关联任务**：task #79（已 completed，结论为延后）

### 杠杆 2 · prompt-per-model 文件组织

- **当前状态**：系统提示词在所有模型间共用
- **触发条件**：开始针对特定模型给不同提示词（Claude 4.6 vs GPT-5.2 vs 国产模型）
- **改造动作**：把单一 prompt 拆成 `prompts/claude_4_6.md` / `prompts/gpt_5_2.md` / `prompts/default.md`，按 model_id 选取
- **不做的理由（Phase 5 期）**：无差异化需求，强拆只会增加加载逻辑复杂度

## 六、Phase 5 完工时的代码锚点

| 改造点 | 代码位置 |
|---|---|
| 17 段 S1–S17 拼装 + 锚点注释 | `crates/magi-conversation-runtime/src/conversation_loop.rs` |
| `INJECTION_DEFENSE_BASELINE` 6 条防御 | `crates/magi-conversation-runtime/src/task_execution_dispatcher.rs:1516` |
| 6 大核心工具三段式描述 | `crates/magi-tool-runtime/src/lib.rs` `BuiltinToolName::description()` |
| `<system-reminder>` 风格运行时注入 | `crates/magi-conversation-runtime/src/prompt_reminder.rs` |
| 12 个 builtin-roles + front-matter | `crates/magi-agent-role/assets/builtin-roles/*.md` |
| insta 快照测试 | `crates/magi-bridge-client/tests/snapshots/` `crates/magi-conversation-runtime/tests/snapshots/` |

## 七、复评触发条件

下次需要重新做这份对比的时机：

1. 接入 Anthropic prompt caching API 之后
2. 开始针对特定模型分化 prompt 之后
3. claude-code 或 codex 发布显著版本更新之后（如 claude-code 3.x、codex 引入新 persona 体系）
4. magi 引入新一类工具（例如 MCP 接入、远程代理）需要重新审视工具描述规范之后
