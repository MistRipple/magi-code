#!/bin/bash
BASE="/Users/xie/code/magi-rust-rewrite"

check() {
    local target="$1"
    if [ -e "$BASE/$target" ]; then
        echo "- $target: EXISTS"
    else
        echo "- $target: MISSING"
    fi
}

echo "## Phase 1 (magi-todo)"
for f in types.rs governance.rs repository.rs manager.rs; do
    check "magi-todo/src/$f"
done

echo ""
echo "## Phase 2 (magi-usage-authority)"
for f in types.rs model_identity.rs costing.rs reducer.rs ledger_store.rs runtime_recorder.rs authority.rs query_service.rs; do
    check "magi-usage-authority/src/$f"
done

echo ""
echo "## Phase 3 (magi-normalizer)"
for f in base.rs claude.rs codex.rs gemini.rs orchestrator.rs; do
    check "magi-normalizer/src/$f"
done

echo ""
echo "## Phase 4 (magi-bridge-client)"
check "magi-bridge-client/src/protocol"
for f in auto_compaction.rs conversation_compaction.rs decision_engine.rs orchestrator_adapter.rs worker_adapter.rs round_policy.rs termination.rs structured_dispatch.rs final_text_policy.rs micro_compaction.rs base_adapter.rs tool_concurrency.rs worker_duplicate_guard.rs llm_client.rs; do
    check "magi-bridge-client/src/$f"
done

echo ""
echo "## Phase 5 (magi-orchestrator)"
for f in manager.rs batch.rs worker_pipeline.rs idempotency.rs scheduler.rs routing.rs completion.rs resume_context_store.rs batch_coordinator.rs reactive_wait.rs protocol_manager.rs presentation_adapter.rs presentation_layer.rs delivery_synthesis.rs post_dispatch_verifier.rs runtime_event_bus.rs todo_update_guard.rs thread_presentation.rs correlation_planner.rs; do
    check "magi-orchestrator/src/dispatch/$f"
done
check "magi-orchestrator/src/orchestration"
check "magi-orchestrator/src/message"
for f in mission_engine.rs governance_engine.rs request_classifier.rs validator_registry.rs verification_runner.rs; do
    check "magi-orchestrator/src/$f"
done
check "magi-orchestrator/src/plan_ledger"
check "magi-orchestrator/src/auto_learning"

echo ""
echo "## Phase 6 (magi-context-runtime)"
for f in shared_context_pool.rs memory_document.rs context_auxiliary.rs layered_memory_store.rs snapshot.rs; do
    check "magi-context-runtime/src/$f"
done

echo ""
echo "## Phase 7 (magi-agent)"
for f in protocol.rs runtime_state.rs session_context.rs session_registry.rs workspace_container.rs; do
    check "magi-agent/src/$f"
done

