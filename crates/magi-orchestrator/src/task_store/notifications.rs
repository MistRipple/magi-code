use super::*;

impl TaskStore {
    /// Create a store with a callback that fires after every successful
    /// `update_status` call.
    pub fn with_status_change_callback(callback: StatusChangeCallback) -> Self {
        Self {
            tasks: RwLock::new(HashMap::new()),
            leases: RwLock::new(HashMap::new()),
            mission_index: RwLock::new(HashMap::new()),
            on_status_change: Mutex::new(Some(Arc::from(callback))),
            on_checkpoint: Mutex::new(None),
            mutation_lock: Mutex::new(()),
            status_change_version: Mutex::new(0),
            runtime_change_notify: tokio::sync::Notify::new(),
            status_change_signal: Condvar::new(),
            status_change_callback_async: AtomicBool::new(false),
        }
    }

    /// Set or replace the status-change callback.
    ///
    /// This is useful after restoring from a checkpoint, since callbacks are
    /// not serialized.
    pub fn set_status_change_callback(&self, callback: StatusChangeCallback) {
        let mut guard = self
            .on_status_change
            .lock()
            .expect("on_status_change lock poisoned");
        *guard = Some(Arc::from(callback));
    }

    /// 生产路径启用异步状态通知，保证 callback 不会在 TaskStore mutation guard
    /// 生命周期内执行。同步模式只用于不涉及外部 IO 的嵌入测试。
    pub fn set_status_change_callback_async(&self, enabled: bool) {
        self.status_change_callback_async
            .store(enabled, Ordering::Release);
    }

    /// Set or replace the per-transition checkpoint callback (design 6.8).
    pub fn set_checkpoint_callback(&self, callback: CheckpointCallback) {
        let mut guard = self
            .on_checkpoint
            .lock()
            .expect("on_checkpoint lock poisoned");
        *guard = Some(callback);
    }

    /// 判断状态变更是否已经接入 durable checkpoint 回调。
    ///
    /// Runner 的调度循环会收到“状态已变化”的信号，但生产 TaskStore 在每次
    /// 变更提交前已经通过该回调完成增量 checkpoint。暴露这个只读事实让 Runner
    /// 避免在终态再次写一份全量快照；未配置回调的轻量测试/嵌入场景仍保留原有
    /// `with_checkpoint_persist` 兜底。
    pub fn has_checkpoint_callback(&self) -> bool {
        self.on_checkpoint
            .lock()
            .expect("on_checkpoint lock poisoned")
            .is_some()
    }

    pub(super) fn fire_checkpoint(&self, snapshot: &TaskStoreSnapshot) -> DomainResult<()> {
        let guard = self
            .on_checkpoint
            .lock()
            .expect("on_checkpoint lock poisoned");
        if let Some(ref cb) = *guard {
            cb(snapshot)?;
        }
        Ok(())
    }

    pub(super) fn emit_status_change(
        &self,
        task_id: &TaskId,
        old_status: TaskStatus,
        new_status: TaskStatus,
        task: Task,
    ) {
        // 状态事实已经通过 checkpoint 和 map 提交完成后才进入这里。回调可能触发
        // SessionStore、事件总线或 Runner 收口，不能在 mutation_lock 生命周期内执行；
        // 复制轻量句柄并异步投递，确保任务状态提交不会被跨模块 IO 反向阻塞。
        let callback = self
            .on_status_change
            .lock()
            .expect("on_status_change lock poisoned")
            .clone();
        if let Some(callback) = callback {
            if self.status_change_callback_async.load(Ordering::Acquire) {
                let callback_task_id = task_id.clone();
                let log_task_id = callback_task_id.clone();
                if let Err(error) = std::thread::Builder::new()
                    .name("magi-task-status-notify".to_string())
                    .spawn(move || callback(&callback_task_id, old_status, new_status, task))
                {
                    tracing::warn!(?error, %log_task_id, "异步任务状态通知线程启动失败");
                }
            } else {
                callback(task_id, old_status, new_status, task);
            }
        }
        self.notify_status_change();
    }

    pub(super) fn notify_status_change(&self) {
        let mut version = self
            .status_change_version
            .lock()
            .expect("status_change_version lock poisoned");
        *version = version.saturating_add(1);
        self.status_change_signal.notify_all();
        self.runtime_change_notify.notify_waiters();
    }

    pub fn status_change_version(&self) -> u64 {
        *self
            .status_change_version
            .lock()
            .expect("status_change_version lock poisoned")
    }

    /// 异步宿主复用相同的提交后通知；先取 future 再读事实，避免查询期间丢失唤醒。
    /// 不复制 Task/转交状态，也不需要轮询或为每个等待者启动阻塞线程。
    pub fn runtime_change_notification(&self) -> impl std::future::Future<Output = ()> + Send + '_ {
        self.runtime_change_notify.notified()
    }

    /// 唤醒等待任务运行态变化的协调器。用于非状态字段但会改变编排决策的事件，
    /// 例如子代理发起上下文请求。
    pub fn notify_runtime_change(&self) {
        self.notify_status_change();
    }

    pub fn wait_for_status_change_since(&self, observed_version: u64, timeout: Duration) -> u64 {
        let version = self
            .status_change_version
            .lock()
            .expect("status_change_version lock poisoned");
        if *version != observed_version || timeout.is_zero() {
            return *version;
        }
        let (version, _) = self
            .status_change_signal
            .wait_timeout_while(version, timeout, |current| *current == observed_version)
            .expect("status_change_signal wait poisoned");
        *version
    }
}
