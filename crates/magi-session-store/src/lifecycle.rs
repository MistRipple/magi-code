//! Session 生命周期 Observer。
//!
//! 由 magi-api 在启动时安装一份实现，负责把 session 创建/归档/删除事件
//! 转发给 magi-snapshot 等下游订阅方。session-store 自身不持有任何
//! 下游知识，只回调 observer。

use magi_core::SessionId;

pub trait SessionLifecycleObserver: Send + Sync {
    /// 新建 session 时触发。`workspace_id` 为 None 表示未绑定 workspace。
    fn on_session_created(&self, session_id: &SessionId, workspace_id: Option<&str>);
    fn on_session_archived(&self, session_id: &SessionId);
    fn on_session_deleted(&self, session_id: &SessionId);
}
