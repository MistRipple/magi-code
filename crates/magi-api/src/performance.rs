use magi_core::UtcMillis;
use std::time::Instant;

/// 对话响应链路的轻量时序记录器。
///
/// 只记录稳定标识、阶段和耗时，不记录用户文本、提示词或模型输出。默认通过
/// `magi.performance` target 输出，便于开发环境单独采集并计算 P50/P95。
#[derive(Clone, Debug)]
pub(crate) struct PerformanceTrace {
    trace_id: String,
    started_at: Instant,
}

impl PerformanceTrace {
    pub(crate) fn new(request_id: Option<&str>, accepted_at: UtcMillis) -> Self {
        let trace_id = request_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("trace-turn-{}", accepted_at.0));
        Self {
            trace_id,
            started_at: Instant::now(),
        }
    }

    pub(crate) fn mark(
        &self,
        stage: &'static str,
        session_id: &str,
        turn_id: Option<&str>,
        provider_call_id: Option<&str>,
    ) {
        tracing::info!(
            target: "magi.performance",
            trace_id = %self.trace_id,
            session_id,
            turn_id = turn_id.unwrap_or_default(),
            request_id = %self.trace_id,
            provider_call_id = provider_call_id.unwrap_or_default(),
            stage,
            elapsed_ms = self.started_at.elapsed().as_millis() as u64,
            "conversation response timing"
        );
    }
}
