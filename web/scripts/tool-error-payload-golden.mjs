import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const toolErrors = await server.ssrLoadModule('/src/lib/tool-error-payload.ts');

  assert.equal(
    toolErrors.isStructuredToolErrorPayload(JSON.stringify({
      status: 'failed',
      error_code: 'mcp_tool_failed',
      error: '/Users/xie/.magi/mcp/server stderr: ENOENT',
    })),
    true,
    'failed payload with error_code should be treated as structured error',
  );

  assert.equal(
    toolErrors.isStructuredToolErrorPayload({
      status: 'rejected',
      errorCode: 'tool_policy_rejected',
      message: 'blocked',
    }),
    true,
    'rejected payload with errorCode should be treated as structured error',
  );

  assert.equal(
    toolErrors.isStructuredToolErrorPayload(JSON.stringify({
      status: 'succeeded',
      error_code: 'diagnostic_code_in_success_payload',
      content: 'normal output',
    })),
    false,
    'successful payload must remain displayable output',
  );

  assert.equal(
    toolErrors.isStructuredToolErrorPayload('plain tool output'),
    false,
    'plain output must remain displayable output',
  );

  assert.equal(
    toolErrors.toolPayloadErrorCode(JSON.stringify({ error_code: 'MCP_TOOL_FAILED' })),
    'mcp_tool_failed',
    'error_code normalization must stay stable',
  );

  const agentApproval = toolErrors.parseToolApprovalPayload(JSON.stringify({
    tool: 'shell_exec',
    status: 'awaiting_approval',
    approval_id: 'approval-agent',
    approval: {
      approvalId: 'approval-agent',
      sessionId: 'session-1',
      taskId: 'task-child',
      reason: '需要执行命令',
      agent: { role: 'executor', title: '构建修复代理' },
    },
  }));
  assert.deepEqual(
    agentApproval?.agent,
    { role: 'executor', title: '构建修复代理' },
    'sub-agent approval must keep the requesting agent identity',
  );
  const mainlineApproval = toolErrors.parseToolApprovalPayload(JSON.stringify({
    status: 'awaiting_approval',
    approval: { approvalId: 'approval-main', sessionId: 'session-1' },
  }));
  assert.equal(mainlineApproval?.agent, null, 'mainline approval has no agent identity');

  console.log('tool error payload golden replay passed');
});
