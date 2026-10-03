//! Magi MCP 服务核心：标准 MCP 协议、令牌、权限档、工具目录、路径守卫与调用管线。
//!
//! 本 crate **不依赖**工具运行时、审批注册表与会话模型：这些由宿主通过 trait 注入
//! （见 [`server::ToolBackend`]、[`server::AttributionResolver`]、[`server::WorkspaceResolver`]），
//! 使它能独立开发与测试，并让 GPT Web 只是它的一个客户端。设计见 `docs/magi-mcp-server-design.md`。

pub mod catalog;
pub mod http;
pub mod local_socket;
pub mod path_guard;
pub mod profile;
pub mod protocol;
pub mod rate_limit;
pub mod relay_cli;
pub mod server;
pub mod token;

pub use catalog::{
    DynamicTool, ResolvedTool, ToolDescriptor, ToolMapping, ToolSchema, ToolSchemaProvider,
    V1_TOOLS, build_catalog, mapping_for_public_name,
};
pub use path_guard::{PathAccess, PathGuard, PathRequest, PathViolation};
pub use profile::{Disposition, Profile, ToolClass};
pub use protocol::{JsonRpcRequest, MCP_PROTOCOL_VERSION};
pub use server::{
    AttributionRefusal, AttributionResolver, AttributionTarget, AuditEvent, AuditOutcome,
    AuditSink, ClientPrincipal, ExternalOnlyAttribution, InvocationOutcome, McpServer, ToolBackend,
    ToolInvocation, WorkspaceResolver,
};
pub use token::{
    AttributionMode, AuthError, IssueTokenRequest, IssuedToken, TokenRecord, TokenStore,
};
