use serde::{Deserialize, Serialize};

/// 会话作用域的协议枚举。
///
/// 所有会话级 HTTP 合同共享这一类型，协议层只允许个人会话或项目会话，
/// 不把空字符串、缺失字段或任意文本传入领域层。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionScopeKindDto {
    Personal,
    Workspace,
}
