//! GPT Web 槽位接入 Magi MCP 服务（`follow_web_slot` 归属）。
//!
//! 槽位只有一个事实源：`magi_web_model::WebSlotTable`。本模块不保存槽位状态，只把它投影成
//! MCP 服务需要的两件事：
//! - **身份**：槽位端点上的调用方是“GPT Web”，工作区取自槽位拥有者会话的项目（W17），
//!   权限档来自用户在设置里选择的连接器权限（默认 `edit`）；
//! - **归属**：`tools/call` 只归属槽位拥有者**唯一进行中的 turn**（W9）；没有进行中的 turn、
//!   槽位已释放或工作区不一致时一律拒绝。
//!
//! 该端点是 daemon 的本地 socket（仅当前 OS 用户可访问），不接受也不签发令牌；ChatGPT 连接器
//! 经 OpenAI Tunnel 的 stdio 拉起 `magi-mcp --stdio --slot` 连到它。槽位释放即令身份消失，
//! 已建立的连接下一条请求立即失败，待审批同时被取消。

use std::sync::Arc;

use magi_mcp_server::local_socket::{PrincipalProvider, SocketAuth};
use magi_mcp_server::{
    AttributionMode, AttributionRefusal, AttributionResolver, AttributionTarget, ClientPrincipal,
    Profile,
};
use magi_web_model::{WebSlotOwner, WebSlotTable};

/// 槽位端点上固定的调用方标识（审批、审计里显示的“令牌”）。
pub(crate) const WEB_SLOT_PRINCIPAL_ID: &str = "web-slot";
pub(crate) const WEB_SLOT_CLIENT_NAME: &str = "GPT Web";

/// 连接器权限档的设置值。**不含 `exec`**：Web 槽位不开放命令执行。
pub(crate) fn slot_profile_from_setting(value: Option<&str>) -> Profile {
    match value.map(str::trim) {
        Some("read_only") => Profile::ReadOnly,
        Some("edit_trusted") => Profile::EditTrusted,
        _ => Profile::Edit,
    }
}

/// 槽位拥有者当前进行中的 turn（若有）。
fn active_turn(slots: &WebSlotTable) -> Option<(WebSlotOwner, u64)> {
    slots.active_context()
}

/// `follow_web_slot` 的归属解析：有进行中的 turn 才接受调用。
pub(crate) struct SlotAttribution {
    pub(crate) slots: Arc<dyn Fn() -> Option<Arc<WebSlotTable>> + Send + Sync>,
}

impl AttributionResolver for SlotAttribution {
    fn resolve(
        &self,
        principal: &ClientPrincipal,
    ) -> Result<AttributionTarget, AttributionRefusal> {
        match principal.attribution {
            AttributionMode::External => Ok(AttributionTarget::ExternalSession {
                token_id: principal.token_id.clone(),
            }),
            AttributionMode::FollowWebSlot => {
                let refusal =
                    || AttributionRefusal("当前没有可归属的进行中的 GPT Web 对话".to_string());
                let slots = (self.slots)().ok_or_else(refusal)?;
                let (owner, turn) = active_turn(&slots).ok_or_else(refusal)?;
                // 身份里的工作区必须就是槽位拥有者会话的项目，防止拿它去操作别的项目。
                if owner.project_id != principal.workspace_id {
                    return Err(refusal());
                }
                Ok(AttributionTarget::WebSlotTurn {
                    session_id: owner.session_id,
                    turn_id: turn.to_string(),
                })
            }
        }
    }
}

/// 槽位端点的身份提供者：每条请求按当前槽位事实现算。
///
/// 没有槽位时仍返回身份（工作区为空）：`initialize` / `tools/list` 可以在 Web turn 之前执行，
/// 连接器配置阶段需要读到工具目录；`tools/call` 会在归属解析处被拒绝。
pub(crate) fn slot_principal_provider(
    slots: Arc<dyn Fn() -> Option<Arc<WebSlotTable>> + Send + Sync>,
    profile: Arc<dyn Fn() -> Profile + Send + Sync>,
) -> SocketAuth {
    let provider: PrincipalProvider = Arc::new(move || {
        let workspace_id = (slots)()
            .and_then(|slots| slots.snapshot())
            .map(|snapshot| snapshot.owner_project_id)
            .unwrap_or_default();
        Some(ClientPrincipal {
            token_id: WEB_SLOT_PRINCIPAL_ID.to_string(),
            token_prefix: WEB_SLOT_PRINCIPAL_ID.to_string(),
            client_name: WEB_SLOT_CLIENT_NAME.to_string(),
            workspace_id,
            profile: profile(),
            attribution: AttributionMode::FollowWebSlot,
        })
    });
    SocketAuth::Principal(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magi_web_model::{WebConversationBinding, WebSlotOwner};

    fn principal(workspace: &str) -> ClientPrincipal {
        ClientPrincipal {
            token_id: WEB_SLOT_PRINCIPAL_ID.to_string(),
            token_prefix: WEB_SLOT_PRINCIPAL_ID.to_string(),
            client_name: WEB_SLOT_CLIENT_NAME.to_string(),
            workspace_id: workspace.to_string(),
            profile: Profile::Edit,
            attribution: AttributionMode::FollowWebSlot,
        }
    }

    fn resolver(table: &Arc<WebSlotTable>) -> SlotAttribution {
        let table = table.clone();
        SlotAttribution {
            slots: Arc::new(move || Some(table.clone())),
        }
    }

    #[test]
    fn calls_attach_only_to_the_owners_single_active_turn_of_the_matching_project() {
        let table = Arc::new(WebSlotTable::new());
        let resolver = resolver(&table);
        assert!(
            resolver.resolve(&principal("project-a")).is_err(),
            "没有槽位"
        );

        let owner = WebSlotOwner::new("session-a", "project-a");
        table
            .claim(owner.clone(), WebConversationBinding::temporary(), "page")
            .unwrap();
        assert!(
            resolver.resolve(&principal("project-a")).is_err(),
            "槽位存在但没有进行中的 turn"
        );

        let lease = table.begin_turn(&owner).unwrap();
        match resolver.resolve(&principal("project-a")).unwrap() {
            AttributionTarget::WebSlotTurn {
                session_id,
                turn_id,
            } => {
                assert_eq!(session_id, "session-a");
                assert_eq!(turn_id, lease.turn_id().to_string());
            }
            other => panic!("{other:?}"),
        }
        assert!(
            resolver.resolve(&principal("project-b")).is_err(),
            "工作区与槽位拥有者的项目不一致"
        );

        drop(lease);
        assert!(
            resolver.resolve(&principal("project-a")).is_err(),
            "turn 结束后立即拒绝"
        );
        table.release(&owner);
        assert!(
            resolver.resolve(&principal("project-a")).is_err(),
            "槽位释放后拒绝"
        );
    }

    #[test]
    fn principal_tracks_the_slot_and_never_offers_exec() {
        let table = Arc::new(WebSlotTable::new());
        let auth = {
            let table = table.clone();
            slot_principal_provider(
                Arc::new(move || Some(table.clone())),
                Arc::new(|| slot_profile_from_setting(Some("exec"))),
            )
        };
        let SocketAuth::Principal(provider) = auth else {
            panic!("槽位端点必须是动态身份");
        };
        let before = provider().unwrap();
        assert_eq!(
            before.workspace_id, "",
            "没有槽位时只有空工作区（只能读目录）"
        );
        assert_eq!(before.attribution, AttributionMode::FollowWebSlot);
        assert_eq!(
            before.profile,
            Profile::Edit,
            "未知 / exec 设置值一律回落到 edit"
        );

        table
            .claim(
                WebSlotOwner::new("s", "project-x"),
                WebConversationBinding::temporary(),
                "page",
            )
            .unwrap();
        assert_eq!(provider().unwrap().workspace_id, "project-x");
    }

    #[test]
    fn profile_setting_parsing_is_conservative() {
        assert_eq!(
            slot_profile_from_setting(Some("read_only")),
            Profile::ReadOnly
        );
        assert_eq!(
            slot_profile_from_setting(Some("edit_trusted")),
            Profile::EditTrusted
        );
        assert_eq!(slot_profile_from_setting(Some("edit")), Profile::Edit);
        assert_eq!(slot_profile_from_setting(None), Profile::Edit);
        assert_eq!(slot_profile_from_setting(Some("exec")), Profile::Edit);
    }

    // ── 端到端：槽位端点 + 槽位表 + 会话 canonical ───────────────────────────────

    use crate::mcp_runtime::McpServiceRuntime;
    use magi_conversation_runtime::ToolApprovalDecision;
    use magi_core::{AbsolutePath, SessionId, UtcMillis, WorkspaceId};
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_session_store::SessionStore;
    use magi_tool_runtime::ToolRegistry;
    use magi_workspace::WorkspaceStore;
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    use crate::state::ApiState;

    struct Fixture {
        state: ApiState,
        table: Arc<WebSlotTable>,
        session_id: SessionId,
        owner: WebSlotOwner,
        root: PathBuf,
        endpoint: PathBuf,
        _dir: tempfile::TempDir,
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let event_bus = Arc::new(InMemoryEventBus::new(64));
        let governance = Arc::new(GovernanceService::default());
        let mut registry = ToolRegistry::new(governance.clone(), event_bus.clone());
        registry.register_default_builtins();
        let mut state = ApiState::new(
            "magi-test",
            event_bus,
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            governance,
        )
        .with_tool_registry(registry);
        state.mcp_service = Arc::new(McpServiceRuntime::load(dir.path().join("mcp-server.json")));
        let root = dir.path().join("project").canonicalize_or_create();
        state
            .workspace_registry
            .register(
                WorkspaceId::new("ws-slot"),
                AbsolutePath::new(root.display().to_string()),
            )
            .unwrap();
        let session_id = SessionId::new("session-owner");
        state
            .session_store
            .create_session_for_workspace(session_id.clone(), "owner", Some("ws-slot".to_string()))
            .unwrap();
        let table = Arc::new(WebSlotTable::new());
        state.web_model.set_bindings(table.clone());
        state.install_web_slot_hooks();
        let endpoint = state.mcp_service.start_slot_gateway(&state).await.unwrap();
        Fixture {
            state,
            table,
            session_id,
            owner: WebSlotOwner::new("session-owner", "ws-slot"),
            root,
            endpoint,
            _dir: dir,
        }
    }

    trait CanonicalizeOrCreate {
        fn canonicalize_or_create(self) -> PathBuf;
    }
    impl CanonicalizeOrCreate for PathBuf {
        fn canonicalize_or_create(self) -> PathBuf {
            std::fs::create_dir_all(&self).unwrap();
            self.canonicalize().unwrap()
        }
    }

    impl Fixture {
        /// 槽位拥有者开始一个 Web turn：会话里有进行中的 Turn，槽位表里有对应租约。
        fn begin_web_turn(&self) -> magi_web_model::WebTurnLease {
            crate::routes::test_turn_fixtures::seed_conversation_turn(
                &self.state.session_store,
                self.state.conversation_registry.turn_coordinator(),
                &self.session_id,
                "turn-web-1",
                1,
                UtcMillis::now(),
                "running",
                "hello",
            );
            if self.table.snapshot().is_none() {
                self.table
                    .claim(
                        self.owner.clone(),
                        WebConversationBinding::temporary(),
                        "page",
                    )
                    .unwrap();
            }
            self.table.begin_turn(&self.owner).unwrap()
        }
    }

    /// 像 `magi-mcp --stdio --slot` 一样：连接槽位端点（无握手）并发一条 JSON-RPC。
    async fn slot_call(
        endpoint: &PathBuf,
        method: &str,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let stream = tokio::net::UnixStream::connect(endpoint).await.unwrap();
        let (read, mut write) = tokio::io::split(stream);
        let request =
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
        write
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let line = tokio::time::timeout(
            Duration::from_secs(15),
            BufReader::new(read).lines().next_line(),
        )
        .await
        .expect("超时")
        .unwrap()
        .expect("连接被关闭");
        serde_json::from_str(&line).unwrap()
    }

    fn is_error(reply: &serde_json::Value) -> bool {
        reply["error"].is_object() || reply["result"]["isError"].as_bool().unwrap_or(false)
    }

    fn read_call() -> serde_json::Value {
        serde_json::json!({"name": "magi.fs.read", "arguments": {"path": "a.txt"}})
    }

    #[tokio::test]
    async fn catalog_is_readable_before_any_slot_but_calls_need_the_owners_active_turn() {
        let f = fixture().await;
        std::fs::write(f.root.join("a.txt"), "data").unwrap();

        let listed = slot_call(&f.endpoint, "tools/list", serde_json::json!({})).await;
        assert!(
            listed["result"]["tools"]
                .as_array()
                .is_some_and(|tools| tools.iter().any(|t| t["name"] == "magi.fs.write")),
            "连接器配置阶段必须能读到工具目录: {listed}"
        );
        let refused = slot_call(&f.endpoint, "tools/call", read_call()).await;
        assert!(
            is_error(&refused),
            "没有槽位 / 进行中的 turn 必须拒绝: {refused}"
        );

        let lease = f.begin_web_turn();
        let accepted = slot_call(&f.endpoint, "tools/call", read_call()).await;
        assert!(!is_error(&accepted), "{accepted}");
        assert!(accepted.to_string().contains("data"));

        drop(lease);
        let after = slot_call(&f.endpoint, "tools/call", read_call()).await;
        assert!(is_error(&after), "turn 结束后必须拒绝: {after}");
    }

    #[tokio::test]
    async fn slot_calls_are_written_inline_into_the_active_turn_as_tool_items() {
        let f = fixture().await;
        std::fs::write(f.root.join("a.txt"), "data").unwrap();
        let _lease = f.begin_web_turn();
        let reply = slot_call(&f.endpoint, "tools/call", read_call()).await;
        assert!(!is_error(&reply), "{reply}");

        let sidecar = f
            .state
            .session_store
            .runtime_sidecar(&f.session_id)
            .unwrap();
        let turn = sidecar.current_turn.expect("进行中的 turn");
        let kinds = turn
            .items
            .iter()
            .filter(|item| item.tool_name.as_deref() == Some("magi.fs.read"))
            .map(|item| (item.kind.clone(), item.tool_status.clone()))
            .collect::<Vec<_>>();
        assert!(
            kinds.iter().any(|(kind, _)| kind == "tool_call_result"),
            "工具调用与结果必须作为普通工具项写入 canonical: {kinds:?}"
        );
        assert!(
            kinds
                .iter()
                .any(|(_, status)| status.as_deref() == Some("succeeded"))
        );
    }

    #[tokio::test]
    async fn slot_write_asks_in_the_owner_session_and_lands_in_the_owner_ledger() {
        let f = fixture().await;
        let _lease = f.begin_web_turn();
        let approver = {
            let state = f.state.clone();
            let session_id = f.session_id.clone();
            std::thread::spawn(move || {
                let approvals = state.conversation_registry.tool_approvals().clone();
                for _ in 0..300 {
                    if let Some(pending) = approvals.pending_for_session(&session_id).first() {
                        // 跨会话待确认入口也必须能看到它。
                        let listed = crate::mcp_service::approval_owners(&state);
                        assert!(
                            listed
                                .iter()
                                .any(|owner| owner.token_id == WEB_SLOT_PRINCIPAL_ID)
                        );
                        approvals
                            .resolve(
                                &session_id,
                                &pending.approval_id,
                                ToolApprovalDecision::AllowOnce,
                            )
                            .unwrap();
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                panic!("槽位审批应出现在拥有者会话里");
            })
        };
        let reply = slot_call(
            &f.endpoint,
            "tools/call",
            serde_json::json!({"name": "magi.fs.write", "arguments": {"path": "slot.txt", "content": "from web"}}),
        )
        .await;
        approver.join().unwrap();
        assert!(!is_error(&reply), "{reply}");
        assert_eq!(
            std::fs::read_to_string(f.root.join("slot.txt")).unwrap(),
            "from web"
        );
        let snapshot = f
            .state
            .snapshot_session(&f.session_id, &f.root)
            .expect("拥有者会话的账本");
        snapshot.reconcile().unwrap();
        assert!(
            snapshot
                .pending_changes()
                .unwrap()
                .iter()
                .any(|c| c.path.ends_with("slot.txt"))
        );
    }

    #[tokio::test]
    async fn ending_the_turn_cancels_a_waiting_approval_without_writing() {
        let f = fixture().await;
        let lease = f.begin_web_turn();
        let ender = {
            let state = f.state.clone();
            let session_id = f.session_id.clone();
            std::thread::spawn(move || {
                let approvals = state.conversation_registry.tool_approvals().clone();
                for _ in 0..300 {
                    if !approvals.pending_for_session(&session_id).is_empty() {
                        drop(lease);
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            })
        };
        let reply = slot_call(
            &f.endpoint,
            "tools/call",
            serde_json::json!({"name": "magi.fs.write", "arguments": {"path": "never.txt", "content": "x"}}),
        )
        .await;
        ender.join().unwrap();
        assert!(is_error(&reply), "{reply}");
        assert!(!f.root.join("never.txt").exists());
        assert!(
            f.state
                .conversation_registry
                .tool_approvals()
                .pending_for_session(&f.session_id)
                .is_empty(),
            "turn 结束后不得遗留待审批"
        );
    }

    #[tokio::test]
    async fn releasing_the_slot_makes_the_next_call_fail_and_never_exposes_exec() {
        let f = fixture().await;
        let _lease = f.begin_web_turn();
        let listed = slot_call(&f.endpoint, "tools/list", serde_json::json!({})).await;
        let names = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert!(
            !names.contains(&"magi.shell.exec".to_string()),
            "槽位不开放命令执行"
        );
        f.table.release(&f.owner);
        let after = slot_call(&f.endpoint, "tools/call", read_call()).await;
        assert!(is_error(&after), "{after}");
    }
}
