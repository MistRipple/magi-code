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
define_id!(TaskId);
define_id!(LeaseId);
// P6 Thread 原语（Y 方案）：同 mission + 同 role 持续存在的执行 thread。
// 一个 Thread 跨多个 task 累积上下文，由 DynamicWorkerCatalog 绑定到
// 具体的 worker 实例（WorkerId）。Thread 生命周期随 mission 结束而终止。
define_id!(ThreadId);
