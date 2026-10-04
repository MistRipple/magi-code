use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

macro_rules! define_id {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }
    };
}

define_id!(WorkspaceId);
define_id!(SessionId);
define_id!(MissionId);
define_id!(AssignmentId);
define_id!(WorkerId);
define_id!(ToolCallId);
define_id!(EventId);

impl EventId {
    /// 生成唯一事件 ID：`{kind}-{毫秒}-{进程实例}-{进程内序号}`。
    ///
    /// 事件总线要求 `event_id` 唯一（审计/用量账本恢复时按它去重），只用毫秒时间戳拼接的
    /// ID 在同一毫秒内或并发发布时会重复。没有可由实体身份（turn、queue、tool call 等）
    /// 推导出确定性 ID 的事件都必须用它生成。进程实例由进程号与启动时刻组成，重启前后
    /// 也不会与已持久化的 ID 冲突。
    pub fn unique(kind: impl Display) -> Self {
        use std::sync::OnceLock;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        static INSTANCE: OnceLock<String> = OnceLock::new();
        let instance = INSTANCE.get_or_init(|| {
            let started_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default();
            format!("{:x}{:x}", std::process::id(), started_at)
        });
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self(format!(
            "{kind}-{}-{instance}-{sequence}",
            crate::UtcMillis::now().0
        ))
    }
}
define_id!(GoalId);
define_id!(PlanId);
define_id!(PlanItemId);
define_id!(TaskId);
define_id!(LeaseId);
define_id!(BrowserProfileId);
define_id!(BrowserSessionId);
define_id!(BrowserTabId);
define_id!(BrowserLeaseId);
define_id!(BrowserCommandId);
define_id!(BrowserAnnotationId);
// P6 Thread 原语（Y 方案）：同 mission + 同 role 持续存在的执行 thread。
// 一个 Thread 跨多个 task 累积上下文，由 DynamicWorkerCatalog 绑定到
// 具体的 worker 实例（WorkerId）。Thread 生命周期随 mission 结束而终止。
define_id!(ThreadId);

#[cfg(test)]
mod tests {
    use super::EventId;
    use std::collections::HashSet;

    #[test]
    fn unique_event_ids_never_repeat_within_the_same_millisecond() {
        let ids = (0..10_000)
            .map(|_| EventId::unique("event-session-turn-item"))
            .collect::<Vec<_>>();
        let distinct = ids.iter().map(EventId::as_str).collect::<HashSet<_>>();
        assert_eq!(distinct.len(), ids.len());
        assert!(ids[0].as_str().starts_with("event-session-turn-item-"));
    }
}
