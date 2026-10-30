//! Task Turn 的接纳、准备、调度与生命周期事件。

use crate::{
    browser_tool_runtime::resolve_browser_annotation_context,
    dto::{SessionDirectoryEntryDto, SessionTurnRequestDto},
    errors::ApiError,
    model_config::initial_session_orchestrator_config,
    performance::PerformanceTrace,
    scope_binding::resolve_turn_session,
    state::{ApiState, RunnerStartError},
    turn_service::{monotonic_accepted_at, publish_session_user_message_event},
};
use magi_conversation_runtime::dispatch_submission::{
    DispatchSubmissionAcceptError, DispatchSubmissionGraph, DispatchSubmissionRuntime,
    accept_dispatch_submission, dispatch_turn_id, ensure_dispatch_submission_acceptance_available,
    materialize_dispatch_submission, prepare_pending_dispatch_submission,
};
pub(crate) use magi_conversation_runtime::dispatch_submission::{
    DispatchSubmissionAccepted, DispatchSubmissionRequest, DispatchTurnOrigin,
};
use magi_conversation_runtime::{
    CoordinatorAdmission, CoordinatorCommandResult, ExecutionProfile, TurnAdmission, TurnCommand,
};
use sha2::{Digest, Sha256};

use magi_core::{
    BrowserSessionId, BrowserTabId, EventId, SessionId, TaskCompletionContract,
    TaskRecoveryCheckpoint, TaskStatus, TaskTier, UtcMillis, WorkspaceId, public_runtime_excerpt,
};
use magi_event_bus::{EventContext, EventEnvelope};
use serde_json::json;

use magi_browser_authority::ValidateBrowserNodeSelection;
use magi_conversation_runtime::dispatch_submission::cleanup_materialized_dispatch_submission_if_not_started;
use magi_conversation_runtime::dispatch_submission::recover_dispatch_submission_request;
use magi_conversation_runtime::session_images::SessionTurnImage;
use magi_conversation_runtime::session_writeback::publish_current_session_turn_item_event;
use magi_session_store::{
    CANONICAL_TURN_SCHEMA_VERSION, CanonicalTurn, CanonicalTurnItem, CanonicalTurnItemKind,
    SessionGoal,
};

/// 恢复/继续执行只能沿原任务绑定；只有旧版单目录任务允许在首次读取时补齐绑定。
pub(crate) fn validate_or_migrate_directory_binding(
    state: &ApiState,
    workspace_id: Option<&WorkspaceId>,
    original: Option<magi_core::ProjectDirectoryBinding>,
) -> Result<Option<magi_core::ProjectDirectoryBinding>, ApiError> {
    let Some(workspace_id) = workspace_id else {
        if original.is_some() {
            return Err(ApiError::Conflict("个人会话不能携带项目目录绑定".into()));
        }
        return Ok(None);
    };
    if let Some(binding) = original {
        if binding.workspace_id != *workspace_id {
            return Err(ApiError::Conflict("原任务目录绑定与当前项目不匹配".into()));
        }
        state
            .workspace_registry
            .validate_directory_membership(&binding)
            .map_err(|error| ApiError::Conflict(format!("原任务的源文件夹授权已失效: {error}")))?;
        return Ok(Some(binding));
    }
    let project = state
        .workspace_registry
        .project(workspace_id)
        .ok_or_else(|| ApiError::not_found("workspace 不存在", workspace_id.as_str()))?;
    if project.source_folders.len() != 1
        || project.revision != 1
        || project.primary_source_folder_id != format!("{workspace_id}-source-0")
    {
        return Err(ApiError::Conflict(
            "旧任务缺少源文件夹绑定，项目已编辑或缺少原单目录身份，不能重解释".into(),
        ));
    }
    state
        .workspace_registry
        .capture_directory_binding(workspace_id, None, None)
        .map(Some)
        .map_err(|error| ApiError::Conflict(format!("补齐旧任务目录绑定失败: {error}")))
}

/// 新接纳、主线继续和独立代理继续共用原目录绑定与会话副本解析。
pub(crate) fn bound_project_execution_root(
    state: &ApiState,
    session_id: &SessionId,
    binding: &magi_core::ProjectDirectoryBinding,
) -> Result<std::path::PathBuf, ApiError> {
    let source = state
        .workspace_registry
        .resolve_bound_source_directory(binding, None)
        .map_err(|error| ApiError::Conflict(format!("原任务的源文件夹不可用: {error}")))?;
    state.session_execution_root_for_source(session_id, &source)
}

pub(crate) fn canonical_project_directory_binding(
    turn: &magi_session_store::CanonicalTurn,
) -> Result<Option<magi_core::ProjectDirectoryBinding>, serde_json::Error> {
    turn.items
        .iter()
        .find_map(|item| {
            item.metadata.get("projectDirectories").or_else(|| {
                item.metadata
                    .get("pendingDispatch")
                    .and_then(|request| request.get("project_directories"))
            })
        })
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
}

/// 必须在 dispatcher / RunnerManager 装配前调用；捕获的状态只持有共享业务依赖，
/// 不持有正在构造的 dispatcher，避免运行时引用环。
pub fn workspace_execution_preparer(
    state: &ApiState,
) -> std::sync::Arc<magi_conversation_runtime::task_execution_dispatcher::WorkspaceExecutionPreparer>
{
    let state = state.clone();
    let runtime = tokio::runtime::Handle::try_current().ok();
    std::sync::Arc::new(move |session_id, workspace_id, turn_id, binding, mode| {
        let runtime = runtime
            .as_ref()
            .ok_or_else(|| "工作区执行准备缺少异步运行时".to_string())?;
        runtime.block_on(async {
            let started = std::time::Instant::now();
            let record_stage = |stage: &str| {
                tracing::info!(
                    %session_id, %turn_id, stage,
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "workspace execution preparation"
                );
            };
            record_stage("started");
            let still_active = || {
                state
                    .session_store
                    .runtime_sidecar(session_id)
                    .and_then(|sidecar| sidecar.current_turn)
                    .is_some_and(|turn| {
                        turn.turn_id == turn_id
                            && state
                                .session_store
                                .canonical_turn_for_session_turn_id(session_id, turn_id)
                                .is_some_and(|canonical| !canonical.status.is_terminal())
                    })
            };
            let execution = mode == magi_conversation_runtime::workspace_execution::WorkspacePreparationMode::Execution;
            if execution && !still_active() {
                return Err("工作区准备目标 Turn 已关闭或变化".to_string());
            }
            let binding = validate_or_migrate_directory_binding(
                &state,
                workspace_id.as_ref(),
                binding.cloned(),
            )
            .map_err(|error| error.message().to_string())?;
            if execution && let Some(binding) = binding.as_ref() {
                state
                    .isolate_on_workspace_contention(session_id, binding, turn_id)
                    .await;
            }
            record_stage("isolation_checked");
            if execution && !still_active() {
                return Err("隔离目录准备期间 Turn 已关闭或变化".to_string());
            }
            let Some(binding) = binding.as_ref() else {
                return Ok(Default::default());
            };
            let directories =
                magi_core::ProjectDirectoryExecutionMap::from_binding(binding, |grant| {
                    let source = state
                        .workspace_registry
                        .resolve_bound_source_directory(binding, Some(&grant.source_folder_id))?;
                    state
                        .session_execution_root_for_source(session_id, &source)
                        .map_err(|error| magi_core::DomainError::Validation {
                            message: error.message().into(),
                        })
                        .and_then(|root| std::fs::canonicalize(root).map_err(|error| magi_core::DomainError::Validation { message: format!("项目执行目录不可用: {error}") }))
                })
                .map_err(|error| error.to_string())?;
            if !execution {
                return Ok(magi_conversation_runtime::workspace_execution::WorkspaceExecutionResources {
                    directories: Some(directories), snapshots: Vec::new(),
                });
            }
            let mut repositories = Vec::new();
            for directory in &directories.roots {
                let source = directory.source_root().map_err(|error| error.to_string())?;
                let execution = directory
                    .execution_root()
                    .map_err(|error| error.to_string())?;
                if source != execution {
                    continue;
                }
                match state.git_service.observe(&execution).await {
                    Ok(observation) => repositories.push(observation.git_common_dir),
                    Err(magi_git::GitError::NotRepository { .. }) => {}
                    Err(error) => return Err(format!("观测项目仓库失败: {error}")),
                }
            }
            state
                .begin_session_git_execution_set(session_id, &repositories, Some(turn_id))
                .await
                .map_err(|error| error.message().to_string())?;
            let prepared = async {
                let mut snapshots = Vec::new();
                for directory in &directories.roots {
                    if !still_active() {
                        return Err("工作区准备期间 Turn 已关闭或变化".to_string());
                    }
                    state
                        .workspace_registry
                        .validate_directory_binding(binding)
                        .map_err(|error| error.to_string())?;
                    let source = directory.source_root().map_err(|error| error.to_string())?;
                    let execution = directory
                        .execution_root()
                        .map_err(|error| error.to_string())?;
                    if std::fs::canonicalize(state
                        .session_execution_root_for_source(session_id, &source)
                        .map_err(|error| error.message().to_string())?).map_err(|error| error.to_string())?
                        != execution
                    {
                        return Err("准备期间项目执行映射已变化".into());
                    }
                    let context = state
                        .ensure_session_code_context_for_root(
                            session_id,
                            &binding.workspace_id,
                            &source,
                        )
                        .await
                        .map_err(|error| format!("{error:?}"))?;
                    if context
                        .as_ref()
                        .is_some_and(|context| !repositories.contains(&context.git.git_common_dir))
                    {
                        return Err("准备期间项目仓库身份已变化".into());
                    }
                    snapshots.push(
                        state
                            .ensure_snapshot_session(session_id, &source)
                            .await
                            .map_err(|error| format!("{error:?}"))?,
                    );
                }
                if !still_active() {
                    return Err("工作区准备期间 Turn 已关闭或变化".to_string());
                }
                record_stage("resources_ready");
                Ok(
                    magi_conversation_runtime::workspace_execution::WorkspaceExecutionResources {
                        directories: Some(directories),
                        snapshots,
                    },
                )
            }
            .await;
            if prepared.is_err() {
                state.release_session_git_execution_lease(session_id);
            }
            prepared
        })
    })
}

/// 显式项目文件引用必须仍属于原冻结授权；引用身份不能用来授权项目外路径。
pub(crate) fn validate_project_context_references(
    binding: Option<&magi_core::ProjectDirectoryBinding>,
    references: &[magi_conversation_runtime::context_reference::SessionContextReference],
) -> Result<(), ApiError> {
    for reference in references {
        let Some(source_id) = reference.source_folder_id.as_deref() else {
            continue;
        };
        let binding = binding
            .ok_or_else(|| ApiError::InvalidInput("个人会话不能携带项目源文件夹引用".into()))?;
        let grant = binding
            .grant(Some(source_id))
            .map_err(|error| ApiError::InvalidInput(format!("上下文引用目录未获授权: {error}")))?;
        let root = magi_core::HostPath::from_path_ref(&grant.source_path_ref)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?
            .into_path_buf();
        let canonical = std::fs::canonicalize(&reference.path).map_err(|_| {
            ApiError::InvalidInput(format!("上下文引用已不可用: {}", reference.path.display()))
        })?;
        if !canonical.starts_with(&root) {
            return Err(ApiError::InvalidInput(
                "上下文引用不属于指定源文件夹".into(),
            ));
        }
    }
    Ok(())
}

pub fn submit_dispatch_submission(
    state: &ApiState,
    mut request: DispatchSubmissionRequest,
) -> Result<DispatchSubmissionAccepted, ApiError> {
    request.project_directories = validate_or_migrate_directory_binding(
        state,
        request.workspace_id.as_ref(),
        request.project_directories.take(),
    )?;
    validate_project_context_references(
        request.project_directories.as_ref(),
        &request.context_references,
    )?;
    if let Some(binding) = request.project_directories.as_ref() {
        request.execution_root = Some(bound_project_execution_root(
            state,
            &request.session_id,
            binding,
        )?);
    }
    ensure_dispatch_submission_acceptance_available(&state.session_store, &request)
        .map_err(dispatch_accept_error_to_api_error)?;
    // 排队输入在实际接纳为 Turn 时才选择插件包。这个不可变选择随原 accepted
    // 请求恢复，materialize 和后续工作流激活不能从当前安装版本重新推导。
    if let Some(workflow_id) = request.workflow_id.as_deref() {
        let selected = magi_plugin_system::ContributionRef::parse("工作流核心", workflow_id)
            .map_err(ApiError::InvalidInput)?;
        let manager = state
            .plugin_manager
            .lock()
            .map_err(|_| ApiError::internal_assembly("固定工作流版本", "插件锁已损坏"))?;
        let package = manager
            .prepare_workflow(&selected)
            .map_err(|_| ApiError::Forbidden("所选插件工作流未激活或正在排空".into()))?;
        request.user_message_metadata.insert(
            "magi.workflowPackageDigest".into(),
            serde_json::Value::String(package.digest().to_string()),
        );
    }

    // Task 与 Conversation 共用同一个 SessionTurnCoordinator。Task 的 root task
    // 仍由 TaskStore 管理，但 Turn 身份、attempt 和 request 幂等必须先注册，
    // 再进入 pending task / canonical accepted 事务；否则两个并发提交可能各自
    // 创建一棵任务树。
    let turn_id = dispatch_turn_id(&request);
    let request_id = request
        .request_id
        .clone()
        .unwrap_or_else(|| format!("request-{turn_id}"));
    let request_fingerprint = request
        .user_message_metadata
        .get("requestFingerprint")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| dispatch_request_fingerprint(&request));
    request.request_id = Some(request_id.clone());
    request.user_message_metadata.insert(
        "executionProfile".to_string(),
        serde_json::Value::String("task".to_string()),
    );
    request.user_message_metadata.insert(
        "requestId".to_string(),
        serde_json::Value::String(request_id.clone()),
    );
    request.user_message_metadata.insert(
        "requestFingerprint".to_string(),
        serde_json::Value::String(request_fingerprint.clone()),
    );
    let admission = match state
        .turn_coordinator()
        .execute_command(
            &request.session_id,
            TurnCommand::Start(TurnAdmission {
                turn_id: turn_id.clone(),
                request_id,
                request_fingerprint,
                profile: ExecutionProfile::Task,
            }),
        )
        .map_err(|error| ApiError::Conflict(error.to_string()))?
    {
        CoordinatorCommandResult::Admission(admission) => admission,
        other => {
            return Err(ApiError::internal_assembly(
                "接纳 task Turn",
                format!("Coordinator 返回了非法 Start 结果: {other:?}"),
            ));
        }
    };
    let coordinator_attempt = match admission {
        CoordinatorAdmission::Accepted(attempt) => attempt,
        CoordinatorAdmission::Replay(attempt) => {
            // canonical replay 在 HTTP/App Server 入口已经优先处理；到这里仍遇到
            // 进程内并发重试时，返回明确冲突，让调用方读取同一个 accepted 事实，
            // 绝不能再次 materialize 第二个 root task。
            return Err(ApiError::Conflict(format!(
                "Task Turn {} 已经接纳（attempt {}），请重放原请求读取 canonical receipt",
                attempt.turn_id, attempt.attempt_id
            )));
        }
    };
    request.user_message_metadata.insert(
        "attemptId".to_string(),
        serde_json::Value::String(coordinator_attempt.attempt_id.clone()),
    );

    let task_store = state.task_store().ok_or_else(|| {
        let _ = state.turn_coordinator().execute_command(
            &request.session_id,
            TurnCommand::Abort {
                attempt: coordinator_attempt.clone(),
            },
        );
        ApiError::internal_assembly("构建任务派发运行时", "task_store 未配置")
    })?;
    let workspace_root_path = request.execution_root.clone();
    let runtime = DispatchSubmissionRuntime {
        session_store: &state.session_store,
        task_store,
        execution_registry: state.task_execution_registry(),
        event_bus: &state.event_bus,
        agent_role_registry: &state.agent_role_registry,
        model_bridge_client: state.model_bridge_client(),
        settings_store: Some(&state.settings_store),
        workspace_root_path: workspace_root_path.as_deref(),
    };
    let pending = match prepare_pending_dispatch_submission(&runtime, &request) {
        Ok(pending) => pending,
        Err(error) => {
            let _ = state.turn_coordinator().execute_command(
                &request.session_id,
                TurnCommand::Abort {
                    attempt: coordinator_attempt.clone(),
                },
            );
            return Err(ApiError::internal_assembly(
                "准备任务派发提交失败",
                error.into_message(),
            ));
        }
    };
    let accepted_session_id_for_abort = request.session_id.clone();
    let graph = DispatchSubmissionGraph::new(
        pending.task.task_id.clone(),
        pending.task.task_id,
        Some(pending.active_execution_chain),
    );
    match accept_dispatch_submission(
        &state.session_store,
        task_store,
        state.task_execution_registry(),
        request,
        graph,
    ) {
        Ok(accepted) => {
            if let Some(notifier) = state.task_completion_notifier() {
                notifier.bind_coordinator_attempt(
                    &accepted.root_task_id,
                    &coordinator_attempt.attempt_id,
                    Some(accepted.session_id.clone()),
                    Some(accepted.turn_id.clone()),
                );
            }
            if let Err(error) = state
                .turn_coordinator()
                .begin_session_turn_input(accepted.session_id.clone(), accepted.turn_id.clone())
            {
                let message = format!("建立 task Turn 输入通道失败: {error}");
                fail_accepted_task_turn(
                    state,
                    &accepted.session_id,
                    &accepted.root_task_id,
                    &accepted.turn_id,
                    &message,
                );
                return Err(ApiError::Conflict(message));
            }
            Ok(accepted)
        }
        Err(error) => {
            let _ = state.turn_coordinator().execute_command(
                &accepted_session_id_for_abort,
                TurnCommand::Abort {
                    attempt: coordinator_attempt.clone(),
                },
            );
            Err(dispatch_accept_error_to_api_error(error))
        }
    }
}

/// accepted 之后才创建 Thread、执行计划和模型快照。
pub(crate) fn materialize_dispatch_submission_after_acceptance(
    state: &ApiState,
    accepted: &DispatchSubmissionAccepted,
) -> Result<(), ApiError> {
    let request = &accepted.request;
    validate_or_migrate_directory_binding(
        state,
        request.workspace_id.as_ref(),
        request.project_directories.clone(),
    )?;
    let workspace_root_path = request.execution_root.clone();
    let task_store = state
        .task_store()
        .ok_or_else(|| ApiError::internal_assembly("装配任务派发", "task_store 未配置"))?;
    let runtime = DispatchSubmissionRuntime {
        session_store: &state.session_store,
        task_store,
        execution_registry: state.task_execution_registry(),
        event_bus: &state.event_bus,
        agent_role_registry: &state.agent_role_registry,
        model_bridge_client: state.model_bridge_client(),
        settings_store: Some(&state.settings_store),
        workspace_root_path: workspace_root_path.as_deref(),
    };
    materialize_dispatch_submission(&runtime, request, &accepted.turn_id).map_err(|error| {
        ApiError::internal_assembly("装配任务派发执行面失败", error.into_message())
    })
}

/// 在调用方已经持有 session lifecycle lock 与 root restart lock 时启动 runner。
///
/// 普通提交的后台 finalizer 必须使用这条路径：如果 Continue 先取得生命周期锁，旧
/// finalizer 就不能在新 Turn 上继续执行；如果 finalizer 先取得锁，Continue 会等待旧
/// runner 完整注册并在恢复前 quiesce。这样“accepted 但尚未登记 runner”的窗口不会
/// 再与恢复流程竞态。
pub(crate) fn drive_dispatch_submission_after_lifecycle_and_restart_lock(
    state: &ApiState,
    accepted: &mut DispatchSubmissionAccepted,
) -> Result<(), ApiError> {
    let manager = state
        .runner_manager()
        .ok_or_else(|| ApiError::internal_assembly("驱动任务派发", "runner_manager 未配置"))?;
    match manager.start_after_quiesce(
        accepted.root_task_id.as_str(),
        Some(accepted.session_id.clone()),
    ) {
        Ok(_) | Err(RunnerStartError::AlreadyRunning) => {}
        Err(RunnerStartError::NotFound) => {
            return Err(ApiError::internal_assembly("驱动任务派发", "根任务不存在"));
        }
        Err(RunnerStartError::SessionUnavailable) => {
            return Err(ApiError::InvalidInput(
                "当前会话已关闭，不能启动任务执行".to_string(),
            ));
        }
    }
    accepted.runner_started = true;
    Ok(())
}

fn dispatch_request_fingerprint(request: &DispatchSubmissionRequest) -> String {
    let bytes = serde_json::to_vec(request).expect("DispatchSubmissionRequest must serialize");
    let digest = Sha256::digest(bytes);
    format!("sha256:{}", magi_core::hex_lower(digest))
}

fn dispatch_accept_error_to_api_error(error: DispatchSubmissionAcceptError) -> ApiError {
    match error {
        DispatchSubmissionAcceptError::Conflict { message } => ApiError::Conflict(message),
        DispatchSubmissionAcceptError::Internal { message } => {
            ApiError::InternalAssemblyError(format!("任务派发接受失败: {}", message))
        }
    }
}

pub(crate) async fn accept_session_task_submission(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    images: Vec<SessionTurnImage>,
    workspace_id: Option<WorkspaceId>,
    task_title: Option<String>,
    execution_goal: Option<String>,
    task_tier: TaskTier,
) -> Result<(DispatchSubmissionAccepted, EventId), ApiError> {
    accept_session_task_submission_at(
        state,
        request,
        SessionTaskSubmissionInput {
            creation_request_fingerprint: None,
            images,
            workspace_id,
            task_title,
            execution_goal,
            task_tier,
            use_tools: true,
            accepted_at: monotonic_accepted_at(),
            required_tool_chain: Vec::new(),
            completion_contract: TaskCompletionContract::default(),
            recovery_checkpoint: None,
            denied_tools: Vec::new(),
            user_message_metadata: Default::default(),
        },
    )
    .await
    .map(|(accepted, event_id, _event_seq, _occurred_at)| (accepted, event_id))
}

pub(crate) struct SessionTaskSubmissionInput {
    /// 已接纳输入的创建身份；排队阶段补齐的服务端 item ID 不得改写它。
    pub creation_request_fingerprint: Option<String>,
    pub images: Vec<SessionTurnImage>,
    pub workspace_id: Option<WorkspaceId>,
    pub task_title: Option<String>,
    pub execution_goal: Option<String>,
    pub task_tier: TaskTier,
    pub use_tools: bool,
    pub accepted_at: UtcMillis,
    pub required_tool_chain: Vec<String>,
    pub completion_contract: TaskCompletionContract,
    pub recovery_checkpoint: Option<TaskRecoveryCheckpoint>,
    pub denied_tools: Vec<String>,
    /// API 语义元数据与幂等指纹一起原子写入 canonical user item。
    pub user_message_metadata: std::collections::HashMap<String, serde_json::Value>,
}

pub(crate) async fn accept_session_task_submission_at(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    input: SessionTaskSubmissionInput,
) -> Result<(DispatchSubmissionAccepted, EventId, u64, UtcMillis), ApiError> {
    let SessionTaskSubmissionInput {
        creation_request_fingerprint,
        images,
        workspace_id,
        task_title,
        execution_goal,
        task_tier,
        use_tools,
        accepted_at,
        required_tool_chain,
        completion_contract,
        recovery_checkpoint,
        denied_tools,
        mut user_message_metadata,
    } = input;
    // 已保存的 GPT Web 对话：接受本轮用户消息前，先把 Web 侧 Magi 尚未同步的内容单向导入。
    // 同步失败（对话不存在 / 冲突 / 槽位被占用）直接拒绝本轮，不写入用户消息。
    if let Some(session_id) = request.requested_session_id() {
        state
            .sync_saved_web_conversation(&session_id)
            .await
            .map_err(crate::web_model_ops::web_model_api_error)?;
    }
    let request_fingerprint = match creation_request_fingerprint {
        Some(fingerprint) => fingerprint,
        None => request
            .request_fingerprint()
            .map_err(ApiError::InvalidInput)?,
    };
    let trace_id = request
        .request_id()
        .unwrap_or_else(|| format!("trace-turn-{}", accepted_at.0));
    user_message_metadata.insert(
        "requestFingerprint".to_string(),
        serde_json::Value::String(request_fingerprint),
    );
    user_message_metadata.insert("traceId".to_string(), serde_json::Value::String(trace_id));
    if let Some(workflow_id) = request.workflow_id.as_deref() {
        user_message_metadata.insert(
            "magi.workflowId".to_string(),
            serde_json::Value::String(workflow_id.to_string()),
        );
    }
    let trimmed_text = request.trimmed_text();
    let message = request.timeline_content(trimmed_text.as_deref());
    let mission_title = task_title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| request.mission_title(trimmed_text.as_deref()));
    let execution_goal = execution_goal
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    execute_dispatch_submission(
        state,
        ExecuteDispatchSubmissionInput {
            requested_session_id: request.requested_session_id(),
            requested_workspace_id: workspace_id,
            mission_title,
            message,
            trimmed_text,
            execution_goal,
            task_tier,
            use_tools,
            skill_name: request.skill_name.clone(),
            target_role: None,
            images,
            request,
            accepted_at,
            required_tool_chain,
            completion_contract,
            recovery_checkpoint,
            denied_tools,
            user_message_metadata,
        },
    )
    .await
}

/// Goal 自动续跑同样提交 root coordinator task，但不伪造用户消息。
///
/// 它和普通主线 Turn 共用 DispatchSubmission / Runner / ConversationLoop；仅以
/// `DispatchTurnOrigin::GoalContinuation` 让 canonical 时间线写入系统通知，并保证
/// 当前 Turn 不出现虚假的 user_message。
fn goal_directory_binding(
    state: &ApiState,
    goal: &SessionGoal,
) -> Result<Option<magi_core::ProjectDirectoryBinding>, ApiError> {
    let Some(owner) = goal.created_by_turn_id.as_deref() else {
        return Ok(None);
    };
    // 原生 Goal 工具以 Task ID 保存创建者，普通 Conversation 则使用 Turn ID。
    if let Some(task) = state
        .task_store()
        .and_then(|tasks| tasks.get_task(&magi_core::TaskId::new(owner)))
    {
        return Ok(task
            .executor_binding
            .and_then(|binding| binding.project_directories));
    }
    let original = state
        .session_store
        .canonical_turns_for_session(&goal.session_id)
        .into_iter()
        .find(|turn| {
            turn.turn_id == owner
                || turn.items.iter().any(|item| {
                    item.worker
                        .as_ref()
                        .and_then(|worker| worker.task_id.as_ref())
                        .is_some_and(|id| id.as_str() == owner)
                })
        });
    original
        .as_ref()
        .map(canonical_project_directory_binding)
        .transpose()
        .map(Option::flatten)
        .map_err(|error| ApiError::internal_assembly("恢复 Goal 原目录绑定", error))
}

pub(crate) async fn accept_goal_continuation_task_submission(
    state: &ApiState,
    session_id: SessionId,
    workspace_id: Option<WorkspaceId>,
    goal: &SessionGoal,
    execution_goal: String,
    accepted_at: UtcMillis,
) -> Result<DispatchSubmissionAccepted, ApiError> {
    let execution_root = workspace_id
        .is_none()
        .then(|| state.personal_session_execution_root_path(&session_id));
    let entry_id = format!(
        "timeline-goal-continuation-{}-{}",
        session_id, accepted_at.0
    );
    let dispatch = DispatchSubmissionRequest {
        project_directories: goal_directory_binding(state, goal)?,
        accepted_at,
        session_id: session_id.clone(),
        workspace_id,
        execution_root,
        orchestrator_session_config: None,
        entry_id,
        timeline_message: format!("目标自动推进: {}", goal.objective),
        images: Vec::new(),
        context_references: Vec::new(),
        browser_annotation_refs: Vec::new(),
        browser_node_selections: Vec::new(),
        plugin_selections: Vec::new(),
        created_session: false,
        mission_title: "目标自动推进".to_string(),
        task_title: "执行: 目标自动推进".to_string(),
        trimmed_text: None,
        execution_goal: Some(execution_goal),
        task_tier: TaskTier::ExecutionChain,
        access_profile: goal.access_profile,
        skill_name: None,
        goal_mode: true,
        use_tools: true,
        target_role: None,
        workflow_id: None,
        request_id: None,
        user_message_id: None,
        placeholder_message_id: None,
        replace_turn_id: None,
        required_tool_chain: vec!["get_goal".to_string()],
        completion_contract: TaskCompletionContract::default(),
        recovery_checkpoint: None,
        denied_tools: Vec::new(),
        user_message_metadata: Default::default(),
        turn_origin: DispatchTurnOrigin::GoalContinuation(goal.goal_id.clone()),
    };
    let accepted = submit_dispatch_submission(state, dispatch)?;
    Ok(accepted)
}

struct ExecuteDispatchSubmissionInput<'a> {
    requested_session_id: Option<SessionId>,
    requested_workspace_id: Option<WorkspaceId>,
    mission_title: String,
    message: String,
    trimmed_text: Option<String>,
    execution_goal: Option<String>,
    task_tier: TaskTier,
    use_tools: bool,
    skill_name: Option<String>,
    target_role: Option<String>,
    images: Vec<SessionTurnImage>,
    request: &'a SessionTurnRequestDto,
    accepted_at: UtcMillis,
    required_tool_chain: Vec<String>,
    completion_contract: TaskCompletionContract,
    recovery_checkpoint: Option<TaskRecoveryCheckpoint>,
    denied_tools: Vec<String>,
    user_message_metadata: std::collections::HashMap<String, serde_json::Value>,
}

async fn execute_dispatch_submission(
    state: &ApiState,
    input: ExecuteDispatchSubmissionInput<'_>,
) -> Result<(DispatchSubmissionAccepted, EventId, u64, UtcMillis), ApiError> {
    let ExecuteDispatchSubmissionInput {
        requested_session_id,
        requested_workspace_id,
        mission_title,
        message,
        trimmed_text,
        execution_goal,
        task_tier,
        use_tools,
        skill_name,
        target_role,
        images,
        request,
        accepted_at,
        required_tool_chain,
        completion_contract,
        recovery_checkpoint,
        denied_tools,
        user_message_metadata,
    } = input;
    let placeholder_title = crate::session_title::NEW_SESSION_PLACEHOLDER_TITLE;
    let previous_current_session_id = state.session_store.current_session_id();
    let (session_id, created_session, workspace_id) = resolve_turn_session(
        state,
        requested_session_id,
        requested_workspace_id,
        placeholder_title,
        accepted_at,
    )?;
    // 项目 cwd 由 submit_dispatch_submission 沿冻结绑定统一解析。
    let execution_root = workspace_id
        .is_none()
        .then(|| state.personal_session_execution_root_path(&session_id));
    let browser_annotation_refs = match resolve_browser_annotation_context(
        state,
        &session_id,
        &request.browser_annotation_refs(),
    ) {
        Ok(refs) => refs,
        Err(error) if created_session => {
            return Err(rollback_created_session_on_dispatch_error(
                state,
                &session_id,
                previous_current_session_id.clone(),
                error,
            )
            .await);
        }
        Err(error) => return Err(error),
    };
    let browser_node_selection_result = {
        let browser_authority = state
            .browser_authority
            .lock()
            .expect("browser authority lock poisoned");
        request.validate_browser_node_selections_with(|index, selection| {
            let browser_session_id = BrowserSessionId::new(selection.browser_session_id.clone());
            let tab_id = BrowserTabId::new(selection.tab_id.clone());
            browser_authority
                .validate_browser_node_selection(ValidateBrowserNodeSelection {
                    session_id: &session_id,
                    browser_session_id: &browser_session_id,
                    tab_id: &tab_id,
                    surface_id: selection.surface_id.trim(),
                    navigation_revision: selection.navigation_revision,
                    page_url: selection.url.trim(),
                })
                .map_err(|error| format!("浏览器节点选择[{index}] 无效: {error}"))
        })
    };
    let browser_node_selections = match browser_node_selection_result {
        Ok(selections) => selections,
        Err(error) if created_session => {
            return Err(rollback_created_session_on_dispatch_error(
                state,
                &session_id,
                previous_current_session_id.clone(),
                ApiError::InvalidInput(error),
            )
            .await);
        }
        Err(error) => return Err(ApiError::InvalidInput(error)),
    };
    let plugin_selection_scope = magi_plugin_system::APPLICATION_SCOPE;
    let plugin_selection_result = request.validate_plugin_selections_with(|selection| {
        let manager = state
            .plugin_manager
            .lock()
            .map_err(|_| "插件管理器不可用".to_string())?;
        // 读取资源会同时核对：作用域已激活、资源由清单声明、存储权限已授予。
        let current = manager
            .read_resource(
                &selection.plugin_id,
                plugin_selection_scope,
                &selection.resource_id,
            )
            .map_err(|error| error.to_string())?;
        if selection.version > current.version {
            return Err("选中对象的资源版本高于当前版本".to_string());
        }
        Ok(())
    });
    let plugin_selections = match plugin_selection_result {
        Ok(selections) => selections,
        Err(error) if created_session => {
            return Err(rollback_created_session_on_dispatch_error(
                state,
                &session_id,
                previous_current_session_id.clone(),
                ApiError::InvalidInput(error),
            )
            .await);
        }
        Err(error) => return Err(ApiError::InvalidInput(error)),
    };
    let user_timeline_entry_id = format!("timeline-{}-{}", session_id, accepted_at.0);
    let action_task_title = format_action_task_title(&mission_title);

    let dispatch = DispatchSubmissionRequest {
        project_directories: request.project_directories.clone(),
        accepted_at,
        session_id: session_id.clone(),
        workspace_id: workspace_id.clone(),
        execution_root,
        orchestrator_session_config: request.orchestrator_session_config.clone(),
        entry_id: user_timeline_entry_id,
        timeline_message: message.clone(),
        images,
        context_references: request.context_references(),
        browser_annotation_refs,
        browser_node_selections,
        plugin_selections,
        created_session,
        mission_title,
        task_title: action_task_title,
        trimmed_text,
        execution_goal,
        task_tier,
        access_profile: request.requested_access_profile(),
        skill_name,
        goal_mode: request.goal_mode,
        use_tools,
        target_role,
        workflow_id: user_message_metadata
            .get("magi.workflowId")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        request_id: request.request_id(),
        user_message_id: request.user_message_id(),
        placeholder_message_id: request.placeholder_message_id(),
        replace_turn_id: request.replace_turn_id(),
        required_tool_chain,
        completion_contract,
        recovery_checkpoint,
        denied_tools,
        turn_origin: DispatchTurnOrigin::User,
        user_message_metadata,
    };
    let accepted = match submit_dispatch_submission(state, dispatch) {
        Ok(accepted) => accepted,
        Err(error) if created_session => {
            return Err(rollback_created_session_on_dispatch_error(
                state,
                &session_id,
                previous_current_session_id.clone(),
                error,
            )
            .await);
        }
        Err(error) => {
            state.release_session_git_execution_lease(&session_id);
            return Err(error);
        }
    };
    publish_session_user_message_event(state, &session_id, workspace_id.clone(), &message);
    if let Some(superseded_turn) = accepted.superseded_turn.as_ref() {
        publish_superseded_turn_event(state, &session_id, workspace_id.as_ref(), superseded_turn);
    }
    let (event_id, event_seq, event_occurred_at) =
        match publish_session_turn_task_accepted_event(state, request, &accepted) {
            Ok(event) => event,
            Err(error) if created_session => {
                return Err(rollback_created_session_on_dispatch_error(
                    state,
                    &session_id,
                    previous_current_session_id,
                    error,
                )
                .await);
            }
            Err(error) => return Err(error),
        };
    if created_session {
        crate::session_title::spawn_new_session_title_refinement(
            state,
            &session_id,
            &message,
            placeholder_title,
        );
    }
    Ok((accepted, event_id, event_seq, event_occurred_at))
}

/// 新会话只有在首条消息完成接纳并发布事实后才算成立。
///
/// `execute_dispatch_submission` 在创建新会话后仍可能失败于引用校验、任务接纳或
/// 事实发布。所有这些失败都必须删除同一会话的任务、sidecar、队列、执行注册和
/// 浏览器资源，并恢复创建前的 current；否则前端会看到“会话消失”，后台却留下
/// 一个污染后续导航的空会话。
async fn rollback_created_session_on_dispatch_error(
    state: &ApiState,
    session_id: &SessionId,
    previous_current_session_id: Option<SessionId>,
    error: ApiError,
) -> ApiError {
    let original_message = error.message().to_string();
    state.release_session_git_execution_lease(session_id);
    match state
        .rollback_created_session_after_navigation_lock(session_id, previous_current_session_id)
        .await
    {
        Ok(()) => error,
        Err(cleanup_error) => ApiError::internal_assembly(
            "创建会话失败且回滚失败",
            format!("{original_message}；回滚失败: {cleanup_error:?}"),
        ),
    }
}

pub(crate) fn dispatch_accepted_canonical_event(
    accepted: &DispatchSubmissionAccepted,
) -> (Option<CanonicalTurn>, Option<CanonicalTurnItem>) {
    let canonical_turn = accepted.accepted_canonical_turn.clone();
    let canonical_item = canonical_turn
        .as_ref()
        .and_then(|turn| {
            turn.items.iter().find(|item| {
                accepted
                    .user_message_item_id
                    .as_ref()
                    .is_some_and(|item_id| item.item_id == *item_id)
            })
        })
        .or_else(|| {
            canonical_turn.as_ref().and_then(|turn| {
                turn.items
                    .iter()
                    .find(|item| item.kind == CanonicalTurnItemKind::UserMessage)
            })
        })
        .cloned();
    (canonical_turn, canonical_item)
}

pub(crate) fn accepted_session_directory_entry(
    state: &ApiState,
    accepted: &DispatchSubmissionAccepted,
) -> Option<SessionDirectoryEntryDto> {
    if !accepted.created_session {
        return None;
    }
    let session = state.session_store.session(&accepted.session_id)?;
    Some(SessionDirectoryEntryDto::from_record(session, true, 1))
}

pub(crate) fn publish_goal_continuation_task_accepted_event(
    state: &ApiState,
    accepted: &DispatchSubmissionAccepted,
) -> EventId {
    let workspace_id = state
        .session_store
        .execution_ownership(&accepted.session_id)
        .and_then(|ownership| ownership.workspace_id);
    let (canonical_turn, canonical_item) = dispatch_accepted_canonical_event(accepted);
    let metadata = canonical_turn.as_ref().map(|turn| &turn.metadata);
    let request_id = metadata
        .and_then(|metadata| metadata.get("requestId"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let request_fingerprint = metadata
        .and_then(|metadata| metadata.get("requestFingerprint"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let attempt_id = metadata
        .and_then(|metadata| metadata.get("attemptId"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let session_summary = accepted_session_directory_entry(state, accepted);
    // 由 turn 身份推导：turn/start 重放时按同一规则找回这条事件的序号。
    let event_id = EventId::new(format!("event-session-turn-task-{}", accepted.turn_id));
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.turn.task.accepted",
        json!({
            "session_id": accepted.session_id,
            "entry_id": accepted.entry_id,
            "accepted_at": accepted.accepted_at.0,
            "workspace_id": workspace_id.as_ref().map(ToString::to_string),
            "text": serde_json::Value::Null,
            "skill_name": serde_json::Value::Null,
            "request_id": request_id,
            "request_fingerprint": request_fingerprint,
            "attempt_id": attempt_id,
            "user_message_id": canonical_item
                .as_ref()
                .and_then(|item| item.metadata.get("userMessageId"))
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            "placeholder_message_id": canonical_item
                .as_ref()
                .and_then(|item| item.metadata.get("placeholderMessageId"))
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            "image_count": 0,
            "created_session": false,
            "session_summary": session_summary,
            "route": "task",
            "goal_continuation": true,
            "root_task_id": accepted.root_task_id.to_string(),
            "action_task_id": accepted.action_task_id.to_string(),
            "runner_started": accepted.runner_started,
            "canonical_schema_version": CANONICAL_TURN_SCHEMA_VERSION,
            "canonical_event_kind": "turn_started",
            "canonical_turn": canonical_turn,
            "canonical_item": canonical_item,
        }),
    )
    .with_context(EventContext {
        session_id: Some(accepted.session_id.clone()),
        workspace_id,
        ..EventContext::default()
    });
    state.event_bus.publish(event);
    event_id
}

/// 在 accepted 事件已经发出后执行所有可能访问磁盘、Git 或模型的准备工作。
///
/// 提交控制面不能等待这段流程：workspace snapshot、Git 观测和上下文准备都属于
/// 可恢复的执行面。失败仍然沿原 Turn 收口，不能再创建第二条错误消息链。
async fn prepare_session_task_dispatch(
    state: &ApiState,
    accepted: &DispatchSubmissionAccepted,
    coordinator_attempt: &magi_conversation_runtime::TurnAttempt,
) -> Result<(), ApiError> {
    let trace_id = state
        .session_store
        .runtime_sidecar(&accepted.session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .and_then(|turn| {
            turn.items.into_iter().find_map(|item| {
                item.metadata
                    .get("traceId")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
        });
    let trace = PerformanceTrace::new(trace_id.as_deref(), accepted.accepted_at);
    trace.mark(
        "preparation_started",
        accepted.session_id.as_str(),
        Some(&accepted.turn_id),
        None,
    );
    let coordinator_is_running = matches!(
        state
            .turn_coordinator()
            .current_status(&accepted.session_id, &accepted.turn_id),
        Ok(magi_conversation_runtime::CoordinatorTurnStatus::Running)
    );
    if !coordinator_is_running {
        state
            .turn_coordinator()
            .execute_command(
                &accepted.session_id,
                magi_conversation_runtime::TurnCommand::SetStatus {
                    attempt: coordinator_attempt.clone(),
                    status: magi_conversation_runtime::CoordinatorTurnStatus::Preparing,
                },
            )
            .map_err(|error| {
                ApiError::internal_assembly("更新任务 Coordinator 准备状态失败", error)
            })?;
        state
            .turn_event_sink()
            .set_status_domain(&accepted.session_id, Some(&accepted.turn_id), "preparing")
            .map_err(|error| ApiError::internal_assembly("更新任务准备状态失败", error))?
            .ok_or_else(|| ApiError::Conflict("当前任务 Turn 已被新的操作取代".to_string()))?;
    }
    trace.mark(
        "preparation_turn_status_written",
        accepted.session_id.as_str(),
        Some(&accepted.turn_id),
        None,
    );
    if let Some(item_id) = accepted.user_message_item_id.as_deref() {
        publish_current_session_turn_item_event(
            &state.event_bus,
            &state.session_store,
            &accepted.session_id,
            &state
                .session_store
                .execution_ownership(&accepted.session_id)
                .and_then(|ownership| ownership.workspace_id),
            item_id,
            state.task_store(),
        )
        .map_err(|error| ApiError::internal_assembly("发布任务用户消息事实失败", error))?;
    }
    trace.mark(
        "preparation_user_message_published",
        accepted.session_id.as_str(),
        Some(&accepted.turn_id),
        None,
    );

    if let Some(config) = initial_session_orchestrator_config(
        state,
        accepted.created_session,
        accepted.request.orchestrator_session_config.as_ref(),
    )? {
        crate::model_config::save_initial_orchestrator_session_override_for_new_session(
            state,
            &accepted.session_id,
            &config,
        )?;
        crate::model_config::require_orchestrator_session_model(state, &accepted.session_id)?;
    }
    trace.mark(
        "preparation_model_configured",
        accepted.session_id.as_str(),
        Some(&accepted.turn_id),
        None,
    );
    if accepted.request.workspace_id.is_none() {
        state.personal_session_execution_root(&accepted.session_id)?;
    }
    if accepted.request.goal_mode {
        state
            .session_store
            .set_active_goal_access_profile(&accepted.session_id, accepted.request.access_profile)
            .map_err(|error| ApiError::internal_assembly("更新 active goal 访问模式失败", error))?;
    } else if let Some((_goal, plan)) = state
        .session_store
        .pause_active_goal_for_diversion(&accepted.session_id)
        .map_err(|error| ApiError::internal_assembly("切换任务时暂停当前 Goal 失败", error))?
        && let Some(plan) = plan.as_ref()
    {
        magi_plan::publish_plan_event(
            &state.event_bus,
            magi_plan::plan_event_type(plan),
            plan,
            accepted.request.workspace_id.as_ref(),
            None,
            None,
        );
    }
    trace.mark(
        "preparation_goal_state_updated",
        accepted.session_id.as_str(),
        Some(&accepted.turn_id),
        None,
    );
    state
        .settle_revoked_goal_agent_runs(&accepted.session_id)
        .await?;

    // 工作区扫描与 Git 执行租约由首次本地工具调用触发，模型准备阶段不做全仓扫描。
    materialize_dispatch_submission_after_acceptance(state, accepted)?;
    trace.mark(
        "preparation_execution_materialized",
        accepted.session_id.as_str(),
        Some(&accepted.turn_id),
        None,
    );
    state.persist_session_state_checkpoint("session_task_turn_prepared")?;
    trace.mark(
        "preparation_completed",
        accepted.session_id.as_str(),
        Some(&accepted.turn_id),
        None,
    );
    Ok(())
}

async fn finalize_session_task_dispatch(state: ApiState, accepted: DispatchSubmissionAccepted) {
    let mut accepted = accepted;
    if state.runner_manager().is_none() {
        fail_accepted_task_submission(&state, &accepted, "runner_manager 未配置");
        return;
    }
    // accepted 事实写入后，执行面仍可能尚未开始。把 finalizer 纳入与 Continue/Restart
    // 相同的生命周期锁顺序，确保新旧 Turn 不会同时进入同一 session 的执行入口。
    let _session_lifecycle_guard = state.lock_session_lifecycle(&accepted.session_id).await;
    let manager = state
        .runner_manager()
        .expect("runner_manager 已在生命周期锁前校验存在");
    let _restart_guard = manager
        .lock_for_restart(accepted.root_task_id.as_str())
        .await;
    let current_turn_matches = state
        .session_store
        .runtime_sidecar(&accepted.session_id)
        .and_then(|sidecar| sidecar.current_turn)
        .is_some_and(|turn| turn.turn_id == accepted.turn_id);
    if !current_turn_matches {
        tracing::info!(
            session_id = %accepted.session_id,
            root_task_id = %accepted.root_task_id,
            turn_id = %accepted.turn_id,
            "跳过已被更新 Turn 取代的后台 session finalizer"
        );
        return;
    }
    let coordinator_attempt = match state
        .turn_coordinator()
        .current_attempt(&accepted.session_id, &accepted.turn_id)
    {
        Ok(attempt) => attempt,
        Err(error) => {
            tracing::error!(
                session_id = %accepted.session_id,
                turn_id = %accepted.turn_id,
                %error,
                "任务准备前读取 Coordinator attempt 失败"
            );
            fail_accepted_task_submission(&state, &accepted, &error.to_string());
            return;
        }
    };
    if let Err(error) = prepare_session_task_dispatch(&state, &accepted, &coordinator_attempt).await
    {
        state.release_session_git_execution_lease(&accepted.session_id);
        tracing::error!(
            session_id = %accepted.session_id,
            root_task_id = %accepted.root_task_id,
            ?error,
            "session turn preparation failed"
        );
        fail_accepted_task_submission(&state, &accepted, error.message());
        return;
    }
    if let Err(error) = state.turn_coordinator().execute_command(
        &accepted.session_id,
        magi_conversation_runtime::TurnCommand::SetStatus {
            attempt: coordinator_attempt.clone(),
            status: magi_conversation_runtime::CoordinatorTurnStatus::Running,
        },
    ) {
        tracing::error!(
            session_id = %accepted.session_id,
            turn_id = %accepted.turn_id,
            %error,
            "更新任务 Coordinator 运行状态失败"
        );
        fail_accepted_task_submission(&state, &accepted, &error.to_string());
        return;
    }
    match state.turn_event_sink().set_status_domain(
        &accepted.session_id,
        Some(&accepted.turn_id),
        "running",
    ) {
        Ok(Some(_)) => {}
        Ok(None) => {
            let error = "当前任务 Turn 已被新的操作取代";
            tracing::error!(
                session_id = %accepted.session_id,
                root_task_id = %accepted.root_task_id,
                turn_id = %accepted.turn_id,
                "session turn running 状态写回时未找到当前 Turn"
            );
            fail_accepted_task_submission(&state, &accepted, error);
            return;
        }
        Err(error) => {
            tracing::error!(
                session_id = %accepted.session_id,
                root_task_id = %accepted.root_task_id,
                ?error,
                "session turn running 状态写回失败"
            );
            fail_accepted_task_submission(&state, &accepted, &error.to_string());
            return;
        }
    }
    if let Err(error) =
        drive_dispatch_submission_after_lifecycle_and_restart_lock(&state, &mut accepted)
    {
        tracing::error!(
            session_id = %accepted.session_id,
            root_task_id = %accepted.root_task_id,
            action_task_id = %accepted.action_task_id,
            ?error,
            "session turn task dispatch failed"
        );
        fail_accepted_task_submission(&state, &accepted, error.message());
    }
}

/// 只把执行面投递到后台；调用方已经完成最小 durable accepted 写入。
pub(crate) fn schedule_session_task_dispatch(
    state: ApiState,
    accepted: DispatchSubmissionAccepted,
) {
    tokio::spawn(async move {
        finalize_session_task_dispatch(state, accepted).await;
    });
}

/// daemon 重启后恢复已经 durable accepted、但尚未开始 Runner 的主线 Turn。
///
/// 只恢复 `accepted/preparing + pending root task`，已经进入模型执行的轮次仍由
/// daemon 的中断收口逻辑处理，避免重复发送上游请求。
pub fn schedule_restored_session_task_dispatches(state: ApiState) {
    let Some(task_store) = state.task_store() else {
        return;
    };
    let restored = state
        .session_store
        .runtime_sidecars()
        .into_iter()
        .filter_map(|sidecar| {
            let turn = sidecar.current_turn.as_ref()?;
            if !matches!(turn.status.as_str(), "accepted" | "preparing") {
                return None;
            }
            let chain = sidecar.active_execution_chain.as_ref()?;
            let root_task = task_store.get_task(&chain.root_task_id)?;
            if root_task.status != TaskStatus::Pending {
                return None;
            }
            let request = match recover_dispatch_submission_request(&sidecar) {
                Ok(request) => request,
                Err(error) => {
                    tracing::error!(
                        session_id = %sidecar.session_id,
                        turn_id = %turn.turn_id,
                        %error,
                        "无法恢复 accepted Turn 的完整派发请求"
                    );
                    // 不能让无法恢复的 accepted Turn 永远占住会话：标记失败并释放执行链。
                    fail_accepted_task_turn(
                        &state,
                        &sidecar.session_id,
                        &chain.root_task_id,
                        &turn.turn_id,
                        &format!("重启后无法恢复本轮请求，请重新发送：{error}"),
                    );
                    return None;
                }
            };
            let action_task_id = chain
                .branches
                .iter()
                .find(|branch| branch.is_primary)
                .map(|branch| branch.task_id.clone())
                .unwrap_or_else(|| chain.root_task_id.clone());
            let user_message_item_id = turn
                .items
                .iter()
                .find(|item| item.kind == "user_message")
                .map(|item| item.item_id.clone());
            Some(DispatchSubmissionAccepted {
                request,
                session_id: sidecar.session_id.clone(),
                entry_id: chain.dispatch_context.entry_id.clone(),
                accepted_at: chain.dispatch_context.accepted_at,
                created_session: false,
                root_task_id: chain.root_task_id.clone(),
                action_task_id,
                turn_id: turn.turn_id.clone(),
                user_message_item_id,
                accepted_canonical_turn: state
                    .session_store
                    .canonical_turn_for_session_turn_id(&sidecar.session_id, &turn.turn_id),
                runner_started: false,
                superseded_turn: None,
            })
        })
        .collect::<Vec<_>>();
    for accepted in restored {
        tracing::info!(
            session_id = %accepted.session_id,
            root_task_id = %accepted.root_task_id,
            "恢复 daemon 重启前已接纳但尚未启动的 session Turn"
        );
        schedule_session_task_dispatch(state.clone(), accepted);
    }
}

fn fail_accepted_task_submission(
    state: &ApiState,
    accepted: &DispatchSubmissionAccepted,
    direct_error: &str,
) {
    fail_accepted_task_turn(
        state,
        &accepted.session_id,
        &accepted.root_task_id,
        &accepted.turn_id,
        direct_error,
    );
}

/// 已 durable accepted 的主线 Turn 无法开始执行时的唯一失败收口：根任务写入 Failed，
/// 再由统一终态入口收口 Turn 并释放执行链。
fn fail_accepted_task_turn(
    state: &ApiState,
    session_id: &SessionId,
    root_task_id: &magi_core::TaskId,
    turn_id: &str,
    direct_error: &str,
) {
    let mut direct_error = public_runtime_excerpt(direct_error, 4096);
    let Some(task_store) = state.task_store() else {
        tracing::error!(
            session_id = %session_id,
            root_task_id = %root_task_id,
            "accepted task 启动失败时 task_store 未配置，无法写入失败事实"
        );
        return;
    };
    let Some(task) = task_store.get_task(root_task_id) else {
        tracing::error!(
            session_id = %session_id,
            root_task_id = %root_task_id,
            "accepted task 启动失败时根任务不存在，无法写入失败事实"
        );
        return;
    };
    if task.status == TaskStatus::Pending
        && let Err(error) = cleanup_materialized_dispatch_submission_if_not_started(
            &state.session_store,
            state.task_execution_registry(),
            session_id,
            root_task_id,
            turn_id,
        )
    {
        direct_error = format!("{direct_error}；清理未启动执行资源失败: {error}");
    }
    let lease_id = task_store
        .get_active_lease(root_task_id)
        .map(|lease| lease.lease_id);
    let terminalized = match task_store.revoke_lease_and_set_task_terminal(
        root_task_id,
        &task.root_task_id,
        lease_id.as_ref(),
        TaskStatus::Failed,
        vec![direct_error],
    ) {
        Ok(changed) => changed,
        Err(error) => {
            tracing::error!(
                session_id = %session_id,
                root_task_id = %root_task_id,
                ?error,
                "accepted task 启动失败时写入 Failed 终态失败，等待 durable accepted 状态恢复"
            );
            return;
        }
    };
    let already_terminal = task_store.get_task(root_task_id).is_some_and(|current| {
        matches!(
            current.status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
        )
    });
    if !(terminalized || already_terminal) {
        tracing::error!(
            session_id = %session_id,
            root_task_id = %root_task_id,
            "accepted task 启动失败未能提交根任务终态，等待恢复"
        );
        return;
    }
    match crate::task_turn_finalize::finalize_background_session_task_turn_if_root_terminal_for_turn(
        state,
        session_id,
        root_task_id,
        "error",
        Some(turn_id),
    ) {
        Ok(true) => {}
        Ok(false) => tracing::error!(
            session_id = %session_id,
            root_task_id = %root_task_id,
            turn_id = %turn_id,
            "accepted task 已进入终态，但当前 Turn 未能完成统一收口，等待恢复"
        ),
        Err(error) => tracing::error!(
            session_id = %session_id,
            root_task_id = %root_task_id,
            turn_id = %turn_id,
            %error,
            "accepted task 终态 Turn 统一收口失败，等待恢复"
        ),
    }
}

fn format_action_task_title(mission_title: &str) -> String {
    let title = mission_title.trim();
    if title.starts_with("执行:") || title.starts_with("执行：") {
        title.to_string()
    } else {
        format!("执行: {title}")
    }
}

fn publish_session_turn_task_accepted_event(
    state: &ApiState,
    request: &SessionTurnRequestDto,
    accepted: &DispatchSubmissionAccepted,
) -> Result<(EventId, u64, UtcMillis), ApiError> {
    let workspace_id = state
        .session_store
        .execution_ownership(&accepted.session_id)
        .and_then(|ownership| ownership.workspace_id)
        .or_else(|| request.requested_workspace_id().map(WorkspaceId::new));
    let workspace_id_payload = workspace_id.as_ref().map(ToString::to_string);
    let (canonical_turn, canonical_item) = dispatch_accepted_canonical_event(accepted);
    let session_summary = accepted_session_directory_entry(state, accepted);
    // 由 turn 身份推导：turn/start 重放时按同一规则找回这条事件的序号。
    let event_id = EventId::new(format!("event-session-turn-task-{}", accepted.turn_id));
    let event = EventEnvelope::domain(
        event_id.clone(),
        "session.turn.task.accepted",
        json!({
            "session_id": accepted.session_id,
            "entry_id": accepted.entry_id,
            "accepted_at": accepted.accepted_at.0,
            "workspace_id": workspace_id_payload,
            "text": request.trimmed_text(),
            "skill_name": request.skill_name.clone(),
            "request_id": request.request_id(),
            "user_message_id": request.user_message_id(),
            "placeholder_message_id": request.placeholder_message_id(),
            "image_count": request.images.len(),
            "created_session": accepted.created_session,
            "session_summary": session_summary,
            "route": "task",
            "root_task_id": accepted.root_task_id.to_string(),
            "action_task_id": accepted.action_task_id.to_string(),
            "runner_started": accepted.runner_started,
            "canonical_schema_version": CANONICAL_TURN_SCHEMA_VERSION,
            "canonical_event_kind": "turn_started",
            "canonical_turn": canonical_turn,
            "canonical_item": canonical_item,
        }),
    )
    .with_context(EventContext {
        session_id: Some(accepted.session_id.clone()),
        workspace_id,
        ..EventContext::default()
    });
    let event_occurred_at = event.occurred_at;
    let event_seq = state.event_bus.publish(event);
    Ok((event_id, event_seq, event_occurred_at))
}

fn publish_superseded_turn_event(
    state: &ApiState,
    session_id: &SessionId,
    workspace_id: Option<&WorkspaceId>,
    turn: &CanonicalTurn,
) {
    state.event_bus.publish(
        EventEnvelope::domain(
            magi_core::EventId::unique(format!("event-session-turn-superseded-{}", turn.turn_id)),
            "session.turn.superseded",
            serde_json::json!({
                "session_id": session_id,
                "workspace_id": workspace_id,
                "canonical_schema_version": CANONICAL_TURN_SCHEMA_VERSION,
                "canonical_event_kind": "turn_superseded",
                "canonical_turn": turn,
            }),
        )
        .with_context(EventContext {
            session_id: Some(session_id.clone()),
            workspace_id: workspace_id.cloned(),
            ..EventContext::default()
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::{
        fail_accepted_task_submission, format_action_task_title,
        initial_session_orchestrator_config, resolve_turn_session,
    };
    use crate::{
        errors::ApiError,
        state::ApiState,
        task_dispatch::{DispatchSubmissionAccepted, DispatchSubmissionRequest},
    };
    use magi_core::{
        AbsolutePath, MissionId, SessionId, Task, TaskId, TaskKind, TaskRuntimePayload, TaskStatus,
        UtcMillis, WorkspaceId,
    };
    use magi_event_bus::InMemoryEventBus;
    use magi_governance::GovernanceService;
    use magi_orchestrator::task_store::TaskStore;
    use magi_session_store::{SessionStore, TimelineEntryKind};
    use magi_workspace::WorkspaceStore;
    use std::sync::Arc;

    fn test_state() -> ApiState {
        ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            Arc::new(SessionStore::default()),
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
    }

    #[tokio::test]
    async fn project_directory_binding_survives_acceptance_reload_and_primary_change() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("a");
        let second = root.path().join("b");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(source.join("same.txt"), "original").unwrap();
        std::fs::write(second.join("same.txt"), "other").unwrap();
        let source = std::fs::canonicalize(source).unwrap();
        let second = std::fs::canonicalize(second).unwrap();
        let tasks = Arc::new(TaskStore::new());
        let state = test_state().with_task_store(tasks.clone());
        let workspace_id = WorkspaceId::new("frozen-project");
        let project = state
            .workspace_registry
            .register_native_path(workspace_id.clone(), source.clone())
            .unwrap();
        let binding = state
            .workspace_registry
            .capture_directory_binding(&workspace_id, None, None)
            .unwrap();
        let session = SessionId::new("frozen-session");
        state
            .session_store
            .create_session_for_workspace(session.clone(), "frozen", Some(workspace_id.to_string()))
            .unwrap();
        let queued_session = SessionId::new("frozen-queued-session");
        state
            .session_store
            .create_session_for_workspace(
                queued_session.clone(),
                "queue",
                Some(workspace_id.to_string()),
            )
            .unwrap();
        crate::routes::test_turn_fixtures::seed_conversation_turn(
            &state.session_store,
            state.turn_coordinator(),
            &queued_session,
            "busy-turn",
            1,
            UtcMillis::now(),
            "running",
            "busy",
        );
        let queued_request: crate::dto::SessionTurnRequestDto = serde_json::from_value(serde_json::json!({
            "sessionId": queued_session, "scope": "workspace", "workspaceId": workspace_id,
            "text": "queued", "requestId": "frozen-queue-request", "userMessageId": "frozen-queue-user"
        })).unwrap();
        let queued_response = crate::turn_service::TurnService::new(state.clone())
            .submit(queued_request.clone())
            .await
            .unwrap();
        assert!(queued_response.queued);
        let queued = state
            .peek_next_regular_session_turn(&queued_session)
            .unwrap();
        assert_eq!(queued.project_directories.as_ref(), Some(&binding));
        let request: DispatchSubmissionRequest = serde_json::from_value(serde_json::json!({
            "accepted_at": UtcMillis::now(), "session_id": session, "workspace_id": workspace_id,
            "project_directories": binding, "execution_root": second,
            "entry_id": "frozen-input", "timeline_message": "inspect", "created_session": false,
            "mission_title": "inspect", "task_title": "inspect", "trimmed_text": "inspect",
            "execution_goal": "inspect", "task_tier": magi_core::TaskTier::ExecutionChain,
            "context_references": [], "browser_annotation_refs": [], "browser_node_selections": [],
            "plugin_selections": [], "goal_mode": false, "target_role": "coordinator",
            "required_tool_chain": [], "completion_contract": magi_core::TaskCompletionContract::default(),
            "denied_tools": [], "user_message_metadata": {},
            "turn_origin": magi_conversation_runtime::dispatch_submission::DispatchTurnOrigin::User
        })).unwrap();
        let accepted = super::submit_dispatch_submission(&state, request).unwrap();
        assert_eq!(accepted.request.execution_root.as_ref(), Some(&source));
        let mut other = project.primary_source_folder().clone();
        let host = magi_core::HostPath::from_path(second);
        other.source_folder_id = "other".into();
        other.path = AbsolutePath::new(host.display_string());
        other.path_ref = host.to_path_ref().as_str().into();
        let mut members = project.source_folders;
        members.push(other.clone());
        state
            .workspace_registry
            .edit_project(
                &workspace_id,
                1,
                magi_workspace::WorkspaceProjectEdit {
                    name: Some("renamed".into()),
                    source_folders: members,
                    primary_source_folder_id: "other".into(),
                },
                |_| Ok(()),
            )
            .unwrap();
        assert_eq!(
            super::bound_project_execution_root(&state, &session, &binding).unwrap(),
            source,
            "主线和独立代理继续使用原绑定，不能随主要源目录切换"
        );
        assert!(
            super::validate_or_migrate_directory_binding(&state, Some(&workspace_id), None)
                .is_err()
        );
        let replay = crate::turn_service::TurnService::new(state.clone())
            .submit(queued_request)
            .await
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(
            state
                .peek_next_regular_session_turn(&queued_session)
                .unwrap()
                .project_directories
                .as_ref(),
            Some(&binding)
        );
        let recovered =
            magi_conversation_runtime::dispatch_submission::recover_dispatch_submission_request(
                &state.session_store.runtime_sidecar(&session).unwrap(),
            )
            .unwrap();
        assert_eq!(recovered.project_directories.as_ref(), Some(&binding));
        assert_eq!(recovered.execution_root.as_ref(), Some(&source));
        let projection = root.path().join("tasks");
        TaskStore::checkpoint_snapshot_to_projection_directory(&tasks.snapshot(), &projection)
            .unwrap();
        let restored = TaskStore::restore_from_projection_directory(&projection)
            .unwrap()
            .unwrap();
        assert_eq!(
            restored
                .get_task(&accepted.root_task_id)
                .unwrap()
                .executor_binding
                .unwrap()
                .project_directories
                .as_ref(),
            Some(&binding)
        );
        let prepare = super::workspace_execution_preparer(&state);
        let session_to_prepare = session.clone();
        let workspace_to_prepare = workspace_id.clone();
        let frozen = binding.clone();
        let resources = tokio::task::spawn_blocking(move || {
            prepare(
                &session_to_prepare,
                &Some(workspace_to_prepare),
                &accepted.turn_id,
                Some(&frozen),
                magi_conversation_runtime::workspace_execution::WorkspacePreparationMode::Execution,
            )
        })
        .await
        .unwrap()
        .unwrap();
        let directory = resources.directories.as_ref().unwrap().root(None).unwrap();
        assert_eq!(directory.source_root().unwrap(), source);
        let expected_execution = state
            .session_execution_root_for_source(&session, &source)
            .unwrap();
        assert_eq!(
            super::bound_project_execution_root(&state, &session, &binding).unwrap(),
            expected_execution,
            "续跑必须使用已建立的原会话执行副本"
        );
        let snapshot = resources.snapshots.into_iter().next().unwrap();
        assert_eq!(
            snapshot.workspace_root(),
            std::fs::canonicalize(expected_execution).unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(snapshot.workspace_root().join("same.txt")).unwrap(),
            "original"
        );
        let goal = state
            .session_store
            .create_goal(
                session.clone(),
                state
                    .session_store
                    .ensure_session_mission(&session, UtcMillis::now(), || {
                        MissionId::new("goal-binding-test")
                    })
                    .1,
                accepted.root_task_id.as_str(),
                "continue original source",
                magi_core::AccessProfile::Restricted,
            )
            .unwrap();
        assert_eq!(
            super::goal_directory_binding(&state, &goal)
                .unwrap()
                .as_ref(),
            Some(&binding)
        );
        tasks.remove_task(&accepted.root_task_id).unwrap();
        assert_eq!(
            super::goal_directory_binding(&state, &goal)
                .unwrap()
                .as_ref(),
            Some(&binding),
            "原 Task 已被清理时仍从原 canonical 请求恢复，不能从当前项目默认值推导"
        );
        state
            .workspace_registry
            .edit_project(
                &workspace_id,
                2,
                magi_workspace::WorkspaceProjectEdit {
                    name: None,
                    source_folders: vec![other],
                    primary_source_folder_id: "other".into(),
                },
                |_| Ok(()),
            )
            .unwrap();
        assert!(super::bound_project_execution_root(&state, &session, &binding).is_err());
        assert!(
            super::validate_or_migrate_directory_binding(
                &state,
                Some(&workspace_id),
                Some(binding)
            )
            .is_err()
        );
        assert!(
            super::validate_or_migrate_directory_binding(&state, Some(&workspace_id), None)
                .is_err(),
            "目录更换后即使只剩一个成员，也不能把旧无绑定任务迁到新目录"
        );
        state.cleanup_session_isolation(&session).await;
    }

    #[tokio::test]
    async fn workflow_acceptance_freezes_the_host_package_in_task_and_pending_request() {
        let root = tempfile::tempdir().unwrap();
        let package = magi_plugin_system::PluginPackage::from_manifest_and_source(
            serde_json::from_value(serde_json::json!({
                "sdkVersion":1,"id":"test.flow","version":"1.0.0","name":"Flow","description":"",
                "backend":"plugin.mjs","permissions":[],"dataSchemaVersion":1,
                "settingsSchema":{"type":"object","additionalProperties":false},
                "contributions":{"workflows":[{"id":"main","title":"Flow","description":""}]}
            }))
            .unwrap(),
            "export default () => null;",
        )
        .unwrap();
        let mut manager = magi_plugin_system::PluginManager::open(root.path()).unwrap();
        manager
            .install(
                &package,
                magi_plugin_system::PluginSource::Local {
                    name: "flow.zip".into(),
                },
            )
            .unwrap();
        manager.enable("test.flow", "application").unwrap();
        manager.activate("test.flow", "application").unwrap();
        let tasks = Arc::new(TaskStore::new());
        let manager = Arc::new(std::sync::Mutex::new(manager));
        let state = test_state()
            .with_task_store(tasks.clone())
            .with_plugin_manager(manager.clone());
        let session = SessionId::new("workflow-package-session");
        state
            .session_store
            .create_session(session.clone(), "工作流包版本")
            .unwrap();
        let original: crate::dto::SessionTurnRequestDto =
            serde_json::from_value(serde_json::json!({
                "sessionId":session,"scope":"personal","text":"运行工作流",
                "workflowId":"plugin/test.flow/main","requestId":"workflow-package-request",
                "userMessageId":"workflow-package-user"
            }))
            .unwrap();
        let mut request: DispatchSubmissionRequest = serde_json::from_value(serde_json::json!({
            "accepted_at":UtcMillis::now(),"session_id":session,"entry_id":"workflow-package-input",
            "timeline_message":"运行工作流","created_session":false,"mission_title":"工作流",
            "task_title":"工作流","trimmed_text":"运行工作流","execution_goal":"执行已选工作流","task_tier":magi_core::TaskTier::ExecutionChain,
            "context_references":[],"browser_annotation_refs":[],"browser_node_selections":[],"plugin_selections":[],
            "goal_mode":false,"target_role":"coordinator","workflow_id":"plugin/test.flow/main",
            "required_tool_chain":[],"completion_contract":magi_core::TaskCompletionContract::default(),
            "denied_tools":[],"user_message_metadata":{"magi.workflowPackageDigest":"caller-forged-digest"},
            "turn_origin":magi_conversation_runtime::dispatch_submission::DispatchTurnOrigin::User
        })).unwrap();
        request.request_id = original.request_id();
        request.user_message_id = original.user_message_id();
        request.user_message_metadata.insert(
            "requestFingerprint".into(),
            original.request_fingerprint().unwrap().into(),
        );
        let mut unsupported = request.clone();
        unsupported.target_role = Some("executor".into());
        assert!(super::submit_dispatch_submission(&state, unsupported).is_err());
        assert!(
            tasks.all_tasks().is_empty(),
            "不支持工作流的角色必须在接纳前拒绝"
        );
        let accepted = super::submit_dispatch_submission(&state, request).unwrap();
        assert_eq!(
            accepted.request.user_message_metadata["magi.workflowPackageDigest"],
            package.digest()
        );
        let task = tasks.get_task(&accepted.root_task_id).unwrap();
        assert_eq!(
            magi_orchestrator::task_worker_catalog::resolve_task_role(
                &task,
                &state.agent_role_registry
            ),
            Some("coordinator")
        );
        assert!(
            matches!(task.runtime_payload, TaskRuntimePayload::Workflow {package_digest:Some(ref digest), ..} if digest == package.digest())
        );
        let snapshot = tasks.snapshot();
        let projection = root.path().join("tasks");
        TaskStore::checkpoint_snapshot_to_projection_directory(&snapshot, &projection).unwrap();
        let restored = TaskStore::restore_from_projection_directory(&projection)
            .unwrap()
            .unwrap();
        assert_eq!(
            restored
                .get_task(&accepted.root_task_id)
                .unwrap()
                .runtime_payload,
            task.runtime_payload
        );
        let session_path = root.path().join("session.json");
        state
            .session_store
            .persist_projection_with(|durable, sidecars| {
                std::fs::write(
                    &session_path,
                    serde_json::to_vec(&(durable, sidecars)).unwrap(),
                )
            })
            .unwrap();
        let drain =
            magi_plugin_system::PluginManager::begin_upgrade_drain(manager.clone(), "test.flow")
                .unwrap();
        let service = crate::turn_service::TurnService::new(state.clone());
        let replay = service
            .submit(original.clone())
            .await
            .expect("排空不能阻止读取原回执");
        assert!(replay.replayed);
        assert_eq!(replay.turn_id.as_deref(), Some(accepted.turn_id.as_str()));
        let mut new_request = original.clone();
        new_request.request_id = Some("workflow-package-new-request".into());
        new_request.user_message_id = Some("workflow-package-new-user".into());
        assert!(matches!(
            service.submit(new_request).await,
            Err(ApiError::Forbidden(_))
        ));
        let mut manifest = serde_json::to_value(package.manifest()).unwrap();
        manifest["version"] = serde_json::json!("1.1.0");
        let upgraded = magi_plugin_system::PluginPackage::from_manifest_and_source(
            serde_json::from_value(manifest).unwrap(),
            "export default () => null;",
        )
        .unwrap();
        {
            let mut manager = manager.lock().unwrap();
            manager
                .upgrade(
                    &upgraded,
                    magi_plugin_system::PluginSource::Local {
                        name: "flow.zip".into(),
                    },
                    &drain,
                    None,
                )
                .unwrap();
            drain.commit(&mut manager);
        }
        let (durable, sidecars) =
            serde_json::from_slice(&std::fs::read(&session_path).unwrap()).unwrap();
        let sessions = Arc::new(SessionStore::from_persisted_parts(durable, sidecars).unwrap());
        let recovered =
            magi_conversation_runtime::dispatch_submission::recover_dispatch_submission_request(
                &sessions.runtime_sidecar(&session).unwrap(),
            )
            .unwrap();
        assert_eq!(
            recovered.user_message_metadata["magi.workflowPackageDigest"],
            package.digest()
        );
        let state = ApiState::new(
            "magi-test",
            Arc::new(InMemoryEventBus::new(32)),
            sessions,
            Arc::new(WorkspaceStore::default()),
            Arc::new(GovernanceService::default()),
        )
        .with_task_store(Arc::new(restored))
        .with_plugin_manager(Arc::new(std::sync::Mutex::new(
            magi_plugin_system::PluginManager::open(root.path()).unwrap(),
        )));
        let replay = crate::turn_service::TurnService::new(state.clone())
            .submit(original)
            .await
            .expect("升级和重启不能阻止读取原回执");
        assert!(replay.replayed);
        assert_eq!(replay.turn_id.as_deref(), Some(accepted.turn_id.as_str()));
        let mut mismatched = recovered.clone();
        mismatched.user_message_metadata.insert(
            "magi.workflowPackageDigest".into(),
            upgraded.digest().into(),
        );
        let error = super::materialize_dispatch_submission_after_acceptance(
            &state,
            &DispatchSubmissionAccepted {
                request: mismatched,
                ..accepted.clone()
            },
        )
        .unwrap_err();
        assert!(error.message().contains("运行负载与持久化请求不一致"));
        assert!(
            state
                .session_store
                .thread_registry_snapshot(&session)
                .is_empty()
        );
        super::materialize_dispatch_submission_after_acceptance(
            &state,
            &DispatchSubmissionAccepted {
                request: recovered,
                ..accepted.clone()
            },
        )
        .unwrap();
        assert_eq!(
            state
                .task_store()
                .unwrap()
                .get_task(&accepted.root_task_id)
                .unwrap()
                .runtime_payload,
            task.runtime_payload
        );
    }

    #[test]
    fn turn_payload_only_initializes_model_for_a_new_session() {
        let state = test_state();
        state
            .settings_store
            .set_section(
                magi_settings_store::ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
                serde_json::json!({
                    "model": "model-last-used",
                    "reasoningEffort": "high"
                }),
            )
            .unwrap();
        let config = serde_json::json!({ "model": "model-selected-by-user" });
        assert_eq!(
            initial_session_orchestrator_config(&state, true, Some(&config))
                .expect("new session config should be accepted"),
            Some(serde_json::json!({
                "model": "model-selected-by-user",
                "reasoningEffort": "high"
            })),
        );
        assert!(
            initial_session_orchestrator_config(&state, false, Some(&config)).is_err(),
            "ordinary turns must not be able to overwrite the persisted session model",
        );
        assert_eq!(
            initial_session_orchestrator_config(&state, false, None)
                .expect("existing session should use its persisted model"),
            None,
        );
    }

    #[test]
    fn new_session_without_explicit_config_uses_persisted_default() {
        let state = test_state();
        state
            .settings_store
            .set_section(
                magi_settings_store::ORCHESTRATOR_SESSION_DEFAULTS_SECTION,
                serde_json::json!({ "model": "model-last-used" }),
            )
            .unwrap();

        assert_eq!(
            initial_session_orchestrator_config(&state, true, None)
                .expect("new session should inherit the persisted default"),
            Some(serde_json::json!({
                "model": "model-last-used",
                "reasoningEffort": null
            })),
        );
    }

    #[test]
    fn action_task_title_does_not_repeat_execute_prefix() {
        assert_eq!(
            format_action_task_title("执行: 批量检查 README"),
            "执行: 批量检查 README"
        );
        assert_eq!(
            format_action_task_title("批量检查 README"),
            "执行: 批量检查 README"
        );
    }

    #[test]
    fn accepted_dispatch_failure_persists_sanitized_direct_error_on_task() {
        let task_store = Arc::new(TaskStore::new());
        let root_task_id = TaskId::new("task-dispatch-direct-error");
        let now = UtcMillis::now();
        task_store
            .insert_task(Task {
                task_id: root_task_id.clone(),
                mission_id: MissionId::new("mission-dispatch-direct-error"),
                root_task_id: root_task_id.clone(),
                parent_task_id: None,
                kind: TaskKind::LocalAgent,
                title: "派发失败诊断".to_string(),
                goal: "保留直接错误".to_string(),
                status: TaskStatus::Pending,
                dependency_ids: Vec::new(),
                required_children: Vec::new(),
                policy_snapshot: None,
                executor_binding: None,
                completion_contract: magi_core::TaskCompletionContract::default(),
                recovery_checkpoint: None,
                knowledge_refs: Vec::new(),
                workspace_scope: None,
                write_scope: None,
                input_refs: Vec::new(),
                output_refs: Vec::new(),
                evidence_refs: Vec::new(),
                retry_count: 0,
                runtime_payload: TaskRuntimePayload::default(),
                created_at: now,
                updated_at: now,
            })
            .expect("任务应插入");
        let state = test_state().with_task_store(task_store.clone());
        let accepted = DispatchSubmissionAccepted {
            request: DispatchSubmissionRequest {
                project_directories: None,
                accepted_at: now,
                session_id: SessionId::new("session-dispatch-direct-error"),
                workspace_id: None,
                execution_root: None,
                orchestrator_session_config: None,
                entry_id: "entry-dispatch-direct-error".to_string(),
                timeline_message: "派发失败诊断".to_string(),
                images: Vec::new(),
                context_references: Vec::new(),
                browser_annotation_refs: Vec::new(),
                browser_node_selections: Vec::new(),
                plugin_selections: Vec::new(),
                created_session: false,
                mission_title: "派发失败诊断".to_string(),
                task_title: "派发失败诊断".to_string(),
                trimmed_text: Some("保留直接错误".to_string()),
                execution_goal: Some("保留直接错误".to_string()),
                task_tier: magi_core::TaskTier::ExecutionChain,
                access_profile: magi_core::AccessProfile::Restricted,
                skill_name: None,
                goal_mode: false,
                use_tools: true,
                target_role: None,
                workflow_id: None,
                request_id: None,
                user_message_id: None,
                placeholder_message_id: None,
                replace_turn_id: None,
                required_tool_chain: Vec::new(),
                completion_contract: magi_core::TaskCompletionContract::default(),
                recovery_checkpoint: None,
                denied_tools: Vec::new(),
                user_message_metadata: Default::default(),
                turn_origin:
                    magi_conversation_runtime::dispatch_submission::DispatchTurnOrigin::User,
            },
            session_id: SessionId::new("session-dispatch-direct-error"),
            entry_id: "entry-dispatch-direct-error".to_string(),
            accepted_at: now,
            created_session: false,
            root_task_id: root_task_id.clone(),
            action_task_id: root_task_id.clone(),
            turn_id: "turn-dispatch-direct-error".to_string(),
            user_message_item_id: None,
            accepted_canonical_turn: None,
            runner_started: false,
            superseded_turn: None,
        };
        let error = ApiError::internal_assembly(
            "驱动任务派发",
            "provider timeout at /Users/xie/code/private.rs with sk-test-secret-value",
        );

        fail_accepted_task_submission(&state, &accepted, error.message());

        let failed_task = task_store
            .get_task(&root_task_id)
            .expect("failed task should remain available");
        assert_eq!(failed_task.status, TaskStatus::Failed);
        assert_eq!(failed_task.output_refs.len(), 1);
        let direct_error = &failed_task.output_refs[0];
        assert!(direct_error.contains("驱动任务派发"));
        assert!(direct_error.contains("provider timeout"));
        assert!(direct_error.contains("[path]"));
        assert!(direct_error.contains("sk-[redacted]"));
        assert!(!direct_error.contains("/Users/xie"));
        assert!(!direct_error.contains("test-secret-value"));
        assert!(!direct_error.contains("可直接重试"));
    }

    #[test]
    fn resolve_turn_session_ignores_empty_current_session_without_explicit_session() {
        let state = test_state();
        let workspace_id = WorkspaceId::new("workspace-dispatch-empty-current");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/tmp/magi-dispatch-empty-current"),
            )
            .expect("workspace should register");
        let empty_session_id = SessionId::new("session-empty-current");
        state
            .session_store
            .create_session_for_workspace(
                empty_session_id.clone(),
                "空白会话",
                Some(workspace_id.to_string()),
            )
            .expect("empty session should create");

        let (resolved_session_id, created_session, resolved_workspace_id) = resolve_turn_session(
            &state,
            None,
            Some(workspace_id.clone()),
            "真实首条消息",
            UtcMillis::now(),
        )
        .expect("dispatch session should resolve");

        assert_ne!(resolved_session_id, empty_session_id);
        assert!(created_session);
        assert_eq!(resolved_workspace_id, Some(workspace_id));
    }

    #[test]
    fn resolve_turn_session_uses_turn_accept_time_for_new_session_creation() {
        let state = test_state();
        let workspace_id = WorkspaceId::new("workspace-dispatch-accepted-at");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/tmp/magi-dispatch-accepted-at"),
            )
            .expect("workspace should register");
        let accepted_at = UtcMillis(1_779_700_000_000);

        let (resolved_session_id, created_session, _) =
            resolve_turn_session(&state, None, Some(workspace_id), "新会话", accepted_at)
                .expect("dispatch session should resolve");

        let session = state
            .session_store
            .session(&resolved_session_id)
            .expect("created session should exist");
        assert!(created_session);
        assert_eq!(session.created_at, accepted_at);
        assert_eq!(session.updated_at, accepted_at);
    }

    #[test]
    fn resolve_turn_session_creates_new_without_explicit_session_even_when_current_has_history() {
        let state = test_state();
        let workspace_id = WorkspaceId::new("workspace-dispatch-current-history");
        state
            .workspace_registry
            .register(
                workspace_id.clone(),
                AbsolutePath::new("/tmp/magi-dispatch-current-history"),
            )
            .expect("workspace should register");
        let session_id = SessionId::new("session-current-history");
        state
            .session_store
            .create_session_for_workspace(
                session_id.clone(),
                "已有历史",
                Some(workspace_id.to_string()),
            )
            .expect("session should create");
        state.session_store.append_timeline_entry(
            session_id.clone(),
            TimelineEntryKind::UserMessage,
            "已有用户消息",
        );

        let (resolved_session_id, created_session, resolved_workspace_id) = resolve_turn_session(
            &state,
            None,
            Some(workspace_id.clone()),
            "后续消息",
            UtcMillis::now(),
        )
        .expect("dispatch session should resolve");

        assert_ne!(resolved_session_id, session_id);
        assert!(created_session);
        assert_eq!(resolved_workspace_id, Some(workspace_id));
    }
}
