//! Tier 1 — `TurnDriver` trait：把每一轮模型 IO + 工具 IO 反向注入给 Conversation。
//!
//! Conversation::advance_turn 持有"for round in 0..round_limit"的循环骨架与
//! Turn 状态机推进；具体每一轮内部"拼请求 / 调模型 / 解析响应 / 执行工具 / 写回
//! messages 与 turn item"的业务细节由 driver 实现。
//!
//! 关联类型 `Outcome` 完全抽象，Conversation 不解释最终交付载体。
//!
//! S2 收口边界：
//! - Conversation 拥有 Turn 状态机推进与单飞不变式
//! - Driver 拥有每轮 IO 细节与最终 outcome 构造
//! - 旧式单体调度中的 `for round in 0..limit` 在 driver 内塌缩为
//!   `TurnDriver::execute_round` 实现

/// 一轮 round 的产出——指示 advance_turn 下一步动作。
#[derive(Debug)]
pub enum RoundOutcome {
    /// 模型返回了 tool_calls 或缺失 required tool，需要再来一轮模型调用。
    Continue,
    /// 模型已经给出可交付 final_content，正常结束循环。
    Done,
    /// 本轮 IO 失败（模型调用错误 / 工具批失败到判失败标准），整 Turn 失败。
    Failed(String),
}

/// Tier 1 抽象 driver：所有业务 helpers 隐藏在 impl 内部。
///
/// driver 把"具体一轮发生什么"封装起来，Conversation 只关心：
/// 1. 是否有 deterministic 跳过循环的捷径
/// 2. for-round 循环最多跑多少轮
/// 3. 每轮跑出什么 outcome
/// 4. 循环结束/失败/耗尽时怎么构造最终 Outcome
pub trait TurnDriver {
    /// driver 最终交付给调用方的 outcome 类型——通常是 `TaskOutcome`
    /// 或更上层的封装。Conversation 不解释它的内容。
    type Outcome;

    /// for-round 循环最大轮次。
    fn round_limit(&self) -> usize;

    /// 进入循环前的"无模型调用"捷径——例如 deterministic planning task。
    /// 返回 Some(outcome) 时 advance_turn 直接走 Done 路径，无 modeling 状态。
    fn deterministic_shortcut(&mut self) -> Option<Self::Outcome> {
        None
    }

    /// 每一轮 `execute_round` 调用前的钩子。
    ///
    /// 用于 driver 在进入新一轮模型调用前完成上一轮 ToolCalling 阶段的副作用
    /// 沉淀（把 tool_messages 推入下一轮请求 / 重置中间累加器 / 触发副作用 fence）。
    /// 默认空实现，driver 仅在需要 N-轮跨轮状态衔接时覆写。
    ///
    /// 调用顺序：
    /// - round=0：在 `enter_modeling` 后、`execute_round(0)` 前一次性调用
    /// - round>0：在 `enter_modeling`（从 ToolCalling 回 Modeling）后、
    ///   `execute_round(round)` 前调用
    fn before_round(&mut self, _round: usize) {}

    /// 执行第 `round` 轮：拼请求 → 调模型 → parse → 工具调用 → 写 turn item 等。
    /// 返回 RoundOutcome 指示循环下一步。
    fn execute_round(&mut self, round: usize) -> RoundOutcome;

    /// 循环正常以 `RoundOutcome::Done` 结束后，driver 自己决定最终是 Completed
    /// 还是其他形态（例如 "final_content 空 / lease 失效 / 工具失败兜底
    /// / validation 拒绝" 后置判定，全部落在这里）。
    fn finalize_success(self) -> Self::Outcome;

    /// 某一轮 `execute_round` 返回 `Failed(reason)` 时构造的 outcome。
    fn finalize_round_failure(self, reason: String) -> Self::Outcome;

    /// for-round 跑满 `round_limit` 仍未拿到 Done 时——driver 自定义如何收尾
    /// （通常走"模型未返回可显示回复"路径）。
    fn finalize_exhausted(self) -> Self::Outcome;
}
