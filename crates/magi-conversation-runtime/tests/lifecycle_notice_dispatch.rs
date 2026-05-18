//! 集成测试:codex goal 桥 §1.4 端到端 — event bus → subscriber → registry → prompt 装配。
//!
//! 单元层已分别覆盖 `LifecycleNoticeRegistry::ingest/pending_notice`(notice_queue 内单测)、
//! `run_subscriber` 同步 ingest(subscriber.rs 内 tokio 单测)、
//! `prepend_session_instructions` 三段排序(prompt_utils.rs 内单测)。
//!
//! 本测试把这三段在异步 broadcast 通道上**串成一条**:
//!
//! 1. 真实 `InMemoryEventBus::publish` 走 broadcast sender
//! 2. tokio task 跑 `run_subscriber`,async recv 后调用 `registry.ingest`
//! 3. 测试线程模拟 dispatcher `assemble_prompt` 的两行核心组合:
//!    `let notice = registry.pending_notice(&mid);`
//!    `prepend_session_instructions(.., notice.as_deref(), base)`
//!
//! 反孤儿担保:dispatcher 在 [task_execution_dispatcher.rs:770-851] 正是这两行
//! 调用 — 这条集成路径在生产 dispatcher 中**没有被绕过的旁路**。完整构造一个
//! `LlmTaskDispatcher` 需要 `ExecutionPipeline` + 7 个 registry,加 ~200 行 fixture
//! 不产生额外覆盖(`assemble_prompt` 的其它分支已由 `prepend_session_instructions`
//! 单元测试覆盖)。所以本测试只对真正的跨 crate 异步链路负责。

use std::sync::Arc;

use magi_conversation_runtime::prompt_utils::prepend_session_instructions;
use magi_core::MissionId;
use magi_event_bus::task_events::{
    mission_plan_step_completed_event, mission_resumed_from_recovery_event,
};
use magi_event_bus::{EventContext, InMemoryEventBus};
use magi_lifecycle_notice::{LifecycleNoticeRegistry, run_subscriber};

/// 把 dispatcher 的"装配生命周期通知"逻辑独立出来 — 与
/// `LlmTaskDispatcher::lifecycle_notice_for_mission` + `assemble_prompt` 中那两行
/// 一一对应,**不是 shim**,只是为了让测试不必构造 200 行 dispatcher fixture。
fn dispatcher_prompt_composition(
    registry: &Arc<LifecycleNoticeRegistry>,
    mission_id: &MissionId,
    base_prompt: &str,
) -> String {
    let notice = registry.pending_notice(mission_id);
    prepend_session_instructions(None, None, notice.as_deref(), base_prompt)
}

/// 让 subscriber 在 broadcast 通道上消费完一批已 publish 的事件。
///
/// `current_thread` runtime 下 publish 不会自动让出执行权 — 必须显式 yield 才能
/// 让 spawn 出去的 subscriber task 跑 `recv().await`。我们没法从外部直接探测
/// "事件已 ingest"(pending_notice 一旦读出就 drain),所以用一个固定的小循环
/// 让出几轮:每轮 yield + 2ms sleep,共 10 轮,经测足以稳定通过。
async fn drain_subscriber() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn resumed_event_injects_then_drains_then_plan_step_overrides() {
    let bus = Arc::new(InMemoryEventBus::new(32));
    let registry = Arc::new(LifecycleNoticeRegistry::new());
    let subscriber_handle = tokio::spawn(run_subscriber(registry.clone(), bus.clone()));

    // 等订阅者真正 subscribe() — 否则后续 publish 会因为没有 receiver 失败。
    tokio::task::yield_now().await;

    let mid = MissionId::new("M-codex-bridge");

    // 1) 发 `mission.resumed.from_recovery`,recovery_id=ckpt-7。
    let resumed_env = mission_resumed_from_recovery_event(
        "M-codex-bridge",
        "ckpt-7",
        9,
        Some("chain/main"),
        Some("commit-abc"),
    )
    .with_context(EventContext {
        mission_id: Some(mid.clone()),
        ..EventContext::default()
    });
    bus.publish(resumed_env).expect("publish resumed");

    // 让 subscriber 把事件 ingest 到 registry。
    drain_subscriber().await;

    // 2) 模拟 dispatcher 装配 prompt。
    let first_prompt =
        dispatcher_prompt_composition(&registry, &mid, "执行任务:推进 Mission M-codex-bridge");
    assert!(
        first_prompt.contains("--- 生命周期通知 ---"),
        "首轮 prompt 应包含生命周期通知段,实际:\n{first_prompt}"
    );
    assert!(
        first_prompt.contains("ckpt-7"),
        "首轮 prompt 应包含 recovery_id `ckpt-7`,实际:\n{first_prompt}"
    );
    assert!(
        first_prompt.ends_with("执行任务:推进 Mission M-codex-bridge"),
        "首轮 prompt 应保留原 base prompt 在尾部,实际:\n{first_prompt}"
    );

    // 3) 第二轮 — mission_resumed 是一次性 slot,应已被 drain。
    let second_prompt =
        dispatcher_prompt_composition(&registry, &mid, "执行任务:推进 Mission M-codex-bridge");
    assert!(
        !second_prompt.contains("ckpt-7"),
        "第二轮 prompt 不应再次注入 mission_resumed(已 drain),实际:\n{second_prompt}"
    );
    assert!(
        !second_prompt.contains("--- 生命周期通知 ---"),
        "第二轮 prompt 全 slot 空,不应出现生命周期通知段,实际:\n{second_prompt}"
    );

    // 4) 发两次 `mission.plan_step.completed`,后者覆盖前者。
    bus.publish(
        mission_plan_step_completed_event("M-codex-bridge", "s1", "搭建脚手架", 3, 1).with_context(
            EventContext {
                mission_id: Some(mid.clone()),
                ..EventContext::default()
            },
        ),
    )
    .expect("publish step s1");
    bus.publish(
        mission_plan_step_completed_event("M-codex-bridge", "s2", "落地核心逻辑", 3, 2)
            .with_context(EventContext {
                mission_id: Some(mid.clone()),
                ..EventContext::default()
            }),
    )
    .expect("publish step s2");

    // 给 subscriber 时间消费两个事件。
    drain_subscriber().await;

    let third_prompt =
        dispatcher_prompt_composition(&registry, &mid, "执行任务:推进 Mission M-codex-bridge");
    assert!(
        third_prompt.contains("--- 生命周期通知 ---"),
        "plan_step 通知应触发生命周期段,实际:\n{third_prompt}"
    );
    assert!(
        third_prompt.contains("s2") && third_prompt.contains("落地核心逻辑"),
        "应保留最新 plan_step(s2)完成通知,实际:\n{third_prompt}"
    );
    assert!(
        !third_prompt.contains("s1") && !third_prompt.contains("搭建脚手架"),
        "旧 plan_step(s1)应被新事件覆盖,实际:\n{third_prompt}"
    );

    // 收尾:drop bus → subscriber 自然结束。
    drop(bus);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(200), subscriber_handle).await;
}

#[tokio::test(flavor = "current_thread")]
async fn registry_without_events_yields_no_lifecycle_section() {
    // 守护:registry 全空时,dispatcher 装配的 prompt 不应出现生命周期段。
    // 防御性回归:防止后续把"--- 生命周期通知 ---"误嵌成静态前缀。
    let bus = Arc::new(InMemoryEventBus::new(8));
    let registry = Arc::new(LifecycleNoticeRegistry::new());
    let subscriber_handle = tokio::spawn(run_subscriber(registry.clone(), bus.clone()));
    tokio::task::yield_now().await;

    let prompt =
        dispatcher_prompt_composition(&registry, &MissionId::new("M-quiet"), "执行任务:无事可述");
    assert_eq!(prompt, "执行任务:无事可述");

    drop(bus);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(200), subscriber_handle).await;
}
