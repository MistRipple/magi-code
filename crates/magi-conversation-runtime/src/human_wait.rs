//! 会话在等人（工具授权、`ask_user_question`）时让出仓库执行租约。
//!
//! 等人不是执行：不让出的话，用户想几分钟，同一仓库的其它会话就几分钟发不出消息。
//! 运行时不直接持有 Git 协调器，由装配层实现 [`HumanWaitHook`]：开始等待时释放租约，
//! 人回应后在把结果交还模型前抢回，仓库被别的会话占着就排队等。
use magi_core::SessionId;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

pub trait HumanWaitHook: Send + Sync {
    /// 开始等人前调用：释放该会话占用的执行租约（没有租约则什么都不做）。
    fn suspend(&self, session_id: &SessionId);
    /// 人回应后调用：重新占用执行租约。仓库正被别的会话占用时返回 `false`，调用方稍后重试。
    fn try_resume(&self, session_id: &SessionId) -> bool;
}

#[derive(Clone, Default)]
pub struct HumanWaitGate {
    hook: Arc<Mutex<Option<Arc<dyn HumanWaitHook>>>>,
}

impl std::fmt::Debug for HumanWaitGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HumanWaitGate").finish_non_exhaustive()
    }
}

impl HumanWaitGate {
    pub fn set_hook(&self, hook: Arc<dyn HumanWaitHook>) {
        *self.hook.lock().expect("human wait hook lock") = Some(hook);
    }

    fn hook(&self) -> Option<Arc<dyn HumanWaitHook>> {
        self.hook.lock().expect("human wait hook lock").clone()
    }

    /// 开始等人。`can_release` 为 `false` 时（会话里还有别的任务在运行，它们仍在使用工作区）
    /// 不让出租约。
    pub fn begin(&self, session_id: &SessionId, can_release: bool) -> HumanWait {
        let suspended = can_release && self.hook().is_some();
        if suspended && let Some(hook) = self.hook() {
            hook.suspend(session_id);
        }
        HumanWait {
            gate: self.clone(),
            session_id: session_id.clone(),
            suspended: AtomicBool::new(suspended),
        }
    }
}

pub struct HumanWait {
    gate: HumanWaitGate,
    session_id: SessionId,
    suspended: AtomicBool,
}

impl HumanWait {
    /// 人回应后抢回租约；仓库被别的会话占用时每 250ms 重试，`keep_waiting` 返回 `false`
    /// （轮次或任务已停止）时放弃并返回 `false`。幂等：已经抢回或从未让出时直接返回 `true`。
    pub fn finish(&self, keep_waiting: impl Fn() -> bool) -> bool {
        if !self.suspended.load(Ordering::SeqCst) {
            return true;
        }
        let Some(hook) = self.gate.hook() else {
            return true;
        };
        loop {
            if hook.try_resume(&self.session_id) {
                self.suspended.store(false, Ordering::SeqCst);
                return true;
            }
            if !keep_waiting() {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct CountingHook {
        suspends: AtomicUsize,
        busy_resumes: AtomicUsize,
    }

    impl HumanWaitHook for CountingHook {
        fn suspend(&self, _session_id: &SessionId) {
            self.suspends.fetch_add(1, Ordering::SeqCst);
        }

        fn try_resume(&self, _session_id: &SessionId) -> bool {
            self.busy_resumes
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_err()
        }
    }

    fn gate(busy_resumes: usize) -> (HumanWaitGate, Arc<CountingHook>) {
        let hook = Arc::new(CountingHook {
            suspends: AtomicUsize::new(0),
            busy_resumes: AtomicUsize::new(busy_resumes),
        });
        let gate = HumanWaitGate::default();
        gate.set_hook(hook.clone());
        (gate, hook)
    }

    #[test]
    fn releases_while_waiting_and_retries_until_the_repository_is_free() {
        let (gate, hook) = gate(2);
        let wait = gate.begin(&SessionId::new("s"), true);
        assert_eq!(hook.suspends.load(Ordering::SeqCst), 1);
        assert!(wait.finish(|| true));
        assert_eq!(hook.busy_resumes.load(Ordering::SeqCst), 0);
        // 再次 finish 不会重复抢占。
        assert!(wait.finish(|| false));
    }

    #[test]
    fn gives_up_when_the_turn_stopped_while_waiting_for_the_repository() {
        let (gate, _hook) = gate(usize::MAX);
        let wait = gate.begin(&SessionId::new("s"), true);
        assert!(!wait.finish(|| false));
    }

    #[test]
    fn keeps_the_lease_when_other_tasks_of_the_session_are_still_running() {
        let (gate, hook) = gate(0);
        let wait = gate.begin(&SessionId::new("s"), false);
        assert_eq!(hook.suspends.load(Ordering::SeqCst), 0);
        assert!(wait.finish(|| false));
    }
}
