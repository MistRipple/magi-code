//! 熔断器：连续失败或窗口内失败率过半则打开；冷却后半开，只放行一次探测。

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug)]
pub struct BreakerPolicy {
    pub consecutive_failures: u32,
    pub window: Duration,
    pub window_min_calls: usize,
    pub cooldown: Duration,
}

impl Default for BreakerPolicy {
    fn default() -> Self {
        Self {
            consecutive_failures: 5,
            window: Duration::from_secs(60),
            window_min_calls: 10,
            cooldown: Duration::from_secs(30),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    Allowed,
    /// 半开状态的唯一探测调用。
    Probe,
    Rejected,
}

#[derive(Debug)]
enum State {
    Closed,
    Open { until: Instant },
    HalfOpen { probing: bool },
}

#[derive(Debug)]
pub struct Breaker {
    policy: BreakerPolicy,
    state: State,
    consecutive: u32,
    window: VecDeque<(Instant, bool)>,
}

impl Breaker {
    pub fn new(policy: BreakerPolicy) -> Self {
        Self {
            policy,
            state: State::Closed,
            consecutive: 0,
            window: VecDeque::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        !matches!(self.state, State::Closed)
    }

    pub fn admit(&mut self, now: Instant) -> Admission {
        match self.state {
            State::Closed => Admission::Allowed,
            State::Open { until } if now >= until => {
                self.state = State::HalfOpen { probing: true };
                Admission::Probe
            }
            State::Open { .. } => Admission::Rejected,
            State::HalfOpen { probing: false } => {
                self.state = State::HalfOpen { probing: true };
                Admission::Probe
            }
            State::HalfOpen { probing: true } => Admission::Rejected,
        }
    }

    pub fn record(&mut self, now: Instant, ok: bool) {
        if let State::HalfOpen { .. } = self.state {
            if ok {
                self.state = State::Closed;
                self.consecutive = 0;
                self.window.clear();
            } else {
                self.state = State::Open {
                    until: now + self.policy.cooldown,
                };
            }
            return;
        }
        while self
            .window
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > self.policy.window)
        {
            self.window.pop_front();
        }
        self.window.push_back((now, ok));
        self.consecutive = if ok { 0 } else { self.consecutive + 1 };
        let failures = self.window.iter().filter(|(_, ok)| !ok).count();
        let tripped = self.consecutive >= self.policy.consecutive_failures
            || (self.window.len() >= self.policy.window_min_calls
                && failures * 2 > self.window.len());
        if tripped {
            self.state = State::Open {
                until: now + self.policy.cooldown,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consecutive_failures_open_and_a_single_probe_decides() {
        let mut breaker = Breaker::new(BreakerPolicy::default());
        let t0 = Instant::now();
        for _ in 0..4 {
            breaker.record(t0, false);
        }
        assert_eq!(breaker.admit(t0), Admission::Allowed);
        breaker.record(t0, false);
        assert_eq!(breaker.admit(t0), Admission::Rejected);

        let later = t0 + Duration::from_secs(31);
        assert_eq!(breaker.admit(later), Admission::Probe);
        assert_eq!(
            breaker.admit(later),
            Admission::Rejected,
            "半开只放行一次探测"
        );
        breaker.record(later, false);
        assert_eq!(
            breaker.admit(later),
            Admission::Rejected,
            "探测失败重新打开"
        );

        let again = later + Duration::from_secs(31);
        assert_eq!(breaker.admit(again), Admission::Probe);
        breaker.record(again, true);
        assert_eq!(breaker.admit(again), Admission::Allowed);
        assert!(!breaker.is_open());
    }

    #[test]
    fn failure_rate_over_half_in_the_window_opens_without_a_streak() {
        let mut breaker = Breaker::new(BreakerPolicy::default());
        let t0 = Instant::now();
        // 交替失败/成功不会形成连续 5 次，但 10 次调用中 6 次失败。
        let pattern = [
            false, true, false, true, false, true, false, true, false, false,
        ];
        for ok in pattern {
            breaker.record(t0, ok);
        }
        assert_eq!(breaker.admit(t0), Admission::Rejected);
    }

    #[test]
    fn old_calls_leave_the_window() {
        let mut breaker = Breaker::new(BreakerPolicy::default());
        let t0 = Instant::now();
        for _ in 0..4 {
            breaker.record(t0, false);
        }
        // 窗口外的失败不再计入失败率，但连续计数独立：一次成功即清零。
        breaker.record(t0 + Duration::from_secs(120), true);
        breaker.record(t0 + Duration::from_secs(121), false);
        assert_eq!(
            breaker.admit(t0 + Duration::from_secs(121)),
            Admission::Allowed
        );
    }
}
