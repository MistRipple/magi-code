use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchCycleBatchStatus {
    Active,
    Summarizing,
    Archived,
}

pub fn should_reset_dispatch_cycle_for_round(
    current_round_request_id: &str,
    root_request_id: &str,
    active_batch_status: Option<DispatchCycleBatchStatus>,
) -> bool {
    if current_round_request_id == root_request_id {
        return false;
    }
    matches!(
        active_batch_status,
        Some(DispatchCycleBatchStatus::Archived) | None
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_request_never_resets() {
        assert!(!should_reset_dispatch_cycle_for_round(
            "req-1",
            "req-1",
            None,
        ));
    }

    #[test]
    fn different_request_with_archived_resets() {
        assert!(should_reset_dispatch_cycle_for_round(
            "req-2",
            "req-1",
            Some(DispatchCycleBatchStatus::Archived),
        ));
    }

    #[test]
    fn different_request_with_none_resets() {
        assert!(should_reset_dispatch_cycle_for_round(
            "req-2",
            "req-1",
            None,
        ));
    }

    #[test]
    fn different_request_with_active_does_not_reset() {
        assert!(!should_reset_dispatch_cycle_for_round(
            "req-2",
            "req-1",
            Some(DispatchCycleBatchStatus::Active),
        ));
    }

    #[test]
    fn different_request_with_summarizing_does_not_reset() {
        assert!(!should_reset_dispatch_cycle_for_round(
            "req-2",
            "req-1",
            Some(DispatchCycleBatchStatus::Summarizing),
        ));
    }
}
