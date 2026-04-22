pub fn build_dispatch_worker_lane_id(dispatch_wave_id: &str, worker: &str) -> String {
    format!("{dispatch_wave_id}:{worker}")
}

pub fn build_dispatch_worker_card_id(dispatch_wave_id: &str, worker: &str) -> String {
    format!("dispatch-lane-card-{dispatch_wave_id}-{worker}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_id_format() {
        assert_eq!(
            build_dispatch_worker_lane_id("wave-1", "worker-a"),
            "wave-1:worker-a"
        );
    }

    #[test]
    fn card_id_format() {
        assert_eq!(
            build_dispatch_worker_card_id("wave-1", "worker-a"),
            "dispatch-lane-card-wave-1-worker-a"
        );
    }
}
