//! `magi-lifecycle-notice`：把 mission 生命周期事件桥接到下轮 prompt 的"生命周期通知"段。
//!
//! 目标：模型不需要主动查工具就能知道 mission 状态发生了什么变化——
//! recovery、人审 resolve、plan step 完成。我们订阅 event bus，按 mission 维度
//! 维护一个小队列，dispatcher 每轮派发前 `pending_notice` 拉一次。
//!
//! Slot 语义（关键设计，避免重复注入）：
//! - `mission_resumed`：一次性，读后清空——recovery 注入只应在恢复后第一轮提示
//! - `human_checkpoint`：approved/rejected 共享一个 slot，后来覆盖前者
//! - `plan_step_completed`：独立 slot，后来覆盖前者（保留最近一次进度信号）
//!
//! 这与 `prepend_session_instructions` 的"生命周期通知"段一起构成完整桥：
//! event publish → registry.ingest → dispatcher.pending_notice → prompt 注入。

mod notice_queue;
mod subscriber;
mod templates;

pub use notice_queue::{LifecycleNoticeRegistry, MissionNoticeState};
pub use subscriber::run_subscriber;
