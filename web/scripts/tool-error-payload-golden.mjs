import assert from 'node:assert/strict';
import { createServer } from 'vite';

const server = await createServer({
  root: process.cwd(),
  configFile: false,
  logLevel: 'silent',
  server: { middlewareMode: true },
});

try {
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

  console.log('tool error payload golden replay passed');
} finally {
  await server.close();
}
