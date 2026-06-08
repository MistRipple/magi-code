import assert from 'node:assert/strict';
import { withGoldenViteServer } from './golden-vite.mjs';

await withGoldenViteServer(async (server) => {
  const display = await server.ssrLoadModule('/src/lib/tool-call-display.ts');
  const fileChange = await server.ssrLoadModule('/src/lib/canonical-tool-file-change.ts');

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
});
