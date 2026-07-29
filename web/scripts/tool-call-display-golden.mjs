import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const display = await server.ssrLoadModule('/src/lib/tool-call-display.ts');
  const fileChange = await server.ssrLoadModule('/src/lib/canonical-tool-file-change.ts');
  const terminal = await server.ssrLoadModule('/src/lib/terminal-utils.ts');
  const toolCallFailure = await server.ssrLoadModule('/src/lib/tool-call-failure.ts');

  assert.deepEqual(
    toolCallFailure.parseToolCallFailureDiagnostic({
      schemaVersion: 'tool-call-failure.v1',
      code: 'tool_arguments_invalid',
      summary: '模型连续提交无效的 shell_exec 工具参数；工具未执行，本轮已停止。',
      detail: '缺少必填参数 command',
      stage: 'tool_call_validation',
      toolName: 'shell_exec',
      reasonCode: 'tool_arguments_missing_required',
      missingFields: ['command'],
      argumentsPreview: '{}',
      retryAttempts: 1,
    }),
    {
      schemaVersion: 'tool-call-failure.v1',
      code: 'tool_arguments_invalid',
      summary: '模型连续提交无效的 shell_exec 工具参数；工具未执行，本轮已停止。',
      detail: '缺少必填参数 command',
      stage: 'tool_call_validation',
      toolName: 'shell_exec',
      reasonCode: 'tool_arguments_missing_required',
      missingFields: ['command'],
      argumentsPreview: '{}',
      retryAttempts: 1,
    },
    '重复无效工具调用必须保留真实工具名、缺失字段和参数摘要',
  );

  const { render } = await server.ssrLoadModule('svelte/server');
  const failureCard = await server.ssrLoadModule('/src/components/ModelFailureCard.svelte');
  const invalidToolMarkup = render(failureCard.default, {
    props: {
      failure: toolCallFailure.parseToolCallFailureDiagnostic({
        schemaVersion: 'tool-call-failure.v1',
        code: 'tool_arguments_invalid',
        summary: '模型连续提交无效的 shell_exec 工具参数；工具未执行，本轮已停止。',
        detail: '工具：shell_exec\n直接原因：缺少必填参数 command\n收到的参数：{}',
        stage: 'tool_call_validation',
        toolName: 'shell_exec',
        reasonCode: 'tool_arguments_missing_required',
        missingFields: ['command'],
        argumentsPreview: '{}',
        retryAttempts: 1,
      }),
    },
  }).body;
  assert.match(invalidToolMarkup, /工具调用格式错误/);
  assert.match(invalidToolMarkup, /shell_exec/);
  assert.match(invalidToolMarkup, /缺少必填参数 command/);
  assert.match(invalidToolMarkup, /tool_arguments_invalid/);

  const unavailableToolMarkup = render(failureCard.default, {
    props: {
      failure: toolCallFailure.parseToolCallFailureDiagnostic({
        schemaVersion: 'tool-call-failure.v1',
        code: 'tool_not_available',
        summary: '模型连续调用本轮未提供的 shell_exec 工具；工具未执行，本轮已停止。',
        detail: '工具：shell_exec\n直接原因：当前运行环境未提供工具 shell_exec。',
        stage: 'tool_call_validation',
        toolName: 'shell_exec',
        reasonCode: 'tool_not_available',
        missingFields: [],
        argumentsPreview: '{"command":"pwd"}',
        retryAttempts: 1,
      }),
    },
  }).body;
  assert.match(unavailableToolMarkup, /工具不可用/);
  assert.match(unavailableToolMarkup, /校验工具可用性/);
  assert.match(unavailableToolMarkup, /重新选择可用工具/);
  assert.match(unavailableToolMarkup, /tool_not_available/);
  assert.doesNotMatch(unavailableToolMarkup, /工具调用格式错误/);

  assert.equal(
    terminal.resolveTerminalArgumentId({ command: 'npm test', action: 'run', terminal_id: 0 }),
    undefined,
    '前台 shell_exec 不得把模型占位 terminal_id=0 显示为真实终端会话',
  );
  assert.equal(
    terminal.resolveTerminalArgumentId({ action: 'read', terminal_id: 12 }),
    12,
    '后台终端控制动作必须保留真实 terminal_id',
  );
  assert.equal(
    terminal.terminalPayloadOutput({
      status: 'failed',
      stdout: '',
      stderr: 'npm ERR! Unknown option --runInBand',
    }),
    'npm ERR! Unknown option --runInBand',
    '失败的 shell_exec 必须把 stderr 作为终端输出展示',
  );
  assert.equal(
    terminal.terminalPayloadErrorText({
      status: 'failed',
      error_code: 'shell_exec_working_directory_unavailable',
      error: '当前工作区目录不可访问，请重新选择工作区',
    }),
    '当前工作区目录不可访问，请重新选择工作区',
    '失败的 shell_exec 必须展示结构化公开错误，不能退回通用提示',
  );

  const terminalCard = await server.ssrLoadModule('/src/components/TerminalSessionCard.svelte');
  const failedTerminalMarkup = render(terminalCard.default, {
    props: {
      status: 'running',
      toolCall: {
        id: 'shell-failure',
        name: 'shell_exec',
        arguments: {
          command: 'npm test -- --runInBand',
          action: 'run',
          terminal_id: 0,
        },
        status: 'error',
        error: JSON.stringify({
          status: 'failed',
          error_code: 'shell_exec_failed',
          error: '测试命令参数无效',
          stderr: 'npm ERR! Unknown option --runInBand',
          exit_code: 1,
        }),
      },
    },
  }).body;
  assert.doesNotMatch(failedTerminalMarkup, /终端会话 #0|data-terminal-id="0"/);
  assert.match(failedTerminalMarkup, /npm ERR! Unknown option --runInBand/);
  assert.match(failedTerminalMarkup, /测试命令参数无效/);
  assert.doesNotMatch(failedTerminalMarkup, /占用状态[^<]*否/);

  assert.deepEqual(
    display.coerceToolArgumentsRecord('src/App.svelte'),
    { input: 'src/App.svelte' },
    'raw tool arguments must survive projection instead of becoming an empty object',
  );

  assert.deepEqual(
    display.resolveToolCardTarget({
      toolName: 'file_read',
      input: { file_path: 'src/App.svelte' },
    }),
    { primaryPath: 'src/App.svelte', paths: ['src/App.svelte'] },
    'file_read should display file_path in the card title',
  );

  assert.deepEqual(
    display.resolveToolCardTarget({
      toolName: 'file_patch',
      input: { filePath: 'src/lib/state.ts' },
    }),
    { primaryPath: 'src/lib/state.ts', paths: ['src/lib/state.ts'] },
    'file_patch should display filePath in the card title',
  );

  assert.deepEqual(
    display.resolveToolCardTarget({
      toolName: 'image_generate',
      input: { prompt: 'blue square' },
      output: JSON.stringify({
        status: 'succeeded',
        path: 'generated-images/blue-square.png',
      }),
    }),
    {
      primaryPath: 'generated-images/blue-square.png',
      paths: ['generated-images/blue-square.png'],
    },
    'image_generate should locate its generated workspace file from the tool result',
  );

  assert.deepEqual(
    display.resolveToolCardTarget({
      toolName: 'apply_patch',
      input: {
        patch: [
          '*** Begin Patch',
          '*** Update File: src/App.svelte',
          '@@',
          '-old',
          '+new',
          '*** End Patch',
        ].join('\n'),
      },
    }),
    { primaryPath: 'src/App.svelte', paths: ['src/App.svelte'] },
    'apply_patch should derive the target from patch text before output is available',
  );

  assert.deepEqual(
    display.resolveToolCardTarget({
      toolName: 'apply_patch',
      input: {},
      output: JSON.stringify({
        tool: 'apply_patch',
        status: 'succeeded',
        changed_paths: ['src/App.svelte', 'src/lib/state.ts'],
      }),
    }),
    { paths: ['src/App.svelte', 'src/lib/state.ts'] },
    'apply_patch should display changed_paths from the tool result when multiple files changed',
  );

  const filePatchBlocks = fileChange.buildCanonicalToolFileChangeBlocks({
    blockIdBase: 'call-file-patch',
    sessionId: 'session-a',
    toolName: 'file_patch',
    arguments: {
      path: 'styles.css',
      old_string: 'button,\ninput {',
      new_string: 'button,\ninput,\nselect {',
    },
    result: {
      status: 'succeeded',
      path: 'styles.css',
    },
    status: 'success',
  });
  assert.equal(filePatchBlocks.length, 1, 'file_patch success should project as one file_change block');
  assert.equal(filePatchBlocks[0].type, 'file_change', 'file_patch should not render as raw JSON ToolCall');
  assert.equal(filePatchBlocks[0].fileChange.filePath, 'styles.css');
  assert.equal(filePatchBlocks[0].fileChange.changeType, 'modify');
  assert.match(filePatchBlocks[0].fileChange.diff, /-button,/);
  assert.match(filePatchBlocks[0].fileChange.diff, /\+select \{/);

  const applyPatchBlocks = fileChange.buildCanonicalToolFileChangeBlocks({
    blockIdBase: 'call-apply-patch',
    sessionId: 'session-a',
    toolName: 'apply_patch',
    arguments: {
      patch: [
        '*** Begin Patch',
        '*** Update File: src/App.svelte',
        '@@',
        '-old',
        '+new',
        '*** End Patch',
      ].join('\n'),
    },
    result: { status: 'succeeded' },
    status: 'success',
  });
  assert.equal(applyPatchBlocks.length, 1, 'apply_patch success should project patch text as file_change');
  assert.equal(applyPatchBlocks[0].fileChange.filePath, 'src/App.svelte');
  assert.match(applyPatchBlocks[0].fileChange.diff, /-old/);
  assert.match(applyPatchBlocks[0].fileChange.diff, /\+new/);

  console.log('tool call display golden replay passed');
}, { configFile: './vite.web.config.ts' });
