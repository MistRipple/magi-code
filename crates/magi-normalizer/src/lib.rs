pub mod base;
pub mod claude;
pub mod codex;
pub mod gemini;
pub mod orchestrator;
pub mod types;

#[cfg(test)]
mod tests;

pub use base::{BaseNormalizer, CallerContext, NormalizerConfig, NormalizerEvent, ParseContext};
pub use claude::{ClaudeNormalizer, create_claude_normalizer};
pub use codex::{CodexNormalizer, create_codex_normalizer};
pub use gemini::{GeminiNormalizer, create_gemini_normalizer};
pub use orchestrator::{
    create_orchestrator_update, get_message_priority, is_internal_state_message,
    normalize_orchestrator_message,
};
pub use types::*;

pub fn create_normalizer(
    _family: NormalizerFamily,
    source: MessageSource,
    debug: bool,
    agent: Option<&str>,
) -> BaseNormalizer {
    let agent_name = agent.unwrap_or("worker");
    let caller_context =
        if agent_name == "orchestrator" || agent_name == "auxiliary" {
            CallerContext::Orchestrator
        } else {
            CallerContext::Worker
        };
    BaseNormalizer::new(NormalizerConfig {
        agent: agent_name.to_string(),
        default_source: source,
        debug,
        caller_context,
    })
}
