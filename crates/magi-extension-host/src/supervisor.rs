//! 单插件监管：启动、崩溃检测、指数退避重启与失败停机。
//!
//! 重启策略（设计文档 5.8）：1 秒起、上限 60 秒的指数退避；10 分钟内崩溃超过 5 次则停止
//! 自动重启并标记失败，等待用户手动重启。失败期间 guard 登记仍保留，受影响动作继续被阻止。

use crate::bus::ExtensionBus;
use crate::instance::{HostInstance, LaunchConfig, launch};
use magi_process::ManagedProcessGroup;
use std::{
    collections::VecDeque,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{Notify, watch},
    task::JoinHandle,
};

#[derive(Clone, Copy, Debug)]
pub struct RestartPolicy {
    pub initial: Duration,
    pub max: Duration,
    pub window: Duration,
    /// 窗口内允许的崩溃次数；超过则停止自动重启。
    pub max_crashes: usize,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(60),
            window: Duration::from_secs(600),
            max_crashes: 5,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartDecision {
    After(Duration),
    GiveUp,
}

#[derive(Debug)]
pub struct RestartTracker {
    policy: RestartPolicy,
    crashes: VecDeque<Instant>,
}

impl RestartTracker {
    pub fn new(policy: RestartPolicy) -> Self {
        Self {
            policy,
            crashes: VecDeque::new(),
        }
    }

    pub fn on_crash(&mut self, now: Instant) -> RestartDecision {
        while self
            .crashes
            .front()
            .is_some_and(|at| now.duration_since(*at) > self.policy.window)
        {
            self.crashes.pop_front();
        }
        self.crashes.push_back(now);
        if self.crashes.len() > self.policy.max_crashes {
            return RestartDecision::GiveUp;
        }
        let exponent = (self.crashes.len() as u32 - 1).min(16);
        let delay = self.policy.initial.saturating_mul(1u32 << exponent);
        RestartDecision::After(delay.min(self.policy.max))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SupervisorState {
    Starting,
    Running {
        generation: u64,
    },
    /// 崩溃或启动失败后等待重启；`reason` 是最近一次失败的原因。
    Backoff {
        crashes: usize,
        reason: String,
    },
    /// 自动重启已停止，等待用户手动重启。
    Failed(String),
    Stopped,
}

pub struct PluginSupervisor {
    state: watch::Receiver<SupervisorState>,
    stop: Arc<Notify>,
    task: JoinHandle<()>,
}

impl PluginSupervisor {
    pub fn state(&self) -> SupervisorState {
        self.state.borrow().clone()
    }

    /// 订阅状态变化（管理面据此刷新界面）。
    pub fn subscribe(&self) -> watch::Receiver<SupervisorState> {
        self.state.clone()
    }

    /// 等待状态满足条件（测试与管理面使用）。
    pub async fn wait_for(&self, predicate: impl Fn(&SupervisorState) -> bool) -> SupervisorState {
        let mut rx = self.state.clone();
        loop {
            let current = rx.borrow_and_update().clone();
            if predicate(&current) {
                return current;
            }
            if rx.changed().await.is_err() {
                return rx.borrow().clone();
            }
        }
    }

    /// 停止监管并回收当前实例。
    pub async fn stop(self) {
        self.stop.notify_one();
        let _ = self.task.await;
    }
}

pub fn supervise(
    group: ManagedProcessGroup,
    bus: Arc<ExtensionBus>,
    config: LaunchConfig,
    policy: RestartPolicy,
) -> PluginSupervisor {
    let (state_tx, state_rx) = watch::channel(SupervisorState::Starting);
    let stop = Arc::new(Notify::new());
    let stop_signal = Arc::clone(&stop);
    let task = tokio::spawn(async move {
        let mut tracker = RestartTracker::new(policy);
        let mut generation = 0u64;
        loop {
            generation += 1;
            state_tx.send_replace(SupervisorState::Starting);
            let launched = tokio::select! {
                result = launch(&group, config.clone(), generation) => result,
                _ = stop_signal.notified() => { state_tx.send_replace(SupervisorState::Stopped); return; }
            };
            let reason = match launched {
                Ok(instance) => {
                    if !bus.attach(Arc::clone(&instance)) {
                        instance.stop().await;
                        state_tx.send_replace(SupervisorState::Failed(
                            "插件未在 ExtensionBus 登记".into(),
                        ));
                        return;
                    }
                    state_tx.send_replace(SupervisorState::Running { generation });
                    let stopped = wait_or_stop(&instance, &stop_signal).await;
                    bus.detach(&config.plugin_id);
                    // 连接失效不等于进程已退出；心跳失败和协议断连也必须回收整棵进程树。
                    instance.stop().await;
                    if let Some(service) = &config.service {
                        service.instance_detached(&config.plugin_id);
                    }
                    if stopped {
                        state_tx.send_replace(SupervisorState::Stopped);
                        return;
                    }
                    "宿主进程退出".to_string()
                }
                Err(error) => error.to_string(),
            };
            match tracker.on_crash(Instant::now()) {
                RestartDecision::GiveUp => {
                    let window = policy.window.as_secs();
                    let window = if window >= 60 && window.is_multiple_of(60) {
                        format!("{} 分钟", window / 60)
                    } else {
                        format!("{window} 秒")
                    };
                    state_tx.send_replace(SupervisorState::Failed(format!(
                        "{window}内崩溃超过 {} 次，已停止自动重启；最近一次：{reason}",
                        policy.max_crashes
                    )));
                    return;
                }
                RestartDecision::After(delay) => {
                    state_tx.send_replace(SupervisorState::Backoff {
                        crashes: tracker.crashes.len(),
                        reason: reason.clone(),
                    });
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = stop_signal.notified() => { state_tx.send_replace(SupervisorState::Stopped); return; }
                    }
                }
            }
        }
    });
    PluginSupervisor {
        state: state_rx,
        stop,
        task,
    }
}

/// 返回 true 表示收到停止请求；false 表示实例自行退出。
async fn wait_or_stop(instance: &Arc<HostInstance>, stop: &Notify) -> bool {
    let mut heartbeat = tokio::time::interval(Duration::from_secs(5));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = instance.wait_exit() => return false,
            _ = stop.notified() => return true,
            _ = heartbeat.tick() => {
                let healthy = tokio::select! {
                    result = instance.ping() => result.is_ok(),
                    _ = stop.notified() => return true,
                };
                if !healthy { return false; }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_cap_and_gives_up_after_too_many_crashes() {
        let mut tracker = RestartTracker::new(RestartPolicy::default());
        let t0 = Instant::now();
        let delays: Vec<_> = (0..5)
            .map(|i| tracker.on_crash(t0 + Duration::from_secs(i)))
            .collect();
        assert_eq!(
            delays,
            [1, 2, 4, 8, 16].map(|s| RestartDecision::After(Duration::from_secs(s)))
        );
        assert_eq!(
            tracker.on_crash(t0 + Duration::from_secs(5)),
            RestartDecision::GiveUp
        );
    }

    #[test]
    fn crashes_outside_the_window_are_forgotten() {
        let mut tracker = RestartTracker::new(RestartPolicy::default());
        let t0 = Instant::now();
        for i in 0..5 {
            tracker.on_crash(t0 + Duration::from_secs(i));
        }
        assert_eq!(
            tracker.on_crash(t0 + Duration::from_secs(700)),
            RestartDecision::After(Duration::from_secs(1))
        );
    }

    #[test]
    fn delay_is_capped() {
        let policy = RestartPolicy {
            max_crashes: 100,
            ..RestartPolicy::default()
        };
        let mut tracker = RestartTracker::new(policy);
        let t0 = Instant::now();
        let last = (0..12)
            .map(|i| tracker.on_crash(t0 + Duration::from_secs(i)))
            .last()
            .unwrap();
        assert_eq!(last, RestartDecision::After(Duration::from_secs(60)));
    }
}
