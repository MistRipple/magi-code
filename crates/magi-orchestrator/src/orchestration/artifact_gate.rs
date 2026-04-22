#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MissionDeliveryStatus {
    Passed,
    Failed,
    Pending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnDeliveryAggregateState {
    Running,
    Completed,
    Failed,
    NeedsRework,
    Blocked,
}

pub fn has_satisfied_orchestration_artifacts(
    delivery_status: Option<MissionDeliveryStatus>,
    turn_delivery_state: Option<TurnDeliveryAggregateState>,
) -> bool {
    delivery_status == Some(MissionDeliveryStatus::Passed)
        || turn_delivery_state == Some(TurnDeliveryAggregateState::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passed_delivery_satisfies() {
        assert!(has_satisfied_orchestration_artifacts(
            Some(MissionDeliveryStatus::Passed),
            None,
        ));
    }

    #[test]
    fn completed_turn_satisfies() {
        assert!(has_satisfied_orchestration_artifacts(
            None,
            Some(TurnDeliveryAggregateState::Completed),
        ));
    }

    #[test]
    fn failed_delivery_does_not_satisfy() {
        assert!(!has_satisfied_orchestration_artifacts(
            Some(MissionDeliveryStatus::Failed),
            None,
        ));
    }

    #[test]
    fn none_does_not_satisfy() {
        assert!(!has_satisfied_orchestration_artifacts(None, None));
    }

    #[test]
    fn both_satisfied() {
        assert!(has_satisfied_orchestration_artifacts(
            Some(MissionDeliveryStatus::Passed),
            Some(TurnDeliveryAggregateState::Completed),
        ));
    }
}
