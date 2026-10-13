//! GPT Web 槽位的工具档与 MCP 授权方式。
//!
//! 工具档决策只依赖两个输入：槽位的工具档、工具的风险类别，是**纯函数**。MCP 令牌不使用
//! 工具档：它们固定开放完整工具目录，只由 [`ApprovalMode`] 决定逐次询问还是自动执行。

use serde::{Deserialize, Serialize};

/// GPT Web 槽位按设置选择的工具档。MCP 令牌不携带工具档，对外一律完整工具目录。
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

/// 需要人工确认的调用如何处理。
///
/// `Always` 自动放行完整工具目录里的调用；Magi 自身的路径限制、安全策略和硬性拒绝仍生效。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalMode {
    /// 默认每次 MCP 工具调用都在 Magi 中询问。
    #[default]
    Ask,
    /// 自动允许所有 MCP 工具调用。
    Always,
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
