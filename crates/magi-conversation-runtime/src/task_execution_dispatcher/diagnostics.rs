use super::*;

impl LlmTaskDispatcher {
    pub(super) fn publish_task_dispatched_event(&self, input: TaskDispatchedEventInput<'_>) {
        let TaskDispatchedEventInput {
            task_id,
            mission_id,
            worker,
            lease_id,
            kind,
            session_id,
            workspace_id,
        } = input;
        let event = EventEnvelope::domain(
            EventId::unique("event-task-dispatched"),
            "task.dispatched",
            serde_json::json!({
                "task_id": task_id.to_string(),
                "mission_id": mission_id.to_string(),
                "session_id": session_id.map(ToString::to_string),
                "workspace_id": workspace_id.map(ToString::to_string),
                "worker_id": worker.worker_id.to_string(),
                "role": worker.role,
                "lease_id": lease_id.to_string(),
                "kind": format!("{:?}", kind),
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.cloned(),
            session_id: session_id.cloned(),
            mission_id: Some(mission_id.clone()),
            task_id: Some(task_id.clone()),
            ..EventContext::default()
        });
        let _ = self.event_bus.publish(event);
    }

    pub(super) fn notify_task_result(
        &self,
        task_id: &TaskId,
        lease_id: &LeaseId,
        outcome: TaskOutcome,
    ) {
        self.completion_sink.notify(TaskResult {
            task_id: task_id.clone(),
            lease_id: lease_id.clone(),
            outcome,
        });
    }

    pub(super) fn schedule_post_completion_enrichment(
        &self,
        session_id: SessionId,
        turn_id: String,
        workspace_id: Option<WorkspaceId>,
        outcome: TaskOutcome,
        execution_settings_snapshot: Option<Arc<SettingsStore>>,
    ) {
        let dispatcher = self.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("magi-session-enrichment".to_string())
            .spawn(move || {
                let execution_settings =
                    dispatcher.execution_settings_or_live(execution_settings_snapshot.as_ref());
                dispatcher.extract_and_persist_knowledge(
                    execution_settings,
                    &session_id,
                    Some(&turn_id),
                    &workspace_id,
                    &outcome,
                );
                dispatcher.extract_and_persist_session_memory(
                    execution_settings,
                    &session_id,
                    Some(&turn_id),
                    &workspace_id,
                );
            })
        {
            tracing::warn!(%error, "启动会话后台沉淀线程失败");
        }
    }

    pub(super) fn extract_and_persist_knowledge(
        &self,
        settings_store: Option<&Arc<SettingsStore>>,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        workspace_id: &Option<WorkspaceId>,
        outcome: &TaskOutcome,
    ) {
        let Some(store) = self.knowledge_store.as_ref() else {
            return;
        };
        let TaskOutcome::Completed { attempt } = outcome else {
            return;
        };

        let timeline_text = self
            .session_store
            .timeline_for_session(session_id)
            .into_iter()
            .rev()
            .filter(|entry| {
                matches!(
                    entry.kind,
                    TimelineEntryKind::UserMessage | TimelineEntryKind::AssistantMessage
                )
            })
            .take(12)
            .filter_map(|entry| timeline_entry_visible_text(&entry.message))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n\n");
        // output_refs 可能包含完整的工具证据（一次大型只读分析可达数百 KB），
        // 不应把它原样复制给辅助模型。知识抽取只需要用户可见的终态答复；
        // 完整证据继续留在 task/session 审计链路中。
        let output_text = attempt
            .final_response
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| {
                attempt
                    .output_refs
                    .iter()
                    .map(|output| public_runtime_excerpt(output, 8_000))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            });
        let extraction_text = format!("{timeline_text}\n\n{output_text}");
        let Some(client) = self
            .resolve_target_cached(settings_store, RoleTarget::Auxiliary, None, "")
            .ok()
            .flatten()
        else {
            self.publish_learning_extraction_diagnostic(
                session_id,
                workspace_id.as_ref(),
                "skipped",
                Some("auxiliary_model_unconfigured"),
                0,
                0,
            );
            return;
        };
        let learnings = match extract_learnings_via_auxiliary(
            AuxiliaryModelExtractionContext {
                client,
                event_bus: self.event_bus.as_ref(),
                session_store: self.session_store.as_ref(),
                settings_store,
                session_id,
                workspace_id,
                expected_turn_id,
            },
            &extraction_text,
        ) {
            Ok(Some(learnings)) => learnings,
            Ok(None) => {
                self.publish_learning_extraction_diagnostic(
                    session_id,
                    workspace_id.as_ref(),
                    "completed",
                    None,
                    0,
                    0,
                );
                return;
            }
            Err(failure) => {
                self.publish_learning_extraction_diagnostic(
                    session_id,
                    workspace_id.as_ref(),
                    "failed",
                    Some(failure.as_str()),
                    0,
                    0,
                );
                return;
            }
        };
        let candidate_count = learnings.len();
        let mut existing = store.list();
        let mut inserted = 0usize;
        for (index, learning) in learnings.into_iter().enumerate() {
            if knowledge_duplicate(
                &existing,
                KnowledgeKind::Learning,
                workspace_id.as_ref(),
                &learning.content,
            ) {
                continue;
            }
            let now = UtcMillis::now();
            let record = KnowledgeRecord {
                knowledge_id: format!("learning-auto-{}-{index}", now.0),
                kind: KnowledgeKind::Learning,
                title: title_from_learning_content(&learning.content),
                content: learning.content,
                tags: learning.tags,
                workspace_id: workspace_id.clone(),
                source_ref: Some(
                    learning
                        .context
                        .unwrap_or_else(|| format!("session:{}", session_id.as_str())),
                ),
                created_at: now,
                updated_at: now,
            };
            store.upsert(record.clone());
            existing.push(record);
            inserted += 1;
        }
        if inserted > 0
            && let Some(callback) = self.knowledge_persist_callback.as_ref()
        {
            callback();
        }
        self.publish_learning_extraction_diagnostic(
            session_id,
            workspace_id.as_ref(),
            "completed",
            None,
            candidate_count,
            inserted,
        );
    }

    /// 走辅助模型把当前会话压缩成 5 类结构化记忆（currentWork / decisions /
    /// importantContext / pendingIssues / nextSteps）。
    ///
    /// 调用时机：与 `extract_and_persist_knowledge` 并列，主 action task 完成且
    /// `writebacks` 非空时触发。配合水位线（自上一次会话记忆抽取以来累计 token
    /// 超过 `SESSION_MEMORY_WATERLINE_TOKENS`）才真正调用 LLM，避免每轮都跑。
    ///
    /// 辅助模型未配置、调用失败、JSON 解析异常一律静默跳过（`tracing::debug!`），
    /// 不做任何"退回到 marker 写回"之类的兜底 —— 与现有辅助模型路径同语义。
    pub(super) fn extract_and_persist_session_memory(
        &self,
        settings_store: Option<&Arc<SettingsStore>>,
        session_id: &SessionId,
        expected_turn_id: Option<&str>,
        workspace_id: &Option<WorkspaceId>,
    ) {
        let Some(client) = self
            .resolve_target_cached(settings_store, RoleTarget::Auxiliary, None, "")
            .ok()
            .flatten()
        else {
            return;
        };

        let timeline = self.session_store.timeline_for_session(session_id);
        let last_extraction_at = self
            .pipeline
            .memory_store
            .extraction_results_for_session(session_id)
            .into_iter()
            .filter(|record| {
                record
                    .source_ref
                    .as_deref()
                    .is_some_and(|s| s.starts_with(SESSION_MEMORY_SOURCE_PREFIX))
            })
            .map(|record| record.created_at)
            .next_back();

        let excerpt_entries: Vec<_> = timeline
            .iter()
            .filter(|entry| {
                matches!(
                    entry.kind,
                    TimelineEntryKind::UserMessage | TimelineEntryKind::AssistantMessage
                )
            })
            .filter(|entry| match last_extraction_at {
                Some(ts) => entry.occurred_at.0 > ts.0,
                None => true,
            })
            .collect();

        let excerpt_text = excerpt_entries
            .iter()
            .filter_map(|entry| {
                let text = timeline_entry_visible_text(&entry.message)?;
                let prefix = match entry.kind {
                    TimelineEntryKind::UserMessage => "用户",
                    TimelineEntryKind::AssistantMessage => "助手",
                    _ => "其他",
                };
                Some(format!("[{prefix}] {text}"))
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        if excerpt_text.is_empty() {
            return;
        }
        if estimate_session_memory_tokens(&excerpt_text) < SESSION_MEMORY_WATERLINE_TOKENS {
            return;
        }

        let Some(slices) = extract_session_memory_via_auxiliary(
            AuxiliaryModelExtractionContext {
                client,
                event_bus: self.event_bus.as_ref(),
                session_store: self.session_store.as_ref(),
                settings_store,
                session_id,
                workspace_id,
                expected_turn_id,
            },
            &excerpt_text,
        ) else {
            return;
        };
        if slices.is_empty() {
            return;
        }

        let now = UtcMillis::now();
        let extraction_id = format!("extract-session-memory-{}-{}", session_id.as_str(), now.0);
        let source_ref = format!(
            "{SESSION_MEMORY_SOURCE_PREFIX}{}/{}",
            session_id.as_str(),
            now.0
        );
        let memories = slices
            .into_iter()
            .enumerate()
            .map(|(index, slice)| ExtractedMemory {
                memory_id: format!(
                    "mem-session-memory-{}-{}-{index}",
                    session_id.as_str(),
                    now.0
                ),
                layer: MemoryLayer::Recent,
                content: format!("[{}] {}", slice.category, slice.content),
                created_at: now,
            })
            .collect::<Vec<_>>();

        self.pipeline
            .memory_store
            .apply_extraction(MemoryExtractionApplyRequest {
                extraction_id,
                session_id: session_id.clone(),
                source_ref: Some(source_ref),
                summary: "辅助模型会话记忆抽取".to_string(),
                memories,
                created_at: now,
            });
    }

    pub(super) fn publish_execution_overview(
        &self,
        task: &magi_core::Task,
        session_id: &SessionId,
        workspace_id: &Option<WorkspaceId>,
        context_summary: Option<ExecutionContextSummary>,
    ) {
        let context_payload = context_summary
            .as_ref()
            .and_then(|s| serde_json::to_value(s).ok())
            .unwrap_or(serde_json::Value::Null);
        let event = EventEnvelope::audit(
            EventId::unique("event-mission-overview"),
            "mission.execution.overview",
            serde_json::json!({
                "mission_id": task.mission_id.to_string(),
                "session_id": session_id.to_string(),
                "workspace_id": workspace_id.as_ref().map(ToString::to_string),
                "context": context_payload,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.clone(),
            session_id: Some(session_id.clone()),
            mission_id: Some(task.mission_id.clone()),
            task_id: Some(task.task_id.clone()),
            ..EventContext::default()
        });
        let _ = self.event_bus.publish(event);
    }

    pub(super) fn publish_knowledge_context_diagnostic(
        &self,
        selection: &KnowledgeContextSelection,
        session_id: &SessionId,
        workspace_id: Option<&WorkspaceId>,
        mission_id: Option<&magi_core::MissionId>,
        task_id: Option<&TaskId>,
    ) {
        let event = EventEnvelope::audit(
            EventId::unique(format!("event-knowledge-context-{}", session_id)),
            "knowledge.context.selected",
            serde_json::json!({
                "consumer": selection.consumer,
                "decision": selection.decision,
                "knowledge_ids": selection.results.iter().map(|item| item.knowledge_id.clone()).collect::<Vec<_>>(),
                "result_kinds": selection.results.iter().map(|item| knowledge_kind_label(item.kind)).collect::<Vec<_>>(),
                "matched_count": selection.matched_count,
                "injected_count": selection.results.len(),
                "injected_chars": selection.injected_chars,
                "graph_focus_count": selection.graph_context.len(),
                "graph_injected_chars": selection.graph_context.iter().map(|graph| graph.injected_chars).sum::<usize>(),
                "graph_node_count": selection.graph_context.iter().map(|graph| graph.nodes.len()).sum::<usize>(),
                "graph_edge_count": selection.graph_context.iter().map(|graph| graph.edges.len()).sum::<usize>(),
                "graph_candidate_edge_count": selection.graph_context.iter().map(|graph| graph.candidate_edge_count).sum::<usize>(),
                "graph_inferred_edge_count": selection.graph_context.iter().map(|graph| graph.inferred_edge_count).sum::<usize>(),
                "graph_dangling_edge_count": selection.graph_context.iter().map(|graph| graph.dangling_edge_count).sum::<usize>(),
                "graph_truncated": selection.graph_context.iter().any(|graph| graph.truncated),
                "truncated": selection.truncated,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.cloned(),
            session_id: Some(session_id.clone()),
            mission_id: mission_id.cloned(),
            task_id: task_id.cloned(),
            ..EventContext::default()
        });
        let _ = self.event_bus.publish(event);
    }

    pub(super) fn publish_learning_extraction_diagnostic(
        &self,
        session_id: &SessionId,
        workspace_id: Option<&WorkspaceId>,
        status: &str,
        failure_reason: Option<&str>,
        candidate_count: usize,
        inserted_count: usize,
    ) {
        let event = EventEnvelope::audit(
            EventId::unique(format!("event-learning-extraction-{}", session_id)),
            "knowledge.learning.extraction",
            serde_json::json!({
                "status": status,
                "failure_reason": failure_reason,
                "candidate_count": candidate_count,
                "inserted_count": inserted_count,
            }),
        )
        .with_context(EventContext {
            workspace_id: workspace_id.cloned(),
            session_id: Some(session_id.clone()),
            ..EventContext::default()
        });
        let _ = self.event_bus.publish(event);
    }
}
