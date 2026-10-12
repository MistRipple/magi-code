/**
 * Magi MCP 服务的窗口级事件名。
 *
 * daemon 发布 `tool.approval.requested`（payload.external=true）时，bridge 转成这个事件，
 * 让设置页与全局托盘各自刷新待确认列表；事实仍然只在 daemon，这里不保存任何状态。
 */
export const MCP_APPROVALS_CHANGED_EVENT = 'magi:mcp-approvals-changed';

/**
 * 外部会话变化的窗口级事件：daemon 发布 `mcp.activity`（某个外部会话有新调用或调用结束）时，
 * bridge 转成这个事件，侧栏据此重新读取外部会话概要。调用内容本身走会话的 canonical 事件。
 */
export const MCP_ACTIVITY_EVENT = 'magi:mcp-activity';
