//! 只读研究按新增观测推进，读取轮数本身不构成无进展。
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Default)]
pub(crate) struct DiscoveryProgress {
    seen: BTreeSet<(String, String, [u8; 32])>,
    stagnant_rounds: usize,
    new_evidence: bool,
}

impl DiscoveryProgress {
    pub(crate) fn observe(
        &mut self,
        name: &str,
        arguments: &str,
        result: &str,
        executed_successfully: bool,
    ) {
        if !executed_successfully {
            return;
        }
        let arguments = crate::tool_result_utils::normalized_tool_arguments(arguments);
        let digest = Sha256::digest(result.as_bytes()).into();
        self.new_evidence |= self.seen.insert((name.into(), arguments, digest));
    }
    pub(crate) fn finish_round(&mut self, had_discovery: bool, had_other_success: bool) {
        if self.new_evidence || had_other_success {
            self.stagnant_rounds = 0;
        } else if had_discovery {
            self.stagnant_rounds = self.stagnant_rounds.saturating_add(1);
        }
        self.new_evidence = false;
    }
    pub(crate) fn should_warn(&self) -> bool {
        self.stagnant_rounds == 8
    }
    pub(crate) fn exhausted(&self) -> bool {
        self.stagnant_rounds >= 12
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinct_read_only_evidence_can_continue_beyond_the_old_limit() {
        let mut progress = DiscoveryProgress::default();
        for index in 0..100 {
            progress.observe(
                "file_read",
                &format!("{{\"path\":\"{index}.rs\"}}"),
                "new contents",
                true,
            );
            progress.finish_round(true, false);
            assert!(!progress.exhausted());
        }
    }
    #[test]
    fn unchanged_observations_and_cached_replays_do_not_fake_progress() {
        let mut progress = DiscoveryProgress::default();
        progress.observe("file_read", r#"{"path":"main.rs"}"#, "unchanged", true);
        progress.finish_round(true, false);
        for _ in 0..12 {
            progress.observe("file_read", r#"{ "path": "main.rs" }"#, "unchanged", true);
            progress.finish_round(true, false);
        }
        assert!(progress.exhausted());
        progress.observe("file_read", r#"{"path":"main.rs"}"#, "changed", true);
        progress.finish_round(true, false);
        assert!(!progress.exhausted());
    }
}
