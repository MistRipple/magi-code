//! 权限档与审批决策。
//!
//! 决策只依赖两个输入：令牌的权限档、工具的风险类别。它是**纯函数**，网络客户端与
//! 本机客户端走同一张表，不允许出现第二套判定。

use serde::{Deserialize, Serialize};

/// 令牌的权限档。默认 `ReadOnly`；网络客户端不得默认高于 `Edit`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// 只读工具。
    #[default]
    ReadOnly,
    /// 允许在工作区内编辑；写入逐次确认。
    Edit,
    /// 在该工作区内预授权编辑，写入无需逐次确认；只能由用户显式授予。破坏性工具与执行不豁免。
    EditTrusted,
    /// 在 `Edit` 之上额外开放 shell；写入与执行都逐次确认。
    Exec,
}

/// 工具的风险类别。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolClass {
    /// 只读。
    Read,
    /// 创建 / 修改。
    Write,
    /// 删除、覆盖式操作、对外推送等：永远逐次确认。
    Destructive,
    /// 执行命令：永远逐次确认。
    Exec,
}

/// 对一次调用的处置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    /// 该权限档不可见也不可调用。
    Deny,
    /// 直接放行。
    Auto,
    /// 需要 Magi 界面里的人工确认。
    RequireApproval,
}

impl Profile {
    pub fn decide(self, class: ToolClass) -> Disposition {
        match (self, class) {
            (_, ToolClass::Read) => Disposition::Auto,

            (Profile::ReadOnly, _) => Disposition::Deny,

            (Profile::Edit, ToolClass::Write) => Disposition::RequireApproval,
            (Profile::EditTrusted, ToolClass::Write) => Disposition::Auto,
            (Profile::Exec, ToolClass::Write) => Disposition::RequireApproval,

            (Profile::Edit | Profile::EditTrusted, ToolClass::Destructive) => {
                Disposition::RequireApproval
            }
            (Profile::Exec, ToolClass::Destructive) => Disposition::RequireApproval,

            (Profile::Edit | Profile::EditTrusted, ToolClass::Exec) => Disposition::Deny,
            (Profile::Exec, ToolClass::Exec) => Disposition::RequireApproval,
        }
    }

    /// 该权限档是否能看到（调用）这个类别的工具。
    pub fn allows(self, class: ToolClass) -> bool {
        self.decide(class) != Disposition::Deny
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_table_is_exactly_the_documented_one() {
        use Disposition::*;
        use ToolClass::*;
        let table = [
            (Profile::ReadOnly, Read, Auto),
            (Profile::ReadOnly, Write, Deny),
            (Profile::ReadOnly, Destructive, Deny),
            (Profile::ReadOnly, Exec, Deny),
            (Profile::Edit, Read, Auto),
            (Profile::Edit, Write, RequireApproval),
            (Profile::Edit, Destructive, RequireApproval),
            (Profile::Edit, Exec, Deny),
            (Profile::EditTrusted, Read, Auto),
            (Profile::EditTrusted, Write, Auto),
            (Profile::EditTrusted, Destructive, RequireApproval),
            (Profile::EditTrusted, Exec, Deny),
            (Profile::Exec, Read, Auto),
            (Profile::Exec, Write, RequireApproval),
            (Profile::Exec, Destructive, RequireApproval),
            (Profile::Exec, Exec, RequireApproval),
        ];
        for (profile, class, expected) in table {
            assert_eq!(profile.decide(class), expected, "{profile:?} × {class:?}");
        }
    }

    #[test]
    fn trusted_profile_never_exempts_destructive_or_exec() {
        assert_eq!(
            Profile::EditTrusted.decide(ToolClass::Destructive),
            Disposition::RequireApproval
        );
        assert_eq!(
            Profile::EditTrusted.decide(ToolClass::Exec),
            Disposition::Deny
        );
        assert_eq!(Profile::default(), Profile::ReadOnly);
    }
}
