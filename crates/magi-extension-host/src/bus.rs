//! ExtensionBus：钩子分发与裁决的唯一入口。
//!
//! 对冻结的有序扩展集合逐项调用并保存裁决轨迹；扩展只能阻止或提出受限改写，
//! 不能批准（I2）。guard 失败（异常、超时、熔断、宿主缺失、协议错误）一律阻止。

use crate::breaker::{Admission, Breaker, BreakerPolicy};
use crate::instance::{ApprovedHook, HostInstance};
use crate::protocol::{
    ErrorKind, HookMode, HookRegistration, MAX_REENTRY_DEPTH, RpcError, hook_spec,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;

#[derive(Clone, Copy, Debug)]
pub struct BusPolicy {
    pub breaker: BreakerPolicy,
    /// 每插件 observe 队列容量。
    pub observe_queue: usize,
    /// 同一钩子点总期限上限（按模式取值时的 guard 上限）。
    pub guard_total_cap: Duration,
    pub advise_total_cap: Duration,
}

impl Default for BusPolicy {
    fn default() -> Self {
        Self {
            breaker: BreakerPolicy::default(),
            observe_queue: 1024,
            guard_total_cap: HookMode::Guard.max_deadline(),
            advise_total_cap: HookMode::Advise.max_deadline(),
        }
    }
}

/// 调用上下文（协议第 3 节）。
#[derive(Clone, Debug)]
pub struct HookContext {
    pub invocation_id: String,
    pub parent_invocation_id: Option<String>,
    pub depth: u32,
    pub owner: Value,
    pub binding: Value,
}

impl HookContext {
    pub fn new(invocation_id: impl Into<String>) -> Self {
        Self {
            invocation_id: invocation_id.into(),
            parent_invocation_id: None,
            depth: 0,
            owner: Value::Null,
            binding: Value::Null,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TraceOutcome {
    Continue,
    Replaced,
    Blocked(String),
    /// 失败但按模式继续（advise）。
    Failed(ErrorKind),
    Skipped(&'static str),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TraceEntry {
    pub plugin: String,
    pub mode: HookMode,
    pub outcome: TraceOutcome,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// 所有扩展处理后的最终候选；仍须原生校验（I3）。
    Proceed(Value),
    Blocked {
        plugin: String,
        reason: String,
    },
}

#[derive(Clone, Debug)]
pub struct HookOutcome {
    pub verdict: Verdict,
    pub trace: Vec<TraceEntry>,
    /// 每个提出 `replace` 的插件各自的补丁（按调用顺序）。合并后的候选无法区分来源，
    /// 需要按插件归因的钩子（如扩展改写记录）读取这里。
    pub patches: Vec<(String, Value)>,
}

/// (序号, 钩子名, 输入)。
type QueuedEvent = (u64, Value, Value);

struct ObserveQueue {
    /// 队列与已丢弃计数（缺口）。
    items: Mutex<(VecDeque<QueuedEvent>, u64)>,
    notify: Notify,
    seq: AtomicU64,
    capacity: usize,
}

impl ObserveQueue {
    fn new(capacity: usize) -> Self {
        Self {
            items: Mutex::new((VecDeque::new(), 0)),
            notify: Notify::new(),
            seq: AtomicU64::new(1),
            capacity,
        }
    }

    /// 队列满则丢弃最旧并记缺口，不阻塞生产者。
    fn push(&self, hook: Value, input: Value) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        {
            let mut guard = self.items.lock().expect("observe queue poisoned");
            if guard.0.len() >= self.capacity {
                guard.0.pop_front();
                guard.1 += 1;
            }
            guard.0.push_back((seq, hook, input));
        }
        self.notify.notify_one();
    }
}

struct Slot {
    plugin_id: String,
    hooks: HashMap<String, ApprovedHook>,
    instance: RwLock<Option<Arc<HostInstance>>>,
    breaker: Mutex<Breaker>,
    observe: Arc<ObserveQueue>,
    /// 用户已授权 `contextRewrite`：只有这样的插件提出的历史改写才会被采纳。
    context_rewrite: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
pub struct ExtensionBus {
    policy: BusPolicy,
    slots: RwLock<HashMap<String, Arc<Slot>>>,
    /// 用户配置的调用顺序；未排序的插件按 ID 稳定排序。
    order: RwLock<Vec<String>>,
    next_hook_id: AtomicU64,
}

impl ExtensionBus {
    pub fn new(policy: BusPolicy) -> Self {
        Self {
            policy,
            ..Self::default()
        }
    }

    /// 启用插件时登记其获批钩子。即使宿主崩溃，guard 登记仍保留，因此宿主缺失会阻止动作。
    pub fn register_plugin(
        &self,
        plugin_id: &str,
        hooks: &[HookRegistration],
    ) -> Result<(), RpcError> {
        let mut approved = HashMap::new();
        for hook in hooks {
            let deadline = hook.validate()?;
            approved.insert(
                hook.name.clone(),
                ApprovedHook {
                    mode: hook.mode,
                    deadline,
                },
            );
        }
        let slot = Arc::new(Slot {
            plugin_id: plugin_id.to_string(),
            hooks: approved,
            instance: RwLock::new(None),
            breaker: Mutex::new(Breaker::new(self.policy.breaker)),
            observe: Arc::new(ObserveQueue::new(self.policy.observe_queue)),
            context_rewrite: std::sync::atomic::AtomicBool::new(false),
        });
        self.slots
            .write()
            .expect("slots poisoned")
            .insert(plugin_id.to_string(), slot);
        Ok(())
    }

    /// 禁用/卸载：移除登记（guard 也随之解除，这是用户显式停用的结果）。
    pub fn unregister_plugin(&self, plugin_id: &str) {
        self.slots
            .write()
            .expect("slots poisoned")
            .remove(plugin_id);
    }

    /// 记录插件是否已被授权 `contextRewrite`（由插件管理的授权事实驱动）。
    pub fn set_context_rewrite_authorized(&self, plugin_id: &str, authorized: bool) {
        if let Some(slot) = self.slots.read().expect("slots poisoned").get(plugin_id) {
            slot.context_rewrite
                .store(authorized, std::sync::atomic::Ordering::SeqCst);
        }
    }

    pub fn is_context_rewrite_authorized(&self, plugin_id: &str) -> bool {
        self.slots
            .read()
            .expect("slots poisoned")
            .get(plugin_id)
            .is_some_and(|slot| {
                slot.context_rewrite
                    .load(std::sync::atomic::Ordering::SeqCst)
            })
    }

    pub fn set_order(&self, order: Vec<String>) {
        *self.order.write().expect("order poisoned") = order;
    }

    /// 绑定运行中的宿主实例并启动其 observe 投递。
    pub fn attach(&self, instance: Arc<HostInstance>) -> bool {
        let Some(slot) = self
            .slots
            .read()
            .expect("slots poisoned")
            .get(&instance.plugin_id)
            .cloned()
        else {
            return false;
        };
        *slot.instance.write().expect("instance poisoned") = Some(Arc::clone(&instance));
        let queue = Arc::clone(&slot.observe);
        tokio::spawn(async move {
            loop {
                let next = {
                    let mut guard = queue.items.lock().expect("observe queue poisoned");
                    let gap = std::mem::take(&mut guard.1);
                    (guard.0.pop_front(), gap)
                };
                match next {
                    (Some((seq, hook, input)), gap) => {
                        if !instance.is_alive() {
                            return;
                        }
                        if gap > 0 {
                            let _ = instance.peer.notify(
                                "hook.gap",
                                json!({"dropped": gap, "reason": "queue_overflow"}),
                            );
                        }
                        let params = json!({"hook": hook, "seq": seq, "input": input});
                        // 通知通道满时短暂退避重试；仍失败则计入缺口，不无界缓存。
                        if instance.peer.notify("hook.notify", params).is_err() {
                            queue.items.lock().expect("observe queue poisoned").1 += 1;
                            tokio::time::sleep(Duration::from_millis(20)).await;
                        }
                    }
                    (None, _) => {
                        tokio::select! {
                            _ = queue.notify.notified() => {}
                            _ = instance.wait_exit() => return,
                        }
                    }
                }
            }
        });
        true
    }

    pub fn detach(&self, plugin_id: &str) {
        if let Some(slot) = self.slots.read().expect("slots poisoned").get(plugin_id) {
            *slot.instance.write().expect("instance poisoned") = None;
        }
    }

    fn frozen(&self, hook: &str, observe: bool) -> Vec<Arc<Slot>> {
        let slots = self.slots.read().expect("slots poisoned");
        let order = self.order.read().expect("order poisoned");
        let mut chosen: Vec<Arc<Slot>> = slots
            .values()
            .filter(|slot| {
                slot.hooks
                    .get(hook)
                    .is_some_and(|h| (h.mode == HookMode::Observe) == observe)
            })
            .cloned()
            .collect();
        chosen.sort_by_key(|slot| {
            (
                order
                    .iter()
                    .position(|id| *id == slot.plugin_id)
                    .unwrap_or(usize::MAX),
                slot.plugin_id.clone(),
            )
        });
        chosen
    }

    /// 是否有插件为该钩子登记了 guard/advise 模式。用于在无扩展时跳过分发。
    pub fn has_hook(&self, hook: &str) -> bool {
        self.slots
            .read()
            .expect("slots poisoned")
            .values()
            .any(|slot| {
                slot.hooks
                    .get(hook)
                    .is_some_and(|h| h.mode != HookMode::Observe)
            })
    }

    /// 观察通知：有界异步队列，不阻塞调用方。
    pub fn notify(&self, hook: &str, input: Value) {
        for slot in self.frozen(hook, true) {
            let bound = slot
                .instance
                .read()
                .expect("instance poisoned")
                .as_ref()
                .is_some_and(|i| i.is_bound(hook));
            if bound {
                slot.observe.push(json!(hook), input.clone());
            }
        }
    }

    /// guard/advise 分发。返回最终候选或阻止；不批准任何动作。
    pub async fn invoke(
        &self,
        hook: &str,
        ctx: &HookContext,
        input: Value,
    ) -> Result<HookOutcome, RpcError> {
        let spec = hook_spec(hook)
            .ok_or_else(|| RpcError::new(ErrorKind::MethodNotFound, format!("未知钩子 {hook}")))?;
        if ctx.depth > MAX_REENTRY_DEPTH {
            return Err(RpcError::new(ErrorKind::ReentryDenied, "重入深度超过上限"));
        }
        let slots = self.frozen(hook, false);
        let total = slots
            .iter()
            .fold((Duration::ZERO, Duration::ZERO), |(guard, advise), slot| {
                let approved = slot.hooks[hook];
                match approved.mode {
                    HookMode::Guard => (guard + approved.deadline, advise),
                    _ => (guard, advise + approved.deadline),
                }
            });
        let guard_budget = total.0.min(self.policy.guard_total_cap);
        let advise_budget = total.1.min(self.policy.advise_total_cap);
        let started = Instant::now();

        let mut candidate = input;
        let mut trace = Vec::new();
        let mut patches: Vec<(String, Value)> = Vec::new();
        for slot in slots {
            let approved = slot.hooks[hook];
            let plugin = slot.plugin_id.clone();
            let guard = approved.mode == HookMode::Guard;
            let budget = if guard { guard_budget } else { advise_budget };
            let fail = |trace: &mut Vec<TraceEntry>,
                        kind: ErrorKind,
                        why: &'static str|
             -> Option<Verdict> {
                if guard {
                    trace.push(TraceEntry {
                        plugin: plugin.clone(),
                        mode: approved.mode,
                        outcome: TraceOutcome::Blocked(why.into()),
                    });
                    Some(Verdict::Blocked {
                        plugin: plugin.clone(),
                        reason: format!("{why}（{kind:?}）"),
                    })
                } else {
                    trace.push(TraceEntry {
                        plugin: plugin.clone(),
                        mode: approved.mode,
                        outcome: TraceOutcome::Failed(kind),
                    });
                    None
                }
            };

            let admission = slot
                .breaker
                .lock()
                .expect("breaker poisoned")
                .admit(Instant::now());
            if admission == Admission::Rejected {
                match fail(&mut trace, ErrorKind::Backpressure, "扩展熔断已打开") {
                    Some(verdict) => {
                        return Ok(HookOutcome {
                            verdict,
                            trace,
                            patches: Vec::new(),
                        });
                    }
                    None => continue,
                }
            }
            let instance = slot.instance.read().expect("instance poisoned").clone();
            let Some(instance) = instance.filter(|i| i.is_alive() && i.is_bound(hook)) else {
                // 宿主缺失不是熔断样本，但 guard 必须阻止。
                if admission == Admission::Probe {
                    slot.breaker
                        .lock()
                        .expect("breaker poisoned")
                        .record(Instant::now(), false);
                }
                match fail(&mut trace, ErrorKind::HostGone, "扩展宿主不可用") {
                    Some(verdict) => {
                        return Ok(HookOutcome {
                            verdict,
                            trace,
                            patches: Vec::new(),
                        });
                    }
                    None => continue,
                }
            };

            let remaining = budget.saturating_sub(started.elapsed());
            let timeout = approved.deadline.min(remaining);
            if timeout.is_zero() {
                match fail(
                    &mut trace,
                    ErrorKind::DeadlineExceeded,
                    "钩子点总期限已用尽",
                ) {
                    Some(verdict) => {
                        return Ok(HookOutcome {
                            verdict,
                            trace,
                            patches: Vec::new(),
                        });
                    }
                    None => continue,
                }
            }
            let hook_id = format!("hk_{}", self.next_hook_id.fetch_add(1, Ordering::SeqCst));
            let invocation_id = format!("{}.{}", ctx.invocation_id, hook_id);
            let params = json!({
                "hook": hook,
                "ctx": {
                    "invocationId": invocation_id,
                    "hookId": hook_id,
                    "parentInvocationId": ctx.parent_invocation_id.clone().or_else(|| Some(ctx.invocation_id.clone())),
                    "generation": instance.generation,
                    "deadlineMs": timeout.as_millis() as u64,
                    "depth": ctx.depth + 1,
                    "owner": ctx.owner,
                    "binding": ctx.binding,
                },
                "input": candidate,
            });
            let result = instance.peer.request("hook.invoke", params, timeout).await;
            if matches!(&result, Err(e) if e.kind() == ErrorKind::DeadlineExceeded) {
                let _ = instance.peer.notify(
                    "host.cancel",
                    json!({"invocationId": invocation_id, "reason": "deadline"}),
                );
            }
            let parsed =
                result.and_then(|value| parse_decision(&value, spec.replace, spec.patch_limit));
            match parsed {
                Ok(Decision::Continue) => {
                    slot.breaker
                        .lock()
                        .expect("breaker poisoned")
                        .record(Instant::now(), true);
                    trace.push(TraceEntry {
                        plugin,
                        mode: approved.mode,
                        outcome: TraceOutcome::Continue,
                    });
                }
                Ok(Decision::Replace(patch)) => {
                    slot.breaker
                        .lock()
                        .expect("breaker poisoned")
                        .record(Instant::now(), true);
                    patches.push((plugin.clone(), patch.clone()));
                    merge(&mut candidate, patch);
                    trace.push(TraceEntry {
                        plugin,
                        mode: approved.mode,
                        outcome: TraceOutcome::Replaced,
                    });
                }
                Ok(Decision::Block(reason)) => {
                    slot.breaker
                        .lock()
                        .expect("breaker poisoned")
                        .record(Instant::now(), true);
                    trace.push(TraceEntry {
                        plugin: plugin.clone(),
                        mode: approved.mode,
                        outcome: TraceOutcome::Blocked(reason.clone()),
                    });
                    return Ok(HookOutcome {
                        verdict: Verdict::Blocked { plugin, reason },
                        trace,
                        patches,
                    });
                }
                Err(error) => {
                    slot.breaker
                        .lock()
                        .expect("breaker poisoned")
                        .record(Instant::now(), false);
                    if let Some(verdict) = fail(&mut trace, error.kind(), "扩展调用失败") {
                        return Ok(HookOutcome {
                            verdict,
                            trace,
                            patches: Vec::new(),
                        });
                    }
                }
            }
        }
        Ok(HookOutcome {
            verdict: Verdict::Proceed(candidate),
            trace,
            patches,
        })
    }
}

enum Decision {
    Continue,
    Block(String),
    Replace(Value),
}

fn parse_decision(
    value: &Value,
    replace_allowed: bool,
    patch_limit: usize,
) -> Result<Decision, RpcError> {
    let bad = |message: &str| RpcError::new(ErrorKind::ProtocolError, message.to_string());
    match value.get("decision").and_then(Value::as_str) {
        Some("continue") => Ok(Decision::Continue),
        Some("block") => {
            let reason = value
                .get("reason")
                .and_then(Value::as_str)
                .filter(|r| !r.trim().is_empty());
            reason
                .map(|r| Decision::Block(r.chars().take(1024).collect()))
                .ok_or_else(|| bad("block 必须带 reason"))
        }
        Some("replace") => {
            if !replace_allowed {
                return Err(bad("该钩子不允许 replace"));
            }
            let patch = value
                .get("patch")
                .filter(|p| p.is_object())
                .ok_or_else(|| bad("replace 必须带对象 patch"))?;
            if serde_json::to_vec(patch).map_or(usize::MAX, |b| b.len()) > patch_limit {
                return Err(bad("patch 超过该钩子上限"));
            }
            Ok(Decision::Replace(patch.clone()))
        }
        _ => Err(bad("decision 必须是 continue、block 或 replace")),
    }
}

/// 浅合并：补丁字段覆盖候选同名字段；候选不是对象时以补丁为准。最终仍由 daemon 原生校验（I3）。
fn merge(candidate: &mut Value, patch: Value) {
    match (candidate.as_object_mut(), patch) {
        (Some(target), Value::Object(patch)) => {
            for (key, value) in patch {
                target.insert(key, value);
            }
        }
        (_, patch) => *candidate = patch,
    }
}
