use thiserror::Error;

#[derive(Debug, Error)]
pub enum DomainError {
    #[error("实体未找到: {entity}")]
    NotFound { entity: &'static str },
    #[error("实体已存在: {entity}")]
    AlreadyExists { entity: &'static str },
    #[error("非法状态转换: {message}")]
    InvalidState { message: String },
    #[error("会话 {session_id} 已有活动轮次 {active_turn_id}")]
    CurrentTurnConflict {
        session_id: String,
        active_turn_id: String,
    },
    #[error("校验失败: {message}")]
    Validation { message: String },
}

pub type DomainResult<T> = Result<T, DomainError>;
