/**
 * Magi MCP 服务的窗口级事件名。
 *
 * daemon 发布 `tool.approval.requested`（payload.external=true）时，bridge 转成这个事件，
 * 让设置页与全局托盘各自刷新待确认列表；事实仍然只在 daemon，这里不保存任何状态。
 */
export const MCP_APPROVALS_CHANGED_EVENT = 'magi:mcp-approvals-changed';
