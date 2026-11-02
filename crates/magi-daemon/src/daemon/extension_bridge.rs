//! ExtensionBus 与对话运行时工具管线的适配器。
//!
//! 对话循环在阻塞线程上同步执行工具；适配器用 daemon 的 tokio Handle 等待 ExtensionBus 的
//! 异步分发。没有任何扩展登记 `tool.call` / `tool.result` 时直接放行，不产生分发开销。

use magi_bridge_client::ChatMessage;
use magi_bridge_client::ChatToolCall;
use magi_conversation_runtime::{
    ExtensionRewriteProposal,
    extension_hooks::{
        AssembleView, CompactionDecision, CompactionView, ContributePhase, ContributeView,
        ContributionStability, ExtensionHooks, InputDecision, InputGuard, InputView,
        PromptContribution, SpawnDecision, SubagentSpawnView, ToolCallDecision, ToolHookScope,
        ToolResultView,
    },
};
use magi_event_bus::{EventEnvelope, InMemoryEventBus};
use magi_extension_host::bus::{ExtensionBus, HookContext, TraceOutcome, Verdict};
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Arc};
use tokio::runtime::Handle;

pub(super) struct ExtensionBusHooks {
    bus: Arc<ExtensionBus>,
    handle: Handle,
}

impl ExtensionBusHooks {
    pub(super) fn new(bus: Arc<ExtensionBus>, handle: Handle) -> Self {
        Self { bus, handle }
    }

    fn context(scope: &ToolHookScope<'_>, call: &ChatToolCall) -> HookContext {
        let mut ctx = HookContext::new(format!("inv_{}", call.id));
        ctx.owner = json!({
            "sessionId": scope.session_id,
            "taskId": scope.task_id,
            "stepId": scope.round,
            "workerId": scope.worker_id,
        });
        ctx.binding = json!({"workspaceId": scope.workspace_id});
        ctx
    }
}

impl ExtensionBusHooks {
    /// 只能阻止的 guard 钩子：改写尚未接线，扩展若提出改写按失败处理（fail-closed），
    /// 不会静默忽略用户启用的必经规则。
    pub(super) fn guard_only(
        &self,
        hook: &str,
        ctx: &HookContext,
        input: Value,
    ) -> Result<(), (String, String)> {
        let outcome = self
            .handle
            .block_on(self.bus.invoke(hook, ctx, input))
            .map_err(|error| ("extension-bus".to_string(), error.to_string()))?;
        match outcome.verdict {
            Verdict::Blocked { plugin, reason } => Err((plugin, reason)),
            Verdict::Proceed(_) => {
                match outcome
                    .trace
                    .iter()
                    .find(|entry| entry.outcome == TraceOutcome::Replaced)
                {
                    Some(entry) => Err((
                        entry.plugin.clone(),
                        format!("钩子 {hook} 的改写尚未支持，已按失败处理"),
                    )),
                    None => Ok(()),
                }
            }
        }
    }
}

impl InputGuard for ExtensionBusHooks {
    fn check<'a>(
        &'a self,
        view: InputView<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = InputDecision> + Send + 'a>> {
        Box::pin(async move {
            if !self.bus.has_hook("input") {
                return InputDecision::Proceed;
            }
            let mut ctx =
                HookContext::new(format!("inv_input_{}", view.request_id.unwrap_or("none")));
            ctx.owner = json!({"sessionId": view.session_id});
            ctx.binding = json!({"workspaceId": view.workspace_id});
            // 原始输入不改写：扩展只能放行或阻止；改写尚未支持，扩展若提出改写按失败处理。
            let input = json!({
                "raw": {"text": view.text, "imageCount": view.image_count},
                "kind": view.kind,
                "requestId": view.request_id,
            });
            match self.bus.invoke("input", &ctx, input).await {
                Err(error) => InputDecision::Blocked {
                    plugin: "extension-bus".into(),
                    reason: error.to_string(),
                },
                Ok(outcome) => match outcome.verdict {
                    Verdict::Blocked { plugin, reason } => {
                        InputDecision::Blocked { plugin, reason }
                    }
                    Verdict::Proceed(_) => outcome
                        .trace
                        .iter()
                        .find(|entry| entry.outcome == TraceOutcome::Replaced)
                        .map_or(InputDecision::Proceed, |entry| InputDecision::Blocked {
                            plugin: entry.plugin.clone(),
                            reason: "input 钩子的改写尚未支持，已按失败处理".into(),
                        }),
                },
            }
        })
    }
}

/// 给扩展看的历史视图的上限：单条正文与总量。超出部分以 `truncated`/`omitted` 如实标注。
const ASSEMBLE_TEXT_CAP_CHARS: usize = 8 * 1024;
const ASSEMBLE_TOTAL_CAP_BYTES: usize = 1024 * 1024;

fn assemble_input(view: &AssembleView<'_>) -> Value {
    let mut total = 0usize;
    let mut omitted = 0usize;
    let mut messages = Vec::new();
    for index in view.rewritable.clone() {
        let message = &view.messages[index];
        let full = message.content.as_deref().unwrap_or_default();
        let text: String = full.chars().take(ASSEMBLE_TEXT_CAP_CHARS).collect();
        total += text.len();
        if total > ASSEMBLE_TOTAL_CAP_BYTES {
            omitted += 1;
            continue;
        }
        messages.push(json!({
            "index": index,
            "role": message.role,
            "text": text,
            "truncated": text.chars().count() < full.chars().count(),
            "hasToolCalls": !message.tool_calls.is_empty() || message.tool_call_id.is_some(),
        }));
    }
    json!({
        "sessionId": view.session_id,
        "contextWindowTokens": view.context_window_tokens,
        "rewritable": {"from": view.rewritable.start, "to": view.rewritable.end},
        "messages": messages,
        "omitted": omitted,
    })
}

/// 把插件补丁里的 `rewrites` 解析成提案；形态不符的补丁整体忽略（advise 失败保留原生视图）。
fn parse_rewrites(
    plugin: &str,
    hook: &str,
    patch: &Value,
) -> Option<Vec<ExtensionRewriteProposal>> {
    let list = patch.get("rewrites")?.as_array()?;
    list.iter()
        .map(|entry| {
            let replacement = entry
                .get("replacement")?
                .as_array()?
                .iter()
                .map(|message| {
                    Some(ChatMessage {
                        input_request_id: None,
                        context_origin: Default::default(),
                        role: message.get("role")?.as_str()?.to_string(),
                        content: Some(message.get("text")?.as_str()?.to_string()),
                        images: Vec::new(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        provider_context: Vec::new(),
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(ExtensionRewriteProposal {
                plugin_id: plugin.to_string(),
                contribution_id: hook.to_string(),
                hook: hook.to_string(),
                from: usize::try_from(entry.get("from")?.as_u64()?).ok()?,
                to: usize::try_from(entry.get("to")?.as_u64()?).ok()?,
                replacement,
            })
        })
        .collect()
}

/// 把插件补丁里的 `fragments` 解析成提示贡献。形态不符的片段被忽略；稳定性、预算与内容规则
/// 由对话运行时的唯一接纳点（`accept_contributions`）校验。
fn parse_fragments(plugin: &str, digest: &str, patch: &Value) -> Vec<PromptContribution> {
    let Some(list) = patch.get("fragments").and_then(Value::as_array) else {
        tracing::warn!(%plugin, "prompt.contribute 补丁形态无效，已忽略");
        return Vec::new();
    };
    list.iter()
        .filter_map(|entry| {
            let stability = match entry.get("stability")?.as_str()? {
                "stable" => ContributionStability::Stable,
                "volatile" => ContributionStability::Volatile,
                _ => return None,
            };
            Some(PromptContribution {
                plugin_id: plugin.to_string(),
                package_digest: digest.to_string(),
                fragment_id: entry.get("id")?.as_str()?.to_string(),
                stability,
                priority: entry.get("priority").and_then(Value::as_i64).unwrap_or(0),
                text: entry.get("text")?.as_str()?.to_string(),
            })
        })
        .collect()
}

impl ExtensionHooks for ExtensionBusHooks {
    fn observe(&self, hook: &'static str, payload: Value) {
        self.bus.notify(hook, payload);
    }

    fn prompt_contribute(
        &self,
        scope: &ToolHookScope<'_>,
        view: &ContributeView<'_>,
    ) -> Vec<PromptContribution> {
        if !self.bus.has_hook("prompt.contribute") {
            return Vec::new();
        }
        let phase = match view.phase {
            ContributePhase::Start => "start",
            ContributePhase::Round => "round",
        };
        let mut ctx = HookContext::new(format!(
            "inv_contribute_{}_{phase}_{}",
            view.task_id, view.round
        ));
        ctx.owner = json!({
            "sessionId": scope.session_id,
            "taskId": scope.task_id,
            "stepId": view.round,
            "workerId": scope.worker_id,
        });
        ctx.binding = json!({"workspaceId": scope.workspace_id});
        let input = json!({
            "target": "task",
            "phase": phase,
            "round": view.round,
            "budget": {
                "maxFragmentChars": view.max_fragment_chars,
                "maxTotalChars": view.max_total_chars,
            },
        });
        let Ok(outcome) = self
            .handle
            .block_on(self.bus.invoke("prompt.contribute", &ctx, input))
        else {
            return Vec::new();
        };
        outcome
            .patches
            .iter()
            .flat_map(|(plugin, patch)| {
                let digest = self.bus.plugin_digest(plugin).unwrap_or_default();
                parse_fragments(plugin, &digest, patch)
            })
            .collect()
    }

    fn context_assemble(
        &self,
        scope: &ToolHookScope<'_>,
        view: &AssembleView<'_>,
    ) -> Vec<ExtensionRewriteProposal> {
        if !self.bus.has_hook("context.assemble") {
            return Vec::new();
        }
        let mut ctx = HookContext::new(format!("inv_assemble_{}_{}", scope.task_id, scope.round));
        ctx.owner = json!({
            "sessionId": scope.session_id,
            "taskId": scope.task_id,
            "stepId": scope.round,
            "workerId": scope.worker_id,
        });
        ctx.binding = json!({"workspaceId": scope.workspace_id});
        let Ok(outcome) = self.handle.block_on(self.bus.invoke(
            "context.assemble",
            &ctx,
            assemble_input(view),
        )) else {
            return Vec::new();
        };
        outcome
            .patches
            .iter()
            .filter_map(|(plugin, patch)| {
                let parsed = parse_rewrites(plugin, "context.assemble", patch);
                if parsed.is_none() {
                    tracing::warn!(%plugin, "context.assemble 补丁形态无效，已忽略");
                }
                parsed
            })
            .flatten()
            .collect()
    }

    fn context_rewrite_authorized(&self, plugin_id: &str) -> bool {
        self.bus.is_context_rewrite_authorized(plugin_id)
    }

    fn compaction_before(&self, view: &CompactionView<'_>) -> CompactionDecision {
        if !self.bus.has_hook("compaction.before") {
            return CompactionDecision::Proceed;
        }
        let mut ctx = HookContext::new(format!("inv_compaction_{}", view.session_id));
        ctx.owner = json!({"sessionId": view.session_id});
        let input = json!({
            "reason": view.reason,
            "compactedMessages": view.compacted_messages,
            "originalTokens": view.original_tokens,
            "summaryBudgetTokens": view.summary_budget_tokens,
            "source": view.source,
        });
        let Ok(outcome) = self
            .handle
            .block_on(self.bus.invoke("compaction.before", &ctx, input))
        else {
            return CompactionDecision::Proceed;
        };
        if let Verdict::Blocked { plugin, reason } = outcome.verdict {
            return CompactionDecision::Cancel { plugin, reason };
        }
        // 自定义摘要需要 contextRewrite 授权；未授权的提案被忽略（advise 保留原生摘要）。
        outcome
            .patches
            .iter()
            .filter(|(plugin, _)| self.bus.is_context_rewrite_authorized(plugin))
            .find_map(|(plugin, patch)| {
                patch.get("summary").and_then(Value::as_str).map(|summary| {
                    CompactionDecision::Summary {
                        plugin: plugin.clone(),
                        summary: summary.to_string(),
                    }
                })
            })
            .unwrap_or(CompactionDecision::Proceed)
    }

    fn subagent_spawn(
        &self,
        scope: &ToolHookScope<'_>,
        request: &SubagentSpawnView<'_>,
    ) -> SpawnDecision {
        if !self.bus.has_hook("subagent.spawn") {
            return SpawnDecision::Proceed;
        }
        let mut ctx = HookContext::new(format!("inv_{}_spawn", request.tool_call_id));
        ctx.owner = json!({
            "sessionId": scope.session_id,
            "taskId": scope.task_id,
            "stepId": scope.round,
            "workerId": scope.worker_id,
        });
        ctx.binding = json!({"workspaceId": scope.workspace_id});
        let input = json!({
            "role": request.role,
            "taskName": request.task_name,
            "goal": request.goal,
            "toolCallId": request.tool_call_id,
        });
        match self.guard_only("subagent.spawn", &ctx, input) {
            Ok(()) => SpawnDecision::Proceed,
            Err((plugin, reason)) => SpawnDecision::Blocked { plugin, reason },
        }
    }

    fn tool_call(&self, scope: &ToolHookScope<'_>, call: &ChatToolCall) -> ToolCallDecision {
        let proceed = ToolCallDecision::Proceed {
            arguments: None,
            rewritten_by: Vec::new(),
        };
        if !self.bus.has_hook("tool.call") {
            return proceed;
        }
        let original: Value = serde_json::from_str(&call.function.arguments)
            .unwrap_or_else(|_| Value::String(call.function.arguments.clone()));
        let input = json!({"callId": call.id, "tool": call.function.name, "input": original});
        let outcome = self.handle.block_on(self.bus.invoke(
            "tool.call",
            &Self::context(scope, call),
            input,
        ));
        match outcome {
            // 总线自身拒绝（如重入超限）按阻止处理，不默认放行。
            Err(error) => ToolCallDecision::Blocked {
                plugin: "extension-bus".into(),
                reason: error.to_string(),
            },
            Ok(outcome) => match outcome.verdict {
                Verdict::Blocked { plugin, reason } => ToolCallDecision::Blocked { plugin, reason },
                Verdict::Proceed(candidate) => {
                    let rewritten = candidate.get("input").filter(|value| **value != original);
                    match rewritten {
                        None => proceed,
                        Some(input) => ToolCallDecision::Proceed {
                            arguments: Some(input.to_string()),
                            rewritten_by: outcome
                                .trace
                                .iter()
                                .filter(|entry| entry.outcome == TraceOutcome::Replaced)
                                .map(|entry| entry.plugin.clone())
                                .collect(),
                        },
                    }
                }
            },
        }
    }

    fn tool_result(
        &self,
        scope: &ToolHookScope<'_>,
        call: &ChatToolCall,
        view: ToolResultView<'_>,
    ) -> Option<String> {
        if !self.bus.has_hook("tool.result") {
            return None;
        }
        let input = json!({
            "callId": call.id,
            "tool": call.function.name,
            "status": view.status,
            "projection": view.projection,
        });
        let outcome = self.handle.block_on(self.bus.invoke(
            "tool.result",
            &Self::context(scope, call),
            input,
        ));
        match outcome {
            Err(error) => Some(format!("[扩展总线拒绝了该工具结果：{error}]")),
            Ok(outcome) => match outcome.verdict {
                Verdict::Blocked { plugin, reason } => {
                    Some(format!("[扩展 {plugin} 阻止了该工具结果：{reason}]"))
                }
                Verdict::Proceed(candidate) => candidate
                    .get("projection")
                    .and_then(Value::as_str)
                    .filter(|projection| *projection != view.projection)
                    .map(str::to_owned),
            },
        }
    }
}

/// 观察类钩子的来源：daemon 的领域事件总线。
///
/// 事件总线上的事件都是先提交后发布的事实，因此“先提交后观察”天然成立，且不需要在
/// 各个路由与运行时热路径上各加一份回调。投影只带有限、脱敏的字段，不转发正文、
/// 提示词、凭据或 base_url。事件总线是广播通道：消费落后时会丢事件，这里只记日志，
/// 扩展需要权威事实时应读取快照（观察钩子不承诺完整送达，见协议第 10 节）。
pub(super) fn spawn_event_forwarder(bus: Arc<ExtensionBus>, events: Arc<InMemoryEventBus>) {
    let mut receiver = events.subscribe();
    tokio::spawn(async move {
        let mut ended = EndedTurns::default();
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    if let Some((hook, payload)) = project_event(&event, &mut ended) {
                        bus.notify(hook, payload);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "扩展事件转发落后，部分观察事件已丢失");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

/// 已经通知过 `turn.end` 的 Turn，避免同一终态的多个 item 事件重复通知。
#[derive(Default)]
struct EndedTurns {
    order: VecDeque<String>,
}

impl EndedTurns {
    fn first_time(&mut self, turn_id: &str) -> bool {
        if self.order.iter().any(|seen| seen == turn_id) {
            return false;
        }
        if self.order.len() >= 1024 {
            self.order.pop_front();
        }
        self.order.push_back(turn_id.to_string());
        true
    }
}

fn first_str<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
}

fn project_event(event: &EventEnvelope, ended: &mut EndedTurns) -> Option<(&'static str, Value)> {
    let payload = &event.payload;
    let session = event.session_id.as_ref().map(ToString::to_string);
    let workspace = event.workspace_id.as_ref().map(ToString::to_string);
    match event.event_type.as_str() {
        "session.created" => Some((
            "session.created",
            json!({"sessionId": session, "workspaceId": workspace}),
        )),
        "session.deleted" => Some((
            "session.deleted",
            json!({"sessionId": session, "workspaceId": workspace}),
        )),
        "session.turn.task.accepted" | "session.turn.conversation.accepted" => {
            let turn = payload.get("canonical_turn");
            Some((
                "turn.accepted",
                json!({
                    "sessionId": session,
                    "workspaceId": workspace,
                    "turnId": turn.and_then(|t| first_str(t, &["turnId", "turn_id"])),
                    "requestId": first_str(payload, &["request_id"]),
                    "route": first_str(payload, &["route"]),
                }),
            ))
        }
        "session.turn.item" => {
            let turn = payload.get("current_turn")?;
            let status = first_str(turn, &["status"])?;
            if !matches!(
                status,
                "completed" | "failed" | "cancelled" | "interrupted" | "superseded"
            ) {
                return None;
            }
            let turn_id = first_str(turn, &["turn_id", "turnId"])?;
            ended.first_time(turn_id).then(|| {
                (
                    "turn.end",
                    json!({
                        "sessionId": session,
                        "workspaceId": workspace,
                        "turnId": turn_id,
                        "outcome": status,
                        "durationMs": turn.get("response_duration_ms").and_then(Value::as_u64),
                    }),
                )
            })
        }
        "model.usage.recorded" => {
            let usage = payload.get("usage");
            let number = |key: &str| usage.and_then(|u| u.get(key)).and_then(Value::as_u64);
            Some((
                "usage.update",
                json!({
                    "sessionId": session,
                    "turnId": first_str(payload, &["turnId"]),
                    "callId": payload.get("callIdentity").and_then(|c| first_str(c, &["callId"])),
                    "provider": payload.get("modelConfig").and_then(|c| first_str(c, &["provider"])),
                    "model": payload.get("modelConfig").and_then(|c| first_str(c, &["model"])),
                    "status": payload.get("status"),
                    "inputTokens": number("inputTokens"),
                    "outputTokens": number("outputTokens"),
                    "cacheReadTokens": number("cacheReadTokens"),
                    "cacheWriteTokens": number("cacheWriteTokens"),
                    "committed": true,
                }),
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_bridge_client::ChatToolFunction;

    fn call() -> ChatToolCall {
        ChatToolCall {
            id: "c1".into(),
            kind: "function".into(),
            function: ChatToolFunction {
                name: "shell_exec".into(),
                arguments: r#"{"command":"ls"}"#.into(),
            },
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_registered_hooks_every_call_proceeds_untouched() {
        let hooks = ExtensionBusHooks::new(Arc::new(ExtensionBus::default()), Handle::current());
        let scope = ToolHookScope {
            session_id: "s",
            task_id: "t",
            workspace_id: None,
            worker_id: None,
            round: 1,
        };
        let call = call();
        let hooks = Arc::new(hooks);
        // 对话循环运行在阻塞线程上；适配器必须能从那里等待总线。
        let decision = tokio::task::spawn_blocking({
            let hooks = Arc::clone(&hooks);
            move || {
                let scope = scope;
                (
                    hooks.tool_call(&scope, &call),
                    hooks.tool_result(
                        &scope,
                        &call,
                        ToolResultView {
                            status: "succeeded",
                            projection: "x",
                        },
                    ),
                )
            }
        })
        .await
        .unwrap();
        assert_eq!(
            decision.0,
            ToolCallDecision::Proceed {
                arguments: None,
                rewritten_by: vec![]
            }
        );
        assert_eq!(decision.1, None);
    }

    /// 真实 Node 宿主经 ExtensionBus、适配器到对话运行时端口的完整链路。
    #[tokio::test(flavor = "multi_thread")]
    async fn a_real_node_extension_blocks_rewrites_and_redacts_through_the_adapter() {
        use magi_extension_host::{
            bus::BusPolicy,
            instance::{LaunchConfig, launch},
            protocol::{HookMode, HookRegistration},
        };
        use std::path::Path;

        let host_crate = Path::new(env!("CARGO_MANIFEST_DIR")).join("../magi-extension-host");
        let dir = tempfile::tempdir().unwrap();
        let hooks_declared = vec![
            HookRegistration {
                name: "tool.call".into(),
                mode: HookMode::Guard,
                deadline_ms: Some(3000),
            },
            HookRegistration {
                name: "tool.result".into(),
                mode: HookMode::Advise,
                deadline_ms: Some(3000),
            },
        ];
        let mut config = LaunchConfig::new(
            magi_process::resolve_executable("node").expect("需要 Node"),
            host_crate.join("runtime/host.mjs"),
            host_crate.join("tests/fixtures/guard/extension/index.mjs"),
            dir.path().join("sock"),
        );
        config.plugin_id = "acme.guard".into();
        config.version = "1.0.0".into();
        config.config = json!({"marker": null, "hooks": ["tool.call", "tool.result"]});
        config.hooks = hooks_declared.clone();

        let bus = Arc::new(ExtensionBus::new(BusPolicy::default()));
        bus.register_plugin("acme.guard", &hooks_declared).unwrap();
        let group = magi_process::ManagedProcessGroup::new();
        let instance = launch(&group, config, 1).await.expect("launch");
        assert!(bus.attach(Arc::clone(&instance)));

        let hooks = Arc::new(ExtensionBusHooks::new(Arc::clone(&bus), Handle::current()));
        let outcome = tokio::task::spawn_blocking({
            let hooks = Arc::clone(&hooks);
            move || {
                let scope = ToolHookScope {
                    session_id: "s",
                    task_id: "t",
                    workspace_id: None,
                    worker_id: None,
                    round: 1,
                };
                let with = |arguments: &str| {
                    let mut call = call();
                    call.function.arguments = arguments.into();
                    call
                };
                (
                    hooks.tool_call(&scope, &with(r#"{"command":"rm -rf /"}"#)),
                    hooks.tool_call(&scope, &with(r#"{"command":"rewrite"}"#)),
                    hooks.tool_call(&scope, &with(r#"{"command":"ls"}"#)),
                    hooks.tool_result(
                        &scope,
                        &call(),
                        ToolResultView {
                            status: "succeeded",
                            projection: "the secret key",
                        },
                    ),
                    hooks.tool_result(
                        &scope,
                        &call(),
                        ToolResultView {
                            status: "succeeded",
                            projection: "fine",
                        },
                    ),
                )
            }
        })
        .await
        .unwrap();

        assert_eq!(
            outcome.0,
            ToolCallDecision::Blocked {
                plugin: "acme.guard".into(),
                reason: "禁止递归删除".into()
            }
        );
        assert_eq!(
            outcome.1,
            ToolCallDecision::Proceed {
                arguments: Some(r#"{"command":"echo safe"}"#.into()),
                rewritten_by: vec!["acme.guard".into()],
            }
        );
        assert_eq!(
            outcome.2,
            ToolCallDecision::Proceed {
                arguments: None,
                rewritten_by: vec![]
            }
        );
        assert_eq!(outcome.3.as_deref(), Some("[redacted]"));
        assert_eq!(outcome.4, None);
        instance.stop().await;
    }

    fn domain(event_type: &str, session: &str, payload: Value) -> EventEnvelope {
        EventEnvelope::domain(magi_core::EventId::unique("evt"), event_type, payload).with_context(
            magi_event_bus::EventContext {
                session_id: Some(magi_core::SessionId::new(session)),
                ..Default::default()
            },
        )
    }

    #[test]
    fn domain_events_are_projected_to_bounded_observe_payloads() {
        let mut ended = EndedTurns::default();
        let created =
            project_event(&domain("session.created", "s1", json!({})), &mut ended).unwrap();
        assert_eq!(created.0, "session.created");
        assert_eq!(created.1["sessionId"], "s1");

        let accepted = project_event(
            &domain(
                "session.turn.task.accepted",
                "s1",
                json!({"request_id": "r1", "route": "task", "text": "秘密正文", "canonical_turn": {"turnId": "t1"}}),
            ),
            &mut ended,
        )
        .unwrap();
        assert_eq!(accepted.0, "turn.accepted");
        assert_eq!(accepted.1["turnId"], "t1");
        assert!(!accepted.1.to_string().contains("秘密正文"), "正文不得转发");

        // 非终态的 item 事件不产生 turn.end；终态只通知一次。
        let item = |status: &str| {
            domain(
                "session.turn.item",
                "s1",
                json!({"current_turn": {"turn_id": "t1", "status": status, "response_duration_ms": 42}}),
            )
        };
        assert!(project_event(&item("running"), &mut ended).is_none());
        let end = project_event(&item("completed"), &mut ended).unwrap();
        assert_eq!(
            (end.0, end.1["outcome"].clone(), end.1["durationMs"].clone()),
            ("turn.end", json!("completed"), json!(42))
        );
        assert!(
            project_event(&item("completed"), &mut ended).is_none(),
            "同一终态不重复通知"
        );
        assert!(
            project_event(
                &domain("session.title.updated", "s1", json!({})),
                &mut ended
            )
            .is_none()
        );
    }

    #[test]
    fn usage_projection_carries_tokens_and_model_identity_but_no_credentials() {
        let mut ended = EndedTurns::default();
        let event = domain(
            "model.usage.recorded",
            "s1",
            json!({
                "turnId": "t1",
                "callIdentity": {"callId": "c1"},
                "modelConfig": {"provider": "anthropic", "model": "m", "baseUrl": "https://user:pass@host", "apiKey": "sk-secret"},
                "usage": {"inputTokens": 10, "outputTokens": 5},
                "status": "succeeded",
            }),
        );
        let (hook, payload) = project_event(&event, &mut ended).unwrap();
        assert_eq!(hook, "usage.update");
        assert_eq!(
            (payload["inputTokens"].clone(), payload["model"].clone()),
            (json!(10), json!("m"))
        );
        let text = payload.to_string();
        assert!(
            !text.contains("sk-secret") && !text.contains("pass@host"),
            "{text}"
        );
    }

    /// 其余已接线钩子经真实 Node 扩展的完整链路：input 与 subagent.spawn 的 guard，
    /// 以及 session / turn / usage 观察事件从领域事件总线转发到扩展。
    #[tokio::test(flavor = "multi_thread")]
    async fn input_spawn_guards_and_observed_domain_events_reach_a_real_node_extension() {
        use magi_extension_host::{
            bus::BusPolicy,
            instance::{LaunchConfig, launch},
            protocol::{HookMode, HookRegistration},
        };
        use std::path::Path;

        let host_crate = Path::new(env!("CARGO_MANIFEST_DIR")).join("../magi-extension-host");
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("marker.log");
        let declared = vec![
            HookRegistration {
                name: "input".into(),
                mode: HookMode::Guard,
                deadline_ms: Some(3000),
            },
            HookRegistration {
                name: "subagent.spawn".into(),
                mode: HookMode::Guard,
                deadline_ms: Some(3000),
            },
            HookRegistration {
                name: "session.created".into(),
                mode: HookMode::Observe,
                deadline_ms: None,
            },
            HookRegistration {
                name: "turn.end".into(),
                mode: HookMode::Observe,
                deadline_ms: None,
            },
            HookRegistration {
                name: "usage.update".into(),
                mode: HookMode::Observe,
                deadline_ms: None,
            },
        ];
        let node = std::env::var_os("MAGI_EXTENSION_NODE")
            .map(std::path::PathBuf::from)
            .or_else(|| magi_process::resolve_executable("node"))
            .expect("需要 Node");
        let mut config = LaunchConfig::new(
            node,
            host_crate.join("runtime/host.mjs"),
            host_crate.join("tests/fixtures/guard/extension/index.mjs"),
            dir.path().join("sock"),
        );
        config.plugin_id = "acme.guard".into();
        config.version = "1.0.0".into();
        config.config = json!({
            "marker": marker,
            "hooks": declared.iter().map(|h| h.name.clone()).collect::<Vec<_>>(),
        });
        config.hooks = declared.clone();

        let bus = Arc::new(ExtensionBus::new(BusPolicy::default()));
        bus.register_plugin("acme.guard", &declared).unwrap();
        let group = magi_process::ManagedProcessGroup::new();
        let instance = launch(&group, config, 1).await.expect("launch");
        assert!(bus.attach(Arc::clone(&instance)));
        let hooks = Arc::new(ExtensionBusHooks::new(Arc::clone(&bus), Handle::current()));

        // input guard（异步端口）：违规输入被拒绝，正常输入放行。
        let view = |text| InputView {
            session_id: Some("s1"),
            workspace_id: None,
            request_id: Some("r1"),
            text,
            image_count: 0,
            kind: "turn",
        };
        assert_eq!(
            hooks.check(view("this is forbidden")).await,
            InputDecision::Blocked {
                plugin: "acme.guard".into(),
                reason: "输入违规".into()
            }
        );
        assert_eq!(hooks.check(view("hello")).await, InputDecision::Proceed);

        // subagent.spawn guard（同步端口，运行在阻塞线程）。
        let spawn = tokio::task::spawn_blocking({
            let hooks = Arc::clone(&hooks);
            move || {
                let scope = ToolHookScope {
                    session_id: "s1",
                    task_id: "t",
                    workspace_id: None,
                    worker_id: None,
                    round: 1,
                };
                let ask = |role: &str| {
                    hooks.subagent_spawn(
                        &scope,
                        &SubagentSpawnView {
                            tool_call_id: "c1",
                            role,
                            task_name: "x",
                            goal: "g",
                        },
                    )
                };
                (ask("reviewer"), ask("worker"))
            }
        })
        .await
        .unwrap();
        assert_eq!(
            spawn.0,
            SpawnDecision::Blocked {
                plugin: "acme.guard".into(),
                reason: "禁止评审代理".into()
            }
        );
        assert_eq!(spawn.1, SpawnDecision::Proceed);

        // 领域事件 -> 观察钩子。
        let events = Arc::new(InMemoryEventBus::new(64));
        spawn_event_forwarder(Arc::clone(&bus), Arc::clone(&events));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        events.publish(domain("session.created", "s9", json!({})));
        events.publish(domain(
            "session.turn.item",
            "s9",
            json!({"current_turn": {"turn_id": "t9", "status": "completed"}}),
        ));
        events.publish(domain(
            "model.usage.recorded",
            "s9",
            json!({"modelConfig": {"provider": "p", "model": "m1"}, "usage": {"inputTokens": 7, "outputTokens": 1}}),
        ));
        let mut log = String::new();
        for _ in 0..60 {
            log = std::fs::read_to_string(&marker).unwrap_or_default();
            if log.lines().count() >= 3 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let lines: std::collections::BTreeSet<_> = log.lines().collect();
        assert!(lines.contains("session.created:s9"), "{log}");
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("turn.end:") && l.ends_with(":t9")),
            "{log}"
        );
        assert!(lines.contains("usage:m1:7"), "{log}");
        instance.stop().await;
    }

    /// 改写类钩子经真实 Node 扩展：历史改写提案、授权开关、压缩前的取消与自定义摘要。
    #[tokio::test(flavor = "multi_thread")]
    async fn history_rewrite_hooks_reach_a_real_node_extension_and_respect_authorization() {
        use magi_extension_host::{
            bus::BusPolicy,
            instance::{LaunchConfig, launch},
            protocol::{HookMode, HookRegistration},
        };
        use std::path::Path;

        let host_crate = Path::new(env!("CARGO_MANIFEST_DIR")).join("../magi-extension-host");
        let dir = tempfile::tempdir().unwrap();
        let declared = vec![
            HookRegistration {
                name: "context.assemble".into(),
                mode: HookMode::Advise,
                deadline_ms: Some(3000),
            },
            HookRegistration {
                name: "compaction.before".into(),
                mode: HookMode::Advise,
                deadline_ms: Some(3000),
            },
        ];
        let node = std::env::var_os("MAGI_EXTENSION_NODE")
            .map(std::path::PathBuf::from)
            .or_else(|| magi_process::resolve_executable("node"))
            .expect("需要 Node");
        let mut config = LaunchConfig::new(
            node,
            host_crate.join("runtime/host.mjs"),
            host_crate.join("tests/fixtures/guard/extension/index.mjs"),
            dir.path().join("sock"),
        );
        config.plugin_id = "acme.guard".into();
        config.version = "1.0.0".into();
        config.config = json!({"marker": null, "hooks": ["context.assemble", "compaction.before"]});
        config.hooks = declared.clone();
        let bus = Arc::new(ExtensionBus::new(BusPolicy::default()));
        bus.register_plugin("acme.guard", &declared).unwrap();
        let group = magi_process::ManagedProcessGroup::new();
        let instance = launch(&group, config, 1).await.expect("launch");
        assert!(bus.attach(Arc::clone(&instance)));
        let hooks = Arc::new(ExtensionBusHooks::new(Arc::clone(&bus), Handle::current()));

        assert!(
            !hooks.context_rewrite_authorized("acme.guard"),
            "默认未授权"
        );
        bus.set_context_rewrite_authorized("acme.guard", true);
        assert!(hooks.context_rewrite_authorized("acme.guard"));

        let result = tokio::task::spawn_blocking({
            let hooks = Arc::clone(&hooks);
            move || {
                let message = |role: &str, text: &str| ChatMessage {
                    input_request_id: None,
                    context_origin: Default::default(),
                    role: role.into(),
                    content: Some(text.into()),
                    images: vec![],
                    tool_calls: vec![],
                    tool_call_id: None,
                    provider_context: vec![],
                };
                let history = vec![
                    message("user", "OLD 目标"),
                    message("assistant", "OLD 回答"),
                    message("user", "新问题"),
                    message("assistant", "新回答"),
                    message("user", "当前输入"),
                ];
                let scope = ToolHookScope {
                    session_id: "s1",
                    task_id: "t",
                    workspace_id: None,
                    worker_id: None,
                    round: 1,
                };
                let rewritable = 0..4;
                let proposals = hooks.context_assemble(
                    &scope,
                    &AssembleView {
                        session_id: "s1",
                        messages: &history,
                        rewritable: &rewritable,
                        context_window_tokens: 100_000,
                    },
                );
                let view = magi_conversation_runtime::apply_extension_rewrites(
                    &history,
                    rewritable.clone(),
                    &|plugin| hooks.context_rewrite_authorized(plugin),
                    &proposals,
                );
                let compaction = |source: &str| {
                    hooks.compaction_before(&CompactionView {
                        session_id: "s1",
                        reason: "threshold",
                        compacted_messages: 10,
                        original_tokens: 1000,
                        summary_budget_tokens: 500,
                        source,
                    })
                };
                (
                    proposals,
                    view,
                    compaction("plain"),
                    compaction("CANCELME"),
                    compaction("CUSTOMSUMMARY"),
                )
            }
        })
        .await
        .unwrap();

        assert_eq!(result.0.len(), 1);
        assert_eq!(
            (
                result.0[0].plugin_id.as_str(),
                result.0[0].from,
                result.0[0].to
            ),
            ("acme.guard", 0, 2)
        );
        let (view, records) = result.1.expect("授权的提案被采纳");
        assert_eq!(
            view.iter()
                .map(|m| m.content.clone().unwrap())
                .collect::<Vec<_>>(),
            ["[扩展摘要]", "新问题", "新回答", "当前输入"]
        );
        assert_eq!(records.len(), 1);
        assert_eq!(result.2, CompactionDecision::Proceed);
        assert_eq!(
            result.3,
            CompactionDecision::Cancel {
                plugin: "acme.guard".into(),
                reason: "稍后再压缩".into()
            }
        );
        assert!(
            matches!(result.4, CompactionDecision::Summary { ref plugin, ref summary } if plugin == "acme.guard" && summary.contains("扩展摘要"))
        );

        // 撤销授权：自定义摘要被忽略（保留原生摘要），历史改写提案会被唯一投影拒绝。
        bus.set_context_rewrite_authorized("acme.guard", false);
        let revoked = tokio::task::spawn_blocking({
            let hooks = Arc::clone(&hooks);
            move || {
                hooks.compaction_before(&CompactionView {
                    session_id: "s1",
                    reason: "threshold",
                    compacted_messages: 10,
                    original_tokens: 1000,
                    summary_budget_tokens: 500,
                    source: "CUSTOMSUMMARY",
                })
            }
        })
        .await
        .unwrap();
        assert_eq!(revoked, CompactionDecision::Proceed);
        instance.stop().await;
    }

    /// `prompt.contribute` 经真实 Node 扩展：包摘要进入片段身份，阶段/稳定性/预算由唯一接纳点裁决。
    #[tokio::test(flavor = "multi_thread")]
    async fn prompt_contributions_from_a_real_node_extension_carry_the_package_digest() {
        use magi_extension_host::{
            bus::BusPolicy,
            instance::{LaunchConfig, launch},
            protocol::{HookMode, HookRegistration},
        };
        use std::path::Path;

        let host_crate = Path::new(env!("CARGO_MANIFEST_DIR")).join("../magi-extension-host");
        let dir = tempfile::tempdir().unwrap();
        let declared = vec![HookRegistration {
            name: "prompt.contribute".into(),
            mode: HookMode::Advise,
            deadline_ms: Some(3000),
        }];
        let node = std::env::var_os("MAGI_EXTENSION_NODE")
            .map(std::path::PathBuf::from)
            .or_else(|| magi_process::resolve_executable("node"))
            .expect("需要 Node");
        let mut config = LaunchConfig::new(
            node,
            host_crate.join("runtime/host.mjs"),
            host_crate.join("tests/fixtures/guard/extension/index.mjs"),
            dir.path().join("sock"),
        );
        config.plugin_id = "acme.guard".into();
        config.version = "1.0.0".into();
        config.digest = "0123456789abcdef0123456789abcdef".into();
        config.config = json!({"marker": null, "hooks": ["prompt.contribute"]});
        config.hooks = declared.clone();
        let bus = Arc::new(ExtensionBus::new(BusPolicy::default()));
        bus.register_plugin("acme.guard", &declared).unwrap();
        let group = magi_process::ManagedProcessGroup::new();
        let instance = launch(&group, config, 1).await.expect("launch");
        assert!(bus.attach(Arc::clone(&instance)));
        let hooks = Arc::new(ExtensionBusHooks::new(Arc::clone(&bus), Handle::current()));

        let (start, round) = tokio::task::spawn_blocking({
            let hooks = Arc::clone(&hooks);
            move || {
                let scope = ToolHookScope {
                    session_id: "s1",
                    task_id: "t1",
                    workspace_id: None,
                    worker_id: None,
                    round: 0,
                };
                let view = |phase, round| ContributeView {
                    session_id: "s1",
                    task_id: "t1",
                    phase,
                    round,
                    max_fragment_chars: 8192,
                    max_total_chars: 16384,
                };
                (
                    hooks.prompt_contribute(&scope, &view(ContributePhase::Start, 0)),
                    hooks.prompt_contribute(&scope, &view(ContributePhase::Round, 3)),
                )
            }
        })
        .await
        .unwrap();

        // 扩展在启动阶段同时提了稳定与易变片段：解析层都交出，由接纳点按阶段裁决。
        assert_eq!(start.len(), 2);
        let stable = start
            .iter()
            .find(|c| c.fragment_id == "house-style")
            .unwrap();
        assert_eq!(stable.stability, ContributionStability::Stable);
        assert_eq!(stable.priority, 1);
        assert_eq!(
            stable.package_digest, "0123456789abcdef0123456789abcdef",
            "片段身份带包摘要"
        );
        assert_eq!(stable.plugin_id, "acme.guard");
        assert_eq!(round.len(), 1);
        assert!(round[0].text.contains("第 3 轮"));
        instance.stop().await;
    }
}
