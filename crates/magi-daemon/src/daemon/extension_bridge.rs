//! ExtensionBus 与对话运行时工具管线的适配器。
//!
//! 对话循环在阻塞线程上同步执行工具；适配器用 daemon 的 tokio Handle 等待 ExtensionBus 的
//! 异步分发。没有任何扩展登记 `tool.call` / `tool.result` 时直接放行，不产生分发开销。

use magi_bridge_client::ChatToolCall;
use magi_conversation_runtime::tool_hooks::{
    ToolCallDecision, ToolHookScope, ToolHooks, ToolResultView,
};
use magi_extension_host::bus::{ExtensionBus, HookContext, TraceOutcome, Verdict};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::runtime::Handle;

pub(super) struct ExtensionToolHooks {
    bus: Arc<ExtensionBus>,
    handle: Handle,
}

impl ExtensionToolHooks {
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

impl ToolHooks for ExtensionToolHooks {
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
        let hooks = ExtensionToolHooks::new(Arc::new(ExtensionBus::default()), Handle::current());
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

        let hooks = Arc::new(ExtensionToolHooks::new(Arc::clone(&bus), Handle::current()));
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
}
